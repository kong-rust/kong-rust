-- Kong-Rust migration 001：添加 workspaces 表和 ws_id 列
-- 参考 Kong 原版 009_200_to_210.lua
-- 策略：透传不丢失 — 确保 ws_id 列存在且数据完整，不做工作空间隔离

-- ============================================================
-- workspaces 表
-- ============================================================
CREATE TABLE IF NOT EXISTS workspaces (
    id         UUID                     PRIMARY KEY,
    name       TEXT                     NOT NULL UNIQUE,
    comment    TEXT,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    meta       JSONB,
    config     JSONB
);

-- 插入默认工作空间（仅在全新数据库时生效；已有 Kong DB 中 workspaces 表已存在则跳过）
INSERT INTO workspaces (id, name)
VALUES ('00000000-0000-0000-0000-000000000000', 'default')
ON CONFLICT DO NOTHING;

-- ============================================================
-- 为所有实体表添加 ws_id 列
-- 使用动态子查询获取默认工作空间 ID，兼容已有 Kong DB（其默认 ws 的 UUID 不是全零）
-- ============================================================

-- services
ALTER TABLE services ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE services SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS services_ws_id_idx ON services(ws_id);

-- routes
ALTER TABLE routes ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE routes SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS routes_ws_id_idx ON routes(ws_id);

-- consumers
ALTER TABLE consumers ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE consumers SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS consumers_ws_id_idx ON consumers(ws_id);

-- plugins
ALTER TABLE plugins ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE plugins SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS plugins_ws_id_idx ON plugins(ws_id);

-- upstreams
ALTER TABLE upstreams ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE upstreams SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS upstreams_ws_id_idx ON upstreams(ws_id);

-- targets
ALTER TABLE targets ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE targets SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS targets_ws_id_idx ON targets(ws_id);

-- certificates
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE certificates SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS certificates_ws_id_idx ON certificates(ws_id);

-- snis
ALTER TABLE snis ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE snis SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS snis_ws_id_idx ON snis(ws_id);

-- ca_certificates
ALTER TABLE ca_certificates ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE ca_certificates SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS ca_certificates_ws_id_idx ON ca_certificates(ws_id);

-- sm_vaults
ALTER TABLE sm_vaults ADD COLUMN IF NOT EXISTS ws_id UUID REFERENCES workspaces(id);
UPDATE sm_vaults SET ws_id = (SELECT id FROM workspaces WHERE name = 'default') WHERE ws_id IS NULL;
CREATE INDEX IF NOT EXISTS sm_vaults_ws_id_idx ON sm_vaults(ws_id);
