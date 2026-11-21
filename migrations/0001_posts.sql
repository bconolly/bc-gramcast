-- Queued posts. slot_at is UNIQUE so two requests can't take the same slot.
CREATE TABLE posts (
  id TEXT PRIMARY KEY,
  video_key TEXT NOT NULL CHECK (video_key LIKE 'videos/%'),
  title TEXT NOT NULL,
  caption TEXT NOT NULL,
  platforms TEXT NOT NULL,
  slot_at INTEGER NOT NULL UNIQUE
);
