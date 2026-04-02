-- Kong-Rust 初始 migration：创建 schema_meta 和 10 个核心实体表
-- 参考 Kong 原版 000_base.lua，适配 kong-rust 的 EntitySchema 定义
-- 表创建顺序按外键依赖排列

-- ============================================================
-- schema_meta：migration 版本追踪
-- ============================================================
CREATE TABLE IF NOT EXISTS schema_meta (
    key            TEXT NOT NULL,
    subsystem      TEXT NOT NULL,
    last_executed  TEXT,
    executed       TEXT[],
    pending        TEXT[],

    PRIMARY KEY (key, subsystem)
);

-- ============================================================
-- certificates（无依赖，被 services/upstreams/snis 引用）
-- ============================================================
CREATE TABLE IF NOT EXISTS certificates (
    id         UUID                     PRIMARY KEY,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    cert       TEXT                     NOT NULL,
    key        TEXT                     NOT NULL,
    cert_alt   TEXT,
    key_alt    TEXT,
    tags       TEXT[]
);

-- ============================================================
-- ca_certificates（无依赖）
-- ============================================================
CREATE TABLE IF NOT EXISTS ca_certificates (
    id          UUID                     PRIMARY KEY,
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    cert        TEXT                     NOT NULL,
    cert_digest TEXT,
    tags        TEXT[]
);

-- ============================================================
-- consumers（无依赖，被 plugins 引用）
-- ============================================================
CREATE TABLE IF NOT EXISTS consumers (
    id         UUID                     PRIMARY KEY,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    username   TEXT                     UNIQUE,
    custom_id  TEXT                     UNIQUE,
    tags       TEXT[]
);

-- ============================================================
-- services（依赖 certificates）
-- ============================================================
CREATE TABLE IF NOT EXISTS services (
    id                   UUID                     PRIMARY KEY,
    created_at           TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at           TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name                 TEXT                     UNIQUE,
    retries              BIGINT,
    protocol             TEXT                     NOT NULL,
    host                 TEXT                     NOT NULL,
    port                 BIGINT                   NOT NULL,
    path                 TEXT,
    connect_timeout      BIGINT,
    write_timeout        BIGINT,
    read_timeout         BIGINT,
    tags                 TEXT[],
    client_certificate_id UUID                    REFERENCES certificates(id),
    tls_verify           BOOLEAN,
    tls_verify_depth     SMALLINT,
    ca_certificates      UUID[],
    enabled              BOOLEAN                  DEFAULT true
);

-- ============================================================
-- routes（依赖 services）
-- ============================================================
CREATE TABLE IF NOT EXISTS routes (
    id                          UUID                     PRIMARY KEY,
    created_at                  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at                  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name                        TEXT                     UNIQUE,
    protocols                   TEXT[],
    methods                     TEXT[],
    hosts                       TEXT[],
    paths                       TEXT[],
    headers                     JSONB,
    https_redirect_status_code  INTEGER,
    regex_priority              BIGINT,
    strip_path                  BOOLEAN                  NOT NULL,
    path_handling               TEXT                     DEFAULT 'v0',
    preserve_host               BOOLEAN                  NOT NULL,
    request_buffering           BOOLEAN                  DEFAULT true,
    response_buffering          BOOLEAN                  DEFAULT true,
    tags                        TEXT[],
    service_id                  UUID                     REFERENCES services(id),
    snis                        TEXT[],
    sources                     JSONB[],
    destinations                JSONB[],
    expression                  TEXT,
    priority                    BIGINT
);

-- ============================================================
-- snis（依赖 certificates）
-- ============================================================
CREATE TABLE IF NOT EXISTS snis (
    id              UUID                     PRIMARY KEY,
    created_at      TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at      TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name            TEXT                     NOT NULL UNIQUE,
    tags            TEXT[],
    certificate_id  UUID                     NOT NULL REFERENCES certificates(id) ON DELETE CASCADE
);

-- ============================================================
-- upstreams（依赖 certificates）
-- ============================================================
CREATE TABLE IF NOT EXISTS upstreams (
    id                         UUID                     PRIMARY KEY,
    created_at                 TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at                 TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name                       TEXT                     NOT NULL UNIQUE,
    algorithm                  TEXT,
    hash_on_cookie_path        TEXT,
    hash_on                    TEXT,
    hash_fallback              TEXT,
    hash_on_header             TEXT,
    hash_fallback_header       TEXT,
    hash_on_cookie             TEXT,
    hash_on_query_arg          TEXT,
    hash_fallback_query_arg    TEXT,
    hash_on_uri_capture        TEXT,
    hash_fallback_uri_capture  TEXT,
    slots                      BIGINT                   DEFAULT 10000,
    healthchecks               JSONB,
    tags                       TEXT[],
    host_header                TEXT,
    client_certificate_id      UUID                     REFERENCES certificates(id),
    use_srv_name               BOOLEAN                  DEFAULT false
);

-- ============================================================
-- targets（依赖 upstreams）
-- ============================================================
CREATE TABLE IF NOT EXISTS targets (
    id          UUID                     PRIMARY KEY,
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(3) AT TIME ZONE 'UTC'),
    updated_at  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(3) AT TIME ZONE 'UTC'),
    target      TEXT                     NOT NULL,
    weight      INTEGER                  DEFAULT 100,
    cache_key   TEXT                     UNIQUE,
    tags        TEXT[],
    upstream_id UUID                     NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE
);

-- ============================================================
-- plugins（依赖 routes, services, consumers）
-- ============================================================
CREATE TABLE IF NOT EXISTS plugins (
    id              UUID                     PRIMARY KEY,
    created_at      TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at      TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    name            TEXT                     NOT NULL,
    config          JSONB                    NOT NULL,
    enabled         BOOLEAN                  DEFAULT true,
    instance_name   TEXT,
    protocols       TEXT[],
    cache_key       TEXT                     UNIQUE,
    tags            TEXT[],
    route_id        UUID                     REFERENCES routes(id) ON DELETE CASCADE,
    service_id      UUID                     REFERENCES services(id) ON DELETE CASCADE,
    consumer_id     UUID                     REFERENCES consumers(id) ON DELETE CASCADE
);

-- ============================================================
-- sm_vaults（无依赖）
-- ============================================================
CREATE TABLE IF NOT EXISTS sm_vaults (
    id          UUID                     PRIMARY KEY,
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    updated_at  TIMESTAMP WITH TIME ZONE DEFAULT (CURRENT_TIMESTAMP(0) AT TIME ZONE 'UTC'),
    prefix      TEXT                     NOT NULL UNIQUE,
    name        TEXT                     NOT NULL,
    description TEXT,
    config      JSONB                    NOT NULL,
    tags        TEXT[]
);

-- ============================================================
-- 索引（参考 Kong 原版 000_base.lua）
-- ============================================================

-- services 索引
CREATE INDEX IF NOT EXISTS services_fkey_client_certificate ON services(client_certificate_id);

-- routes 索引
CREATE INDEX IF NOT EXISTS routes_fkey_service ON routes(service_id);

-- snis 索引
CREATE INDEX IF NOT EXISTS snis_fkey_certificate ON snis(certificate_id);

-- upstreams 索引
CREATE INDEX IF NOT EXISTS upstreams_fkey_client_certificate ON upstreams(client_certificate_id);

-- targets 索引
CREATE INDEX IF NOT EXISTS targets_fkey_upstream ON targets(upstream_id);
CREATE INDEX IF NOT EXISTS targets_target ON targets(target);

-- plugins 索引
CREATE INDEX IF NOT EXISTS plugins_fkey_route ON plugins(route_id);
CREATE INDEX IF NOT EXISTS plugins_fkey_service ON plugins(service_id);
CREATE INDEX IF NOT EXISTS plugins_fkey_consumer ON plugins(consumer_id);
CREATE INDEX IF NOT EXISTS plugins_name ON plugins(name);

-- tags GIN 索引（支持标签过滤查询）
CREATE INDEX IF NOT EXISTS services_tags_idx ON services USING GIN(tags);
CREATE INDEX IF NOT EXISTS routes_tags_idx ON routes USING GIN(tags);
CREATE INDEX IF NOT EXISTS consumers_tags_idx ON consumers USING GIN(tags);
CREATE INDEX IF NOT EXISTS plugins_tags_idx ON plugins USING GIN(tags);
CREATE INDEX IF NOT EXISTS upstreams_tags_idx ON upstreams USING GIN(tags);
CREATE INDEX IF NOT EXISTS targets_tags_idx ON targets USING GIN(tags);
CREATE INDEX IF NOT EXISTS certificates_tags_idx ON certificates USING GIN(tags);
CREATE INDEX IF NOT EXISTS snis_tags_idx ON snis USING GIN(tags);
CREATE INDEX IF NOT EXISTS ca_certificates_tags_idx ON ca_certificates USING GIN(tags);
CREATE INDEX IF NOT EXISTS sm_vaults_tags_idx ON sm_vaults USING GIN(tags);
