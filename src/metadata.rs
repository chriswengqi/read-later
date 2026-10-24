//! Fetch a title / site name / thumbnail for a saved link.
//!
//! This runs *after* the save request has already been answered (see `ctx.wait_until` in
//! `lib.rs`), so it never slows down saving. It is also best-effort: any failure just means the
//! link shows up with its URL instead of a pretty title.

use std::pin::pin;
use std::time::Duration;

use futures_util::future::{Either, select};
use serde::Deserialize;
use url::Url;
use worker::{Delay, Fetch, Headers, Request, RequestInit};

use crate::html;

#[derive(Debug, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub site: Option<String>,
    pub thumbnail: Option<String>,
}

impl Metadata {
    fn is_empty(&self) -> bool {
        self.title.is_none() && self.thumbnail.is_none()
    }
}

const TIMEOUT: Duration = Duration::from_secs(8);
/// Only the `<head>` matters, and it's near the top. Cap how much HTML we scan.
const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_TITLE_CHARS: usize = 200;
const USER_AGENT: &str = "Mozilla/5.0 (compatible; ReadLater/1.0; personal link previews)";

pub async fn fetch(url: &Url) -> Metadata {
    let host = url.host_str().unwrap_or_default();

    // Sites with an official oEmbed API give much better results than scraping their HTML
    // (X in particular serves almost nothing to non-browsers).
    let specialized = match host {
        "www.youtube.com" => youtube(url).await,
        "x.com" if url.path().contains("/status/") => x_post(url).await,
        _ => Err(worker::Error::RustError("no specialized fetcher".into())),
    };

    let meta = match specialized {
        Ok(meta) if !meta.is_empty() => meta,
        _ => generic(url).await.unwrap_or_else(|err| {
            worker::console_warn!("metadata fetch failed for {url}: {err}");
            Metadata::default()
        }),
    };

    Metadata {
        title: meta.title.map(|t| truncate_chars(&t, MAX_TITLE_CHARS)),
        ..meta
    }
}

#[derive(Deserialize)]
struct OEmbed {
    title: Option<String>,
    author_name: Option<String>,
    thumbnail_url: Option<String>,
    /// X returns the post's text as an HTML snippet here.
    html: Option<String>,
}

async fn youtube(url: &Url) -> worker::Result<Metadata> {
    let endpoint = Url::parse_with_params(
        "https://www.youtube.com/oembed",
        &[("format", "json"), ("url", url.as_str())],
    )?;
    let oembed: OEmbed = serde_json::from_str(&get_text(&endpoint, "application/json").await?)?;
    Ok(Metadata {
        title: oembed.title,
        site: Some(match oembed.author_name {
            Some(author) => format!("YouTube · {author}"),
            None => "YouTube".into(),
        }),
        thumbnail: oembed.thumbnail_url,
    })
}

async fn x_post(url: &Url) -> worker::Result<Metadata> {
    let endpoint = Url::parse_with_params(
        "https://publish.twitter.com/oembed",
        &[("omit_script", "1"), ("dnt", "1"), ("url", url.as_str())],
    )?;
    let oembed: OEmbed = serde_json::from_str(&get_text(&endpoint, "application/json").await?)?;
    Ok(Metadata {
        title: oembed.html.as_deref().and_then(post_text_from_embed),
        site: Some(match oembed.author_name {
            Some(author) => format!("X · {author}"),
            None => "X".into(),
        }),
        thumbnail: None,
    })
}

/// The embed HTML looks like `<blockquote><p lang="en">The post text<br>more</p>— Name ...`.
/// Grab what's inside the first `<p>` and strip any tags in it.
fn post_text_from_embed(embed: &str) -> Option<String> {
    let start = embed.find("<p")?;
    let after_open = &embed[start..];
    let inner = &after_open[after_open.find('>')? + 1..];
    let inner = &inner[..inner.find("</p>").unwrap_or(inner.len())];

    let mut text = String::with_capacity(inner.len());
    let mut in_tag = false;
    for c in inner.chars() {
        match c {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = html::decode_entities(&text);
    (!text.is_empty()).then_some(text)
}

async fn generic(url: &Url) -> worker::Result<Metadata> {
    // HTTP redirects are followed by `fetch` itself, but some static sites redirect with
    // `<meta http-equiv="refresh">` instead. Follow those too, a couple of hops at most.
    const MAX_HOPS: usize = 2;

    let mut page_url = url.clone();
    let mut page = fetch_head(&page_url).await?;
    for _ in 0..MAX_HOPS {
        let Some(next) = page.refresh_url.as_deref().and_then(|t| page_url.join(t).ok()) else {
            break;
        };
        page = fetch_head(&next).await?;
        page_url = next;
    }

    Ok(Metadata {
        title: page.title,
        site: page.site_name,
        // og:image is sometimes relative ("/img/cover.png"); resolve it against the page URL.
        thumbnail: page
            .image
            .and_then(|src| page_url.join(&src).ok())
            .filter(|u| u.scheme() == "https")
            .map(String::from),
    })
}

async fn fetch_head(url: &Url) -> worker::Result<html::PageMeta> {
    let mut body = get_text(url, "text/html,application/xhtml+xml").await?;
    if body.len() > MAX_HTML_BYTES {
        // Truncating a String must land on a char boundary or it panics.
        let mut cut = MAX_HTML_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }
    Ok(html::parse_head(&body))
}

/// GET a URL and return the body as text, giving up after `TIMEOUT`.
async fn get_text(url: &Url, accept: &str) -> worker::Result<String> {
    let headers = Headers::new();
    headers.set("User-Agent", USER_AGENT)?;
    headers.set("Accept", accept)?;
    let mut init = RequestInit::new();
    init.with_headers(headers);
    let request = Request::new_with_init(url.as_str(), &init)?;

    let download = async {
        let mut response = Fetch::Request(request).send().await?;
        match response.status_code() {
            200..=299 => response.text().await,
            status => Err(worker::Error::RustError(format!("HTTP {status}"))),
        }
    };

    // Race the download against a timer; whichever finishes first wins.
    match select(pin!(download), pin!(Delay::from(TIMEOUT))).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(worker::Error::RustError("timed out".into())),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", s[..cut].trim_end()),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_post_text_from_x_embed() {
        let embed = r##"<blockquote class="twitter-tweet"><p lang="en" dir="ltr">Rust 2024 is out!<br><br>Read more <a href="https://t.co/x">t.co/x</a> &amp; enjoy</p>&mdash; Rust (@rustlang) <a href="#">Feb 20, 2025</a></blockquote>"##;
        assert_eq!(
            post_text_from_embed(embed).as_deref(),
            Some("Rust 2024 is out! Read more t.co/x & enjoy")
        );
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate_chars("héllo wörld", 5), "héllo…");
        assert_eq!(truncate_chars("short", 10), "short");
    }
}
