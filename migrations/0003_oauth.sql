-- Owner OAuth (slice 5b-1). New tables only; the earlier migrations are untouched.

-- Pending authorisation states: one row per "Connect" click, used once.
-- Stored as issued (not hashed): the value is 256 random bits, lives ten minutes,
-- is deleted on first use, and the callback it unlocks sits behind the owner sign-in.
-- The domain decides expiry; rows past it are dropped whenever a new state is saved.
CREATE TABLE oauth_state (
  state TEXT PRIMARY KEY CHECK (length(state) = 64),
  expires_at INTEGER NOT NULL CHECK (typeof(expires_at) = 'integer')
);

-- The owner's provider token, encrypted (AES-256-GCM, key in the TOKEN_ENC_KEY secret).
-- ciphertext and nonce are standard base64 text; the nonce is 12 bytes, new on every
-- write; the provider name is the additional authenticated data. The ciphertext holds
-- the whole token, expiry included, and is the authoritative copy: expires_at here is
-- only for looking at the table. Never store a token in another column.
-- needs_reconnect is set by the scheduled run when the provider refuses the token. A
-- refresh save leaves it alone; only the owner's connect clears it.
CREATE TABLE oauth_tokens (
  provider TEXT PRIMARY KEY CHECK (provider IN ('youtube')),
  ciphertext TEXT NOT NULL CHECK (length(ciphertext) BETWEEN 1 AND 24000),
  nonce TEXT NOT NULL CHECK (length(nonce) = 16),
  expires_at INTEGER NOT NULL CHECK (typeof(expires_at) = 'integer'),
  needs_reconnect INTEGER NOT NULL DEFAULT 0 CHECK (needs_reconnect IN (0, 1)),
  updated_at INTEGER NOT NULL CHECK (typeof(updated_at) = 'integer')
);
