CREATE TABLE links (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    url       TEXT    NOT NULL UNIQUE,
    title     TEXT,
    site      TEXT,
    thumbnail TEXT,
    kind      TEXT    NOT NULL DEFAULT 'article', -- article | video | post
    saved_at  INTEGER NOT NULL,                    -- unix millis
    read_at   INTEGER                              -- NULL = unread
);

CREATE INDEX idx_links_unread ON links (read_at, saved_at DESC);
