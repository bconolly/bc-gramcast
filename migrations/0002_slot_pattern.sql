-- Never applied anywhere yet, so this file is edited in place. Once applied,
-- change the schema with a new migration instead.
-- The owner's weekly posting pattern. One row only: id is pinned to 1.
-- rules is compact text like 'mon@06:00,wed@19:00' (empty for none). The
-- domain parses and validates it again on read, so a bad row is refused.
CREATE TABLE slot_pattern (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  utc_offset_minutes INTEGER NOT NULL CHECK (
    typeof(utc_offset_minutes) = 'integer' AND utc_offset_minutes BETWEEN -720 AND 840
  ),
  rules TEXT NOT NULL CHECK (length(rules) <= 400)
);
