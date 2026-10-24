//! The `Link` model and every SQL query that touches the `links` table.
//!
//! Keeping all SQL in one module means the handlers never see raw queries, and if the schema
//! changes there's exactly one file to update (plus a new migration).

use serde::{Deserialize, Serialize};
use worker::wasm_bindgen::JsValue;
use worker::D1Database;

use crate::error::AppResult;
use crate::metadata::Metadata;

/// What kind of content a link points to. Stored in D1 as lowercase text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Article,
    Video,
    Post,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Article => "article",
            LinkKind::Video => "video",
            LinkKind::Post => "post",
        }
    }
}

/// One row of the `links` table. Field names match the column names, so serde can
/// deserialize D1 rows straight into this struct and serialize it straight to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub id: i64,
    pub url: String,
    pub title: Option<String>,
    pub site: Option<String>,
    pub thumbnail: Option<String>,
    pub kind: LinkKind,
    /// Unix milliseconds.
    pub saved_at: i64,
    /// `None` while unread.
    pub read_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unread,
    Read,
}

impl Status {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unread" => Some(Status::Unread),
            "read" => Some(Status::Read),
            _ => None,
        }
    }
}

// D1's typed integer binding is `i32`, too small for millisecond timestamps, so numbers are
// bound as JS numbers (`f64`), which represent integers exactly up to 2^53.
fn num(n: i64) -> JsValue {
    JsValue::from_f64(n as f64)
}

fn opt_text(s: Option<&str>) -> JsValue {
    s.map_or(JsValue::NULL, JsValue::from_str)
}

const MAX_LIST: u32 = 500;

pub async fn list(db: &D1Database, status: Status) -> AppResult<Vec<Link>> {
    let sql = match status {
        Status::Unread => {
            "SELECT * FROM links WHERE read_at IS NULL ORDER BY saved_at DESC LIMIT ?1"
        }
        Status::Read => {
            "SELECT * FROM links WHERE read_at IS NOT NULL ORDER BY read_at DESC LIMIT ?1"
        }
    };
    let rows = db
        .prepare(sql)
        .bind(&[MAX_LIST.into()])?
        .all()
        .await?
        .results::<Link>()?;
    Ok(rows)
}

/// Insert a new link, or, if the URL is already saved, bump it to the top of the unread list.
pub async fn upsert(db: &D1Database, url: &str, kind: LinkKind, now: i64) -> AppResult<Link> {
    let link = db
        .prepare(
            "INSERT INTO links (url, kind, saved_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (url) DO UPDATE SET saved_at = excluded.saved_at, read_at = NULL
             RETURNING *",
        )
        .bind(&[url.into(), kind.as_str().into(), num(now)])?
        .first::<Link>(None)
        .await?;
    // RETURNING always yields the row on success; treat absence as a runtime bug.
    link.ok_or_else(|| worker::Error::RustError("upsert returned no row".into()).into())
}

pub async fn set_metadata(db: &D1Database, id: i64, meta: &Metadata) -> AppResult<()> {
    db.prepare(
        "UPDATE links SET title = COALESCE(?1, title), site = COALESCE(?2, site),
                          thumbnail = COALESCE(?3, thumbnail)
         WHERE id = ?4",
    )
    .bind(&[
        opt_text(meta.title.as_deref()),
        opt_text(meta.site.as_deref()),
        opt_text(meta.thumbnail.as_deref()),
        num(id),
    ])?
    .run()
    .await?;
    Ok(())
}

/// Returns `false` if no link has that id.
pub async fn set_read(db: &D1Database, id: i64, read_at: Option<i64>) -> AppResult<bool> {
    let result = db
        .prepare("UPDATE links SET read_at = ?1 WHERE id = ?2")
        .bind(&[read_at.map_or(JsValue::NULL, num), num(id)])?
        .run()
        .await?;
    Ok(rows_changed(&result)? > 0)
}

/// Returns `false` if no link has that id.
pub async fn delete(db: &D1Database, id: i64) -> AppResult<bool> {
    let result = db
        .prepare("DELETE FROM links WHERE id = ?1")
        .bind(&[num(id)])?
        .run()
        .await?;
    Ok(rows_changed(&result)? > 0)
}

fn rows_changed(result: &worker::D1Result) -> AppResult<usize> {
    Ok(result
        .meta()?
        .and_then(|meta| meta.changes)
        .unwrap_or(0))
}
