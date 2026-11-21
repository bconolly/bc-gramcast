-- Queued posts. slot_at is UNIQUE so two requests can't take the same slot.
-- GLOB is case-sensitive (LIKE is not), so 'videos/x.MP4' is refused.
CREATE TABLE posts (
  id TEXT PRIMARY KEY,
  video_key TEXT NOT NULL UNIQUE CHECK (video_key GLOB 'videos/*.mp4'),
  title TEXT NOT NULL,
  caption TEXT NOT NULL,
  platforms TEXT NOT NULL CHECK (platforms IN ('instagram', 'youtube', 'instagram,youtube', 'youtube,instagram')),
  slot_at INTEGER NOT NULL UNIQUE
);
