-- Favourites are independent of read/unread: a link can be both done and a favourite.
ALTER TABLE links ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_links_favorite ON links (favorite, saved_at DESC);
