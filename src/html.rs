//! A tiny, forgiving scanner that pulls the title / preview image / site name out of an HTML
//! page's `<head>`.
//!
//! A full HTML parser crate would bloat the `.wasm` (Workers have a size limit), and we only need
//! `<title>` plus a handful of `<meta>` tags, which are reliably present because every site wants
//! nice link previews on social media. So we walk the tags by hand and stop at `<body>`.

/// What we could find in a page's `<head>`. Every field is optional because real pages are messy.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PageMeta {
    pub title: Option<String>,
    pub image: Option<String>,
    pub site_name: Option<String>,
    /// Target of a `<meta http-equiv="refresh" content="0; url=...">` redirect, if any.
    pub refresh_url: Option<String>,
}

/// Candidates collected while scanning; resolved by priority at the end.
#[derive(Default)]
struct Candidates {
    og_title: Option<String>,
    twitter_title: Option<String>,
    title_tag: Option<String>,
    og_image: Option<String>,
    twitter_image: Option<String>,
    og_site_name: Option<String>,
    application_name: Option<String>,
    refresh_url: Option<String>,
}

pub fn parse_head(html: &str) -> PageMeta {
    let mut found = Candidates::default();
    let mut rest = html;

    while let Some(lt) = rest.find('<') {
        rest = &rest[lt + 1..];

        // Comments and <script>/<style> bodies can contain anything, including fake tags.
        if rest.starts_with("!--") {
            rest = skip_past(rest, b"-->");
            continue;
        }

        let name_len = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        // `/head` has its slash first, so handle closing tags explicitly.
        let name = if let Some(after_slash) = rest.strip_prefix('/') {
            let len = after_slash
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .map_or(rest.len(), |i| i + 1);
            &rest[..len]
        } else {
            &rest[..name_len]
        };

        if name.eq_ignore_ascii_case("meta") {
            let end = tag_end(rest);
            record_meta(&mut found, &parse_attrs(&rest[name_len..end]));
            rest = &rest[end..];
        } else if name.eq_ignore_ascii_case("title") {
            let Some(gt) = rest.find('>') else { break };
            let body = &rest[gt + 1..];
            let close = find_ignore_case(body, b"</title").unwrap_or(body.len());
            if found.title_tag.is_none() {
                found.title_tag = non_empty(decode_entities(&body[..close]));
            }
            rest = &body[close..];
        } else if name.eq_ignore_ascii_case("script") {
            rest = skip_past(rest, b"</script");
        } else if name.eq_ignore_ascii_case("style") {
            rest = skip_past(rest, b"</style");
        } else if name.eq_ignore_ascii_case("/head") || name.eq_ignore_ascii_case("body") {
            break;
        }
    }

    PageMeta {
        title: found.og_title.or(found.twitter_title).or(found.title_tag),
        image: found.og_image.or(found.twitter_image),
        site_name: found.og_site_name.or(found.application_name),
        refresh_url: found.refresh_url,
    }
}

fn record_meta(found: &mut Candidates, attrs: &[(String, String)]) {
    let attr = |key: &str| {
        attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    if attr("http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("refresh")) {
        if found.refresh_url.is_none() {
            found.refresh_url = attr("content").and_then(refresh_target);
        }
        return;
    }

    // Sites disagree on whether OpenGraph uses `property=` or `name=`; accept both.
    let Some(key) = attr("property").or_else(|| attr("name")) else {
        return;
    };
    let Some(content) = attr("content").map(decode_entities).and_then(non_empty) else {
        return;
    };

    let slot = match key.to_ascii_lowercase().as_str() {
        "og:title" => &mut found.og_title,
        "twitter:title" => &mut found.twitter_title,
        "og:image" | "og:image:url" | "og:image:secure_url" => &mut found.og_image,
        "twitter:image" | "twitter:image:src" => &mut found.twitter_image,
        "og:site_name" => &mut found.og_site_name,
        "application-name" => &mut found.application_name,
        _ => return,
    };
    // First occurrence wins (pages sometimes list several og:image tags; the first is the main one).
    slot.get_or_insert(content);
}

/// `"0; url=https://example.com/new"` → `"https://example.com/new"`.
fn refresh_target(content: &str) -> Option<String> {
    let at = find_ignore_case(content, b"url=")?;
    let target = content[at + 4..].trim().trim_matches(['\'', '"']);
    non_empty(decode_entities(target))
}

/// Byte index of the `>` that closes the tag starting at `s`, ignoring `>` inside quotes.
fn tag_end(s: &str) -> usize {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return i,
            _ => {}
        }
    }
    s.len()
}

/// Parse `name="value" other='x' bare=y flag` into lowercase-keyed pairs.
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut rest = s;

    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        if rest.is_empty() {
            break;
        }
        let key_len = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = rest[..key_len].to_ascii_lowercase();
        rest = rest[key_len..].trim_start();

        let value = if let Some(after_eq) = rest.strip_prefix('=') {
            let after_eq = after_eq.trim_start();
            match after_eq.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let inner = &after_eq[1..];
                    let close = inner.find(q).unwrap_or(inner.len());
                    rest = inner.get(close + 1..).unwrap_or("");
                    &inner[..close]
                }
                _ => {
                    let end = after_eq
                        .find(|c: char| c.is_ascii_whitespace())
                        .unwrap_or(after_eq.len());
                    rest = &after_eq[end..];
                    &after_eq[..end]
                }
            }
        } else {
            ""
        };
        attrs.push((key, value.to_owned()));
    }
    attrs
}

/// Case-insensitive search for an ASCII needle. Returns a byte index into `haystack`.
fn find_ignore_case(haystack: &str, needle: &[u8]) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Everything after the first occurrence of `needle`, or `""` if it never appears.
fn skip_past<'a>(s: &'a str, needle: &[u8]) -> &'a str {
    // Safe to slice: the needle is ASCII, so a match always lands on a char boundary.
    find_ignore_case(s, needle).map_or("", |i| &s[i + needle.len()..])
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Decode the HTML entities that show up in titles, and collapse whitespace.
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;

    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // Entities are short; if there's no `;` soon after, it's just a literal `&`.
        let decoded = rest[1..]
            .find(';')
            .filter(|&semi| semi <= 10)
            .and_then(|semi| Some((decode_entity(&rest[1..=semi])?, semi + 2)));
        match decoded {
            Some((ch, consumed)) => {
                out.push(ch);
                rest = &rest[consumed..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "rsquo" => Some('’'),
        "lsquo" => Some('‘'),
        "rdquo" => Some('”'),
        "ldquo" => Some('“'),
        _ => {
            let code = name.strip_prefix('#')?;
            let n = match code.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => code.parse().ok()?,
            };
            char::from_u32(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_opengraph_over_title_tag() {
        let html = r#"<html><head>
            <title>Fallback Title | Site</title>
            <meta property="og:title" content="The Real Title">
            <meta property="og:image" content="https://cdn.example.com/a.jpg" />
            <meta property="og:site_name" content="Example Blog">
            </head><body><meta property="og:title" content="ignored"></body></html>"#;
        assert_eq!(
            parse_head(html),
            PageMeta {
                title: Some("The Real Title".into()),
                image: Some("https://cdn.example.com/a.jpg".into()),
                site_name: Some("Example Blog".into()),
                refresh_url: None,
            }
        );
    }

    #[test]
    fn falls_back_to_title_tag_and_twitter_image() {
        let html = r#"<HEAD><TITLE>  Hello
            World  </TITLE><meta name='twitter:image' content='/img.png'></HEAD>"#;
        let meta = parse_head(html);
        assert_eq!(meta.title.as_deref(), Some("Hello World"));
        assert_eq!(meta.image.as_deref(), Some("/img.png"));
    }

    #[test]
    fn handles_attribute_order_and_gt_inside_quotes() {
        let html = r#"<meta content="a > b" property="og:title">"#;
        assert_eq!(parse_head(html).title.as_deref(), Some("a > b"));
    }

    #[test]
    fn ignores_tags_inside_scripts_and_comments() {
        let html = r#"<head>
            <script>var s = '<meta property="og:title" content="nope">';</script>
            <!-- <meta property="og:title" content="nope either"> -->
            <meta property="og:title" content="yes">
            </head>"#;
        assert_eq!(parse_head(html).title.as_deref(), Some("yes"));
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_entities("Tom &amp; Jerry&#39;s &#x1F980; &lt;3"), "Tom & Jerry's 🦀 <3");
        assert_eq!(decode_entities("AT&T rocks"), "AT&T rocks");
        assert_eq!(decode_entities("&bogus; stays"), "&bogus; stays");
    }

    #[test]
    fn survives_garbage() {
        assert_eq!(parse_head(""), PageMeta::default());
        assert_eq!(parse_head("<<<>>><meta"), PageMeta::default());
        assert_eq!(parse_head("<title>unterminated"), PageMeta {
            title: Some("unterminated".into()),
            ..Default::default()
        });
    }

    #[test]
    fn finds_meta_refresh_redirects() {
        let html = r#"<title>Redirect</title><noscript>
            <meta http-equiv="Refresh" content="0; URL='https://example.com/new/'"></noscript>"#;
        assert_eq!(parse_head(html).refresh_url.as_deref(), Some("https://example.com/new/"));
    }
}
