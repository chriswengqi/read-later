//! Turning whatever the share sheet hands us into one canonical URL.
//!
//! Canonical URLs matter because the database has `UNIQUE(url)`: saving the same YouTube video
//! from the app (`youtu.be/abc?si=...`) and from the browser (`www.youtube.com/watch?v=abc`)
//! should hit the same row instead of creating a duplicate.
//!
//! Everything here is pure (no I/O), so it's covered by plain `cargo test`.

use url::Url;

use crate::links::LinkKind;

/// Query parameters that only exist for tracking and never change what the page shows.
const TRACKING_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "igshid", "mc_cid", "mc_eid", "ref_src", "ref_url", "si",
    "feature", "_hsenc", "_hsmi", "mkt_tok",
];

/// Parameters that are tracking-only on specific sites (elsewhere they can mean something).
/// e.g. on x.com `?s=20&t=abc` is share tracking; on YouTube `t=` is the start timestamp.
const X_TRACKING_PARAMS: &[&str] = &["s", "t"];

const VIDEO_HOSTS: &[&str] = &[
    "youtube.com", "youtu.be", "vimeo.com", "tiktok.com", "twitch.tv", "dailymotion.com",
    "bilibili.com", "loom.com",
];

const POST_HOSTS: &[&str] = &[
    "x.com", "twitter.com", "threads.net", "threads.com", "bsky.app", "reddit.com",
    "instagram.com", "news.ycombinator.com", "linkedin.com", "mastodon.social",
];

/// Extract the first http(s) URL from `input` and canonicalize it.
///
/// Returns `None` when there's no usable URL. Share sheets sometimes send
/// `"Some title https://example.com/post"` instead of a bare link, so we search for one.
pub fn normalize(input: &str) -> Option<Url> {
    let start = input.find("https://").or_else(|| input.find("http://"))?;
    let candidate = input[start..].split_whitespace().next()?;
    let mut url = Url::parse(candidate).ok()?;

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }

    rewrite_host_aliases(&mut url);
    strip_tracking(&mut url);
    Some(url)
}

/// Is `host` equal to `domain` or a subdomain of it? (`m.youtube.com` matches `youtube.com`.)
fn host_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn host_in(url: &Url, domains: &[&str]) -> bool {
    url.host_str()
        .is_some_and(|host| domains.iter().any(|d| host_matches(host, d)))
}

/// Collapse the different hostnames a site uses into one.
fn rewrite_host_aliases(url: &mut Url) {
    let Some(host) = url.host_str().map(str::to_owned) else {
        return;
    };

    match host.as_str() {
        // youtu.be/<id>?t=30  →  www.youtube.com/watch?v=<id>&t=30
        "youtu.be" => {
            let id = url.path().trim_start_matches('/').to_owned();
            if id.is_empty() {
                return;
            }
            let rest: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            let _ = url.set_host(Some("www.youtube.com"));
            url.set_path("/watch");
            let mut query = url.query_pairs_mut();
            query.clear().append_pair("v", &id);
            for (k, v) in &rest {
                query.append_pair(k, v);
            }
        }
        "youtube.com" | "m.youtube.com" | "music.youtube.com" => {
            let _ = url.set_host(Some("www.youtube.com"));
        }
        "twitter.com" | "www.twitter.com" | "mobile.twitter.com" | "www.x.com" | "mobile.x.com" => {
            let _ = url.set_host(Some("x.com"));
        }
        "old.reddit.com" | "new.reddit.com" | "reddit.com" | "m.reddit.com" => {
            let _ = url.set_host(Some("www.reddit.com"));
        }
        _ => {}
    }
}

fn strip_tracking(url: &mut Url) {
    let on_x = host_in(url, &["x.com"]);
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.as_ref();
            !(key.starts_with("utm_")
                || TRACKING_PARAMS.contains(&key)
                || (on_x && X_TRACKING_PARAMS.contains(&key)))
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(&kept);
    }
}

/// Guess what kind of thing a URL points to, from its host.
pub fn kind_of(url: &Url) -> LinkKind {
    if host_in(url, VIDEO_HOSTS) {
        LinkKind::Video
    } else if host_in(url, POST_HOSTS) {
        LinkKind::Post
    } else {
        LinkKind::Article
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        normalize(s).expect("should parse").to_string()
    }

    #[test]
    fn extracts_url_from_shared_text() {
        assert_eq!(
            norm("Check this out https://example.com/post?id=1 via @someone"),
            "https://example.com/post?id=1"
        );
    }

    #[test]
    fn rejects_non_urls() {
        assert!(normalize("just some text").is_none());
        assert!(normalize("ftp://example.com/file").is_none());
        assert!(normalize("").is_none());
    }

    #[test]
    fn strips_tracking_params_but_keeps_real_ones() {
        assert_eq!(
            norm("https://blog.example.com/a?utm_source=x&utm_medium=y&page=2&fbclid=abc"),
            "https://blog.example.com/a?page=2"
        );
        assert_eq!(norm("https://example.com/a?utm_source=x"), "https://example.com/a");
    }

    #[test]
    fn youtube_short_links_become_canonical() {
        assert_eq!(
            norm("https://youtu.be/dQw4w9WgXcQ?si=TRACKING&t=42"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42"
        );
        assert_eq!(
            norm("https://m.youtube.com/watch?v=dQw4w9WgXcQ&feature=shared"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn x_links_are_unified_and_share_tracking_removed() {
        assert_eq!(
            norm("https://twitter.com/rustlang/status/123?s=46&t=abcdef"),
            "https://x.com/rustlang/status/123"
        );
    }

    #[test]
    fn t_param_is_only_stripped_on_x() {
        // On YouTube, t= is the start time and must survive.
        assert!(norm("https://www.youtube.com/watch?v=abc&t=90").contains("t=90"));
    }

    #[test]
    fn classifies_by_host() {
        let kind = |s: &str| kind_of(&normalize(s).unwrap());
        assert_eq!(kind("https://youtu.be/abc"), LinkKind::Video);
        assert_eq!(kind("https://vimeo.com/123"), LinkKind::Video);
        assert_eq!(kind("https://x.com/a/status/1"), LinkKind::Post);
        assert_eq!(kind("https://old.reddit.com/r/rust"), LinkKind::Post);
        assert_eq!(kind("https://blog.rust-lang.org/2024/01/01/post.html"), LinkKind::Article);
        // Lookalike domains must not match.
        assert_eq!(kind("https://notyoutube.com/watch"), LinkKind::Article);
    }
}
