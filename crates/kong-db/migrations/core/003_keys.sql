-- Kong key_sets + keys tables (Kong 3.10 compatible)
-- Kong key_sets + keys 表（与 Kong 3.10 兼容）
-- Derived from Kong migration core/017_300_to_310.lua — 源于 Kong 迁移 core/017_300_to_310.lua
--
-- Note: Kong's original uses DO $$ ... $$ blocks for EXCEPTION handling, but our
-- migration runner splits on ';' and cannot parse dollar-quoted strings.
-- CREATE INDEX/TABLE IF NOT EXISTS is already idempotent so we use them directly.
-- 注意：Kong 原版使用 DO $$ ... $$ 包裹 EXCEPTION，但我们的 migration 执行器按 ';'
-- 拆分，无法解析 dollar-quoted 字符串。CREATE INDEX/TABLE IF NOT EXISTS 已是幂等
-- 操作，直接使用即可。

CREATE TABLE IF NOT EXISTS "key_sets" (
  "id"           UUID                       PRIMARY KEY,
  "name"         TEXT                       UNIQUE,
  "tags"         TEXT[],
  "ws_id"        UUID                       REFERENCES "workspaces" ("id"),
  "created_at"   TIMESTAMP WITH TIME ZONE,
  "updated_at"   TIMESTAMP WITH TIME ZONE
);

CREATE INDEX IF NOT EXISTS "key_sets_tags_idx" ON "key_sets" USING GIN ("tags");

CREATE TABLE IF NOT EXISTS "keys" (
  "id"           UUID                       PRIMARY KEY,
  "set_id"       UUID                       REFERENCES "key_sets" ("id") ON DELETE CASCADE,
  "name"         TEXT                       UNIQUE,
  "cache_key"    TEXT                       UNIQUE,
  "ws_id"        UUID                       REFERENCES "workspaces" ("id"),
  "kid"          TEXT,
  "jwk"          TEXT,
  "pem"          JSONB,
  "tags"         TEXT[],
  "created_at"   TIMESTAMP WITH TIME ZONE,
  "updated_at"   TIMESTAMP WITH TIME ZONE,
  UNIQUE ("kid", "set_id")
);

CREATE INDEX IF NOT EXISTS "keys_fkey_key_sets" ON "keys" ("set_id");
CREATE INDEX IF NOT EXISTS "keys_tags_idx" ON "keys" USING GIN ("tags");
