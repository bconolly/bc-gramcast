-- Publish settings (slice 6a). The earlier migrations are untouched.
--
-- One row, enforced by the primary key: id can only be 1. With no row, the Worker falls
-- back to the PUBLISH_ENABLED and PUBLISH_PRIVACY vars. Apply this to the remote
-- database BEFORE deploying the Worker that reads it.
--   publish_enabled: 0 or 1 (stored as an integer, never text)
--   publish_privacy: the three names YouTube takes
--   updated_at: Unix seconds from the Worker's clock; it strictly increases with every
--     save and is what a save must quote to prove it saw the latest row
CREATE TABLE settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  publish_enabled INTEGER NOT NULL
    CHECK (typeof(publish_enabled) = 'integer' AND publish_enabled IN (0, 1)),
  publish_privacy TEXT NOT NULL
    CHECK (publish_privacy IN ('private', 'unlisted', 'public')),
  updated_at INTEGER NOT NULL
    CHECK (typeof(updated_at) = 'integer' AND updated_at >= 0)
);
