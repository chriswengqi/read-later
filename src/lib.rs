//! Read Later: a personal "save this link for later" service on Cloudflare Workers.
//!
//! Static files in `public/` (the web app) are served by Cloudflare before this code runs;
//! everything else lands in `fetch` below.
//!
//! API (all require `Authorization: Bearer <AUTH_TOKEN>`):
//!   GET    /api/links?status=unread|read   list links
//!   POST   /api/links        {"url": ...}  save a link (also accepts shared text containing a URL)
//!   GET    /api/links?status=favorites      list favourites (read or not)
//!   PATCH  /api/links/:id    {"read"?: bool, "favorite"?: bool}  update either or both
//!   DELETE /api/links/:id                  delete
//!   GET    /save?url=...&token=...         save from the desktop bookmarklet (returns a tiny page)

mod error;
mod html;
mod links;
mod metadata;
mod url_clean;

use serde::Deserialize;
use worker::{Context, D1Database, Date, Env, Method, Request, Response, event};

use crate::error::{AppError, AppResult};
use crate::links::{Link, Status};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> worker::Result<Response> {
    match route(req, &env, &ctx).await {
        Ok(response) => Ok(response),
        Err(err) => err.into_response(),
    }
}

async fn route(mut req: Request, env: &Env, ctx: &Context) -> AppResult<Response> {
    let url = req.url()?;
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|seg| !seg.is_empty()).collect())
        .unwrap_or_default();
    let query = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };

    // The bookmarklet opens a plain browser window, which can't send headers, so it carries
    // the token in the query string instead.
    if let (Method::Get, ["save"]) = (req.method(), segments.as_slice()) {
        authorize(env, query("token").as_deref())?;
        let link = save(env, ctx, &query("url").unwrap_or_default()).await?;
        return Ok(Response::from_html(saved_page(&link))?);
    }

    authorize(env, bearer_token(&req).as_deref())?;
    let db = env.d1("DB")?;

    match (req.method(), segments.as_slice()) {
        (Method::Get, ["api", "links"]) => {
            let status = match query("status") {
                None => Status::Unread,
                Some(s) => Status::parse(&s)
                    .ok_or_else(|| AppError::BadRequest("status must be unread, read or favorites".into()))?,
            };
            Ok(Response::from_json(&links::list(&db, status).await?)?)
        }

        (Method::Post, ["api", "links"]) => {
            #[derive(Deserialize)]
            struct Body {
                url: String,
            }
            let body: Body = req
                .json()
                .await
                .map_err(|_| AppError::BadRequest(r#"expected JSON {"url": "..."}"#.into()))?;
            let link = save(env, ctx, &body.url).await?;
            Ok(Response::from_json(&link)?.with_status(201))
        }

        (Method::Patch, ["api", "links", id]) => {
            // Each field is optional: `None` means "leave it as it is".
            #[derive(Deserialize)]
            struct Body {
                read: Option<bool>,
                favorite: Option<bool>,
            }
            let id = parse_id(id)?;
            let bad_body =
                || AppError::BadRequest(r#"expected JSON {"read"?: bool, "favorite"?: bool}"#.into());
            let body: Body = req.json().await.map_err(|_| bad_body())?;
            if body.read.is_none() && body.favorite.is_none() {
                return Err(bad_body());
            }

            let mut found = true;
            if let Some(read) = body.read {
                found &= links::set_read(&db, id, read.then(now_millis)).await?;
            }
            if let Some(favorite) = body.favorite {
                found &= links::set_favorite(&db, id, favorite).await?;
            }
            if found {
                Ok(Response::empty()?.with_status(204))
            } else {
                Err(AppError::NotFound)
            }
        }

        (Method::Delete, ["api", "links", id]) => {
            if links::delete(&db, parse_id(id)?).await? {
                Ok(Response::empty()?.with_status(204))
            } else {
                Err(AppError::NotFound)
            }
        }

        _ => Err(AppError::NotFound),
    }
}

/// Save a link and kick off the title/thumbnail lookup in the background.
async fn save(env: &Env, ctx: &Context, input: &str) -> AppResult<Link> {
    let url = url_clean::normalize(input)
        .ok_or_else(|| AppError::BadRequest("no http(s) URL found".into()))?;
    let db = env.d1("DB")?;
    let link = links::upsert(&db, url.as_str(), url_clean::kind_of(&url), now_millis()).await?;

    if link.title.is_none() {
        // `wait_until` lets the Worker keep running after the response is sent, so saving feels
        // instant. The future must be `'static`, so it takes ownership of everything it uses.
        let id = link.id;
        ctx.wait_until(async move {
            fill_metadata(db, id, url).await;
        });
    }
    Ok(link)
}

async fn fill_metadata(db: D1Database, id: i64, url: url::Url) {
    let meta = metadata::fetch(&url).await;
    if let Err(err) = links::set_metadata(&db, id, &meta).await {
        worker::console_error!("saving metadata for link {id} failed: {err:?}");
    }
}

fn authorize(env: &Env, provided: Option<&str>) -> AppResult<()> {
    // Set with `wrangler secret put AUTH_TOKEN` in production, or `.dev.vars` locally.
    let expected = env
        .secret("AUTH_TOKEN")
        .map(|s| s.to_string())
        .map_err(|_| {
            worker::console_error!("AUTH_TOKEN is not configured");
            AppError::Unauthorized
        })?;

    match provided {
        Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => Ok(()),
        _ => Err(AppError::Unauthorized),
    }
}

fn bearer_token(req: &Request) -> Option<String> {
    let header = req.headers().get("Authorization").ok().flatten()?;
    header.strip_prefix("Bearer ").map(str::to_owned)
}

/// Compare secrets without returning early at the first mismatch, so response timing
/// doesn't reveal how many leading characters of a guess were right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn parse_id(s: &str) -> AppResult<i64> {
    s.parse()
        .map_err(|_| AppError::BadRequest(format!("invalid id {s:?}")))
}

fn now_millis() -> i64 {
    // Milliseconds since 1970 fit comfortably in an i64 for the next few hundred million years.
    Date::now().as_millis() as i64
}

/// The little confirmation window the desktop bookmarklet opens. Closes itself.
fn saved_page(link: &Link) -> String {
    let escaped = link
        .url
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Saved</title>
<style>body{{font:15px -apple-system,system-ui,sans-serif;display:grid;place-items:center;height:90vh;margin:0;
background:#f6f5f2;color:#1b1b1b}}@media(prefers-color-scheme:dark){{body{{background:#151515;color:#ededed}}}}
p{{margin:4px;text-align:center;word-break:break-all;max-width:320px}}small{{opacity:.6}}</style>
<div><p>✅ <strong>Saved for later</strong></p><p><small>{escaped}</small></p></div>
<script>setTimeout(() => window.close(), 1200)</script>"#
    )
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret!"));
        assert!(!constant_time_eq(b"", b"x"));
    }
}
