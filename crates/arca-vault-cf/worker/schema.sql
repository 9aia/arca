-- D1 schema for the arca-vault-cf worker.
-- ciphertext is base64 age ciphertext — THE STORE NEVER SEES
-- PLAINTEXT. Apply with:
--   wrangler d1 execute arca-vault --file=schema.sql
CREATE TABLE IF NOT EXISTS secrets (
  env        TEXT NOT NULL,
  ref        TEXT NOT NULL,                -- ref path, or HMAC-derived
                                           -- ref_key when private_refs
  ciphertext TEXT NOT NULL,                -- base64 age ciphertext, opaque
  name       TEXT,                         -- private-refs name blob
                                           -- (base64 age ciphertext of
                                           -- the real ref); NULL on
                                           -- plain rows
  version    INTEGER NOT NULL DEFAULT 1,   -- server-side write generation
  created_at TEXT NOT NULL,                -- ISO 8601 / RFC 3339
  rotated_at TEXT,                         -- set by rotate writes only
  PRIMARY KEY (env, ref)
);

-- Upgrading a v1 database? Add the name column once:
--   wrangler d1 execute arca-vault --command="ALTER TABLE secrets ADD COLUMN name TEXT"
