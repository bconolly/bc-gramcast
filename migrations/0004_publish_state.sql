-- Publishing state (slice 5b-2). The earlier migrations are untouched. Existing
-- rows become Queued with 0 attempts, which is what they were.
--
-- One column per field of the domain's PublishState. A field that does not belong to
-- the current state is NULL, and attempts and not_before are 0 outside Queued and
-- Failed, so a state has exactly one stored shape and the Worker's compare-and-set
-- can compare every column.
--   queued:     attempts, not_before
--   publishing: attempt (1 or more), started_at
--   published:  external_id (the provider's video id)
--   failed:     attempts, fail_reason (one of the fixed codes)
-- ALTER TABLE cannot add a table-level CHECK, so the cross-column rule sits on the
-- last column added, where every earlier column already exists.
ALTER TABLE posts ADD COLUMN state TEXT NOT NULL DEFAULT 'queued'
  CHECK (state IN ('queued', 'publishing', 'published', 'failed'));
ALTER TABLE posts ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0
  CHECK (typeof(attempts) = 'integer' AND attempts BETWEEN 0 AND 4294967295);
ALTER TABLE posts ADD COLUMN not_before INTEGER NOT NULL DEFAULT 0
  CHECK (typeof(not_before) = 'integer' AND not_before >= 0);
ALTER TABLE posts ADD COLUMN attempt INTEGER
  CHECK (attempt IS NULL OR (typeof(attempt) = 'integer' AND attempt BETWEEN 1 AND 4294967295));
ALTER TABLE posts ADD COLUMN started_at INTEGER
  CHECK (started_at IS NULL OR (typeof(started_at) = 'integer' AND started_at >= 0));
-- Same rule as the domain's is_valid_external_id: 1 to 32 of [A-Za-z0-9_-].
ALTER TABLE posts ADD COLUMN external_id TEXT
  CHECK (external_id IS NULL OR (typeof(external_id) = 'text'
    AND length(external_id) BETWEEN 1 AND 32
    AND external_id NOT GLOB '*[^A-Za-z0-9_-]*'));
ALTER TABLE posts ADD COLUMN fail_reason TEXT
  CHECK (
    (fail_reason IS NULL OR fail_reason IN
      ('auth', 'rate_limited', 'rejected', 'transient', 'stuck_publishing', 'unknown'))
    AND CASE state
      WHEN 'queued' THEN
        attempt IS NULL AND started_at IS NULL AND external_id IS NULL AND fail_reason IS NULL
      WHEN 'publishing' THEN
        attempt IS NOT NULL AND started_at IS NOT NULL AND external_id IS NULL
        AND fail_reason IS NULL AND attempts = 0 AND not_before = 0
      WHEN 'published' THEN
        external_id IS NOT NULL AND attempt IS NULL AND started_at IS NULL
        AND fail_reason IS NULL AND attempts = 0 AND not_before = 0
      WHEN 'failed' THEN
        fail_reason IS NOT NULL AND attempt IS NULL AND started_at IS NULL
        AND external_id IS NULL AND not_before = 0
      ELSE 0
    END
  );
