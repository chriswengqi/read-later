# Read Later

A personal "save this for later" list. Share any link (article, YouTube video, X post…) from an
iPhone share sheet or a desktop bookmarklet; open the list later and tap through to the original.

- **Backend:** Rust on Cloudflare Workers (`worker` crate → WebAssembly), D1 (SQLite) database
- **Frontend:** a single static page in `public/`, installable to the iPhone Home Screen
- **Cost:** $0. Everything fits in Cloudflare's free tier.

Titles and thumbnails are fetched in the background after saving (YouTube/X via oEmbed, other
sites via OpenGraph tags), so saving is instant.

## Code map

| File | What it does |
|---|---|
| `src/lib.rs` | Entry point, routing, auth |
| `src/error.rs` | `AppError`: one error type, converted to HTTP responses in one place |
| `src/links.rs` | `Link` model and all SQL |
| `src/url_clean.rs` | URL canonicalization (strip tracking params, unify youtu.be / twitter.com…) |
| `src/metadata.rs` | Background title/thumbnail fetching |
| `src/html.rs` | Tiny `<head>` scanner for `<title>` / `<meta>` tags |
| `migrations/` | D1 schema migrations, applied automatically on deploy |

## Local development

Prerequisites: Rust (stable) with `wasm32-unknown-unknown`, Node 22+, and on Linux `pkg-config libssl-dev`.

```sh
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.7 --locked
npm install
echo "AUTH_TOKEN=dev-token-change-me" > .dev.vars
npm run db:migrate:local
npm run dev            # http://localhost:8787
cargo test             # unit tests (run natively, no Workers runtime needed)
```

## API

All endpoints require `Authorization: Bearer <AUTH_TOKEN>`.

| Method | Path | Body | |
|---|---|---|---|
| `GET` | `/api/links?status=unread\|read` | | List links |
| `POST` | `/api/links` | `{"url": "..."}` | Save (also accepts shared text containing a URL) |
| `PATCH` | `/api/links/:id` | `{"read": true}` | Mark read / unread |
| `DELETE` | `/api/links/:id` | | Delete |
| `GET` | `/save?url=...&token=...` | | Bookmarklet endpoint |

## Deployment

Every push to `main` runs clippy and the tests, then applies D1 migrations and deploys
(`.github/workflows/deploy.yml`). Required repository secrets: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`.
