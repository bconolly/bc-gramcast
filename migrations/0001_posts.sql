-- Never applied anywhere yet, so this file is edited in place. Once applied,
-- change the schema with a new migration instead.
-- Queued posts. slot_at is UNIQUE so two requests can't take the same slot.
-- GLOB is case-sensitive (LIKE is not), so 'videos/x.MP4' is refused.
CREATE TABLE posts (
  id TEXT PRIMARY KEY,
  video_key TEXT NOT NULL UNIQUE CHECK (
    -- Same rule as the domain: videos/ + 1 to 64 of [A-Za-z0-9_-] + .mp4.
    video_key GLOB 'videos/*.mp4'
    AND length(video_key) BETWEEN 12 AND 75
    AND substr(video_key, 8, length(video_key) - 11) NOT GLOB '*[^A-Za-z0-9_-]*'
  ),
  title TEXT NOT NULL,
  caption TEXT NOT NULL,
  platforms TEXT NOT NULL CHECK (platforms IN ('instagram', 'youtube', 'instagram,youtube', 'youtube,instagram')),
  slot_at INTEGER NOT NULL UNIQUE
);
