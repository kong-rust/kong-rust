# DB ws_id 兼容 + WebSocket 代理支持 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复数据库 ws_id 丢失和 WebSocket 代理无响应两个基础问题，实现 Kong 数据库零成本切换和 WebSocket 协议代理。

**Architecture:** 在所有 10 个核心实体的模型/迁移/DAO 中添加 ws_id 字段（透传不丢失策略），新增 workspaces 表。WebSocket 代理通过确保 Pingora 正确接收和转发 HTTP Upgrade 头来实现。

**Tech Stack:** Rust, sqlx (PostgreSQL), Pingora, uuid crate

---

## File Structure

### Block 1: ws_id 兼容

**新建文件：**
- `crates/kong-db/migrations/core/001_add_workspaces.sql` — 新增 workspaces 表 + 所有实体表添加 ws_id 列

**修改文件（模型层，每个加 `ws_id: Option<Uuid>`）：**
- `crates/kong-core/src/models/service.rs`
- `crates/kong-core/src/models/route.rs`
- `crates/kong-core/src/models/consumer.rs`
- `crates/kong-core/src/models/plugin.rs`
- `crates/kong-core/src/models/upstream.rs`
- `crates/kong-core/src/models/target.rs`
- `crates/kong-core/src/models/certificate.rs`
- `crates/kong-core/src/models/sni.rs`
- `crates/kong-core/src/models/ca_certificate.rs`
- `crates/kong-core/src/models/vault.rs`

**修改文件（DAO 层）：**
- `crates/kong-db/src/dao/postgres.rs` — 每个 `*_schema()` 函数添加 `.column("ws_id", "ws_id", ColumnType::Uuid, true)`
- `crates/kong-db/src/migrations.rs` — `CORE_MIGRATIONS` 数组添加 `001_add_workspaces` 条目 + `KNOWN_TABLES` 添加 `"workspaces"`

### Block 2: WebSocket 代理

**修改文件：**
- `crates/kong-proxy/src/lib.rs` — 修复 `upstream_request_filter` 中的 WebSocket 头转发逻辑，确保 `Sec-WebSocket-Key`、`Sec-WebSocket-Version` 等关键头被透传

---

## Task 1: 创建 workspaces 迁移 SQL

**Files:**
- Create: `crates/kong-db/migrations/core/001_add_workspaces.sql`

- [ ] **Step 1: 编写迁移 SQL**

创建 workspaces 表和默认工作空间，然后为所有 10 个实体表添加 ws_id 列。

```sql
-- Kong-Rust migration 001：添加 workspaces 表和 ws_id 列
-- 参考 Kong 原版 009_200_to_210.lua

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

-- 插入默认工作空间（Kong 的默认 ws_id）
INSERT INTO workspaces (id, name)
VALUES ('00000000-0000-0000-0000-000000000000', 'default')
ON CONFLICT (id) DO NOTHING;

-- ============================================================
-- 为所有实体表添加 ws_id 列
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
```

- [ ] **Step 2: 提交迁移文件**

```bash
git add crates/kong-db/migrations/core/001_add_workspaces.sql
git commit -m "feat(db): add workspaces table and ws_id migration SQL / 添加 workspaces 表和 ws_id 迁移 SQL"
```

---

## Task 2: 注册迁移到 migration 引擎

**Files:**
- Modify: `crates/kong-db/src/migrations.rs:18-21`

- [ ] **Step 1: 在 CORE_MIGRATIONS 数组中添加新迁移条目**

找到 `crates/kong-db/src/migrations.rs` 的 `CORE_MIGRATIONS` 常量，将：

```rust
const CORE_MIGRATIONS: &[Migration] = &[Migration {
    name: "000_base",
    sql: include_str!("../migrations/core/000_base.sql"),
}];
```

改为：

```rust
const CORE_MIGRATIONS: &[Migration] = &[
    Migration {
        name: "000_base",
        sql: include_str!("../migrations/core/000_base.sql"),
    },
    Migration {
        name: "001_add_workspaces",
        sql: include_str!("../migrations/core/001_add_workspaces.sql"),
    },
];
```

- [ ] **Step 2: 在 KNOWN_TABLES 数组末尾添加 workspaces**

找到 `KNOWN_TABLES` 常量（约第 29 行），将 `"schema_meta"` 之后添加 `"workspaces"`：

```rust
const KNOWN_TABLES: &[&str] = &[
    "plugins",
    "targets",
    "snis",
    "routes",
    "upstreams",
    "services",
    "consumers",
    "certificates",
    "ca_certificates",
    "sm_vaults",
    "schema_meta",
    "workspaces",
];
```

注意：`workspaces` 放在最后，因为其他表的 ws_id 外键引用了它，必须最后删除。

- [ ] **Step 3: 验证编译通过**

Run: `cd /Users/dawxy/proj/kong-rust/.claude/worktrees/silly-williams && cargo check -p kong-db`
Expected: 编译成功，无错误

- [ ] **Step 4: 提交**

```bash
git add crates/kong-db/src/migrations.rs
git commit -m "feat(db): register 001_add_workspaces migration / 注册 workspaces 迁移"
```

---

## Task 3: 为所有模型添加 ws_id 字段

**Files:**
- Modify: `crates/kong-core/src/models/service.rs`
- Modify: `crates/kong-core/src/models/route.rs`
- Modify: `crates/kong-core/src/models/consumer.rs`
- Modify: `crates/kong-core/src/models/plugin.rs`
- Modify: `crates/kong-core/src/models/upstream.rs`
- Modify: `crates/kong-core/src/models/target.rs`
- Modify: `crates/kong-core/src/models/certificate.rs`
- Modify: `crates/kong-core/src/models/sni.rs`
- Modify: `crates/kong-core/src/models/ca_certificate.rs`
- Modify: `crates/kong-core/src/models/vault.rs`

对每个模型文件执行相同的模式：

- [ ] **Step 1: 在每个模型 struct 中添加 ws_id 字段**

在 `tags` 字段之后（或其他合适位置）添加：

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
```

对每个模型的 `Default` impl 中添加：

```rust
            ws_id: None,
```

具体文件列表和上下文：

**service.rs** — 在 `enabled: bool,` 之后加 ws_id 字段，Default 的 `enabled: true,` 之后加 `ws_id: None,`

**route.rs** — 在 `priority` 字段之后加 ws_id 字段

**consumer.rs** — 在 `tags` 字段之后加 ws_id 字段

**plugin.rs** — 在现有字段末尾加 ws_id 字段（注意 plugin.rs 可能有 `ordering` 字段）

**upstream.rs** — 在末尾字段之后加 ws_id 字段

**target.rs** — 在末尾字段之后加 ws_id 字段

**certificate.rs** — 在 `tags` 字段之后加 ws_id 字段

**sni.rs** — 在 `certificate` 字段之后加 ws_id 字段

**ca_certificate.rs** — 在 `tags` 字段之后加 ws_id 字段

**vault.rs** — 在 `tags` 字段之后加 ws_id 字段

- [ ] **Step 2: 验证编译通过**

Run: `cd /Users/dawxy/proj/kong-rust/.claude/worktrees/silly-williams && cargo check -p kong-core`
Expected: 编译成功

- [ ] **Step 3: 提交**

```bash
git add crates/kong-core/src/models/
git commit -m "feat(core): add ws_id field to all entity models / 为所有实体模型添加 ws_id 字段"
```

---

## Task 4: 为所有 EntitySchema 添加 ws_id 列定义

**Files:**
- Modify: `crates/kong-db/src/dao/postgres.rs:1066-1221`

- [ ] **Step 1: 在每个 `*_schema()` 函数末尾添加 ws_id 列**

对以下 10 个 schema 函数，在最后一个 `.xxx()` 调用之后、`}` 之前添加：

```rust
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
```

需要修改的函数列表：
- `service_schema()` (行 ~1066)
- `route_schema()` (行 ~1088)
- `consumer_schema()` (行 ~1115)
- `upstream_schema()` (行 ~1125)
- `target_schema()` (行 ~1150)
- `plugin_schema()` (行 ~1163)
- `certificate_schema()` (行 ~1180)
- `sni_schema()` (行 ~1192)
- `ca_certificate_schema()` (行 ~1202)
- `vault_schema()` (行 ~1212)

例如 `service_schema()` 改为：

```rust
pub fn service_schema() -> EntitySchema {
    EntitySchema::new("services")
        .pk()
        .timestamps()
        .text_opt("name")
        .integer("retries")
        .text("protocol")
        .text("host")
        .integer("port")
        .text_opt("path")
        .integer("connect_timeout")
        .integer("write_timeout")
        .integer("read_timeout")
        .tags()
        .foreign_key("client_certificate")
        .boolean_opt("tls_verify")
        .integer_opt("tls_verify_depth")
        .uuid_array("ca_certificates")
        .boolean("enabled")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}
```

- [ ] **Step 2: 验证整个 workspace 编译通过**

Run: `cd /Users/dawxy/proj/kong-rust/.claude/worktrees/silly-williams && cargo check`
Expected: 编译成功（可能有 warnings，不应有 errors）

- [ ] **Step 3: 提交**

```bash
git add crates/kong-db/src/dao/postgres.rs
git commit -m "feat(db): add ws_id column to all EntitySchema definitions / 为所有 EntitySchema 添加 ws_id 列定义"
```

---

## Task 5: 验证 ws_id 端到端工作

**Files:** (无新文件，仅运行测试)

- [ ] **Step 1: 运行现有测试确认无回归**

Run: `cd /Users/dawxy/proj/kong-rust/.claude/worktrees/silly-williams && cargo test --workspace 2>&1 | tail -20`
Expected: 现有测试全部通过（或至少不因 ws_id 变更而失败）

- [ ] **Step 2: 检查 Admin API JSON 序列化包含 ws_id**

手动检查（如果有测试环境）：通过 Admin API 创建 Service，确认返回的 JSON 中包含 `"ws_id": null` 或 `"ws_id": "00000000-..."` 字段。

由于 serde 的 `skip_serializing_if = "Option::is_none"` 标注，ws_id 为 None 时不会出现在 JSON 中。这与 Kong 行为一致——Kong OSS 的 ws_id 始终存在（默认工作空间），但我们的透传策略是：
- 从 Kong DB 读出来的数据有 ws_id → 透传保留
- 通过 kong-rust Admin API 新建的数据 ws_id 为 NULL → 不序列化（Kong 会自动填充默认值）

- [ ] **Step 3: 提交验证通过的状态**

如果测试有需要修复的地方，在此步骤修复并提交。

---

## Task 6: WebSocket 代理修复

**Files:**
- Modify: `crates/kong-proxy/src/lib.rs:995-1008`

- [ ] **Step 1: 研究 Pingora WebSocket 支持**

在实现前，先确认 Pingora 的行为。检查 Pingora 源码或文档：
- Pingora 的 `HttpProxy` 是否自动处理 HTTP/1.1 101 Switching Protocols？
- 当 upstream 返回 101 时，Pingora 是否自动切换到双向字节流转发？

Run: `grep -r "upgrade\|websocket\|101\|Switching" ~/.cargo/registry/src/*/pingora-proxy-*/src/ 2>/dev/null | head -30`

也可以在 GitHub 上查看 `cloudflare/pingora` 仓库的 `pingora-proxy/src/proxy_h1.rs` 和 `proxy_common.rs`。

**关键假设**：Pingora 作为 Cloudflare 的生产代理，很可能自动处理 WebSocket 升级。问题可能出在 `upstream_request_filter` 中头部处理不完整——缺少关键的 WebSocket 握手头。

- [ ] **Step 2: 修复 WebSocket 头透传逻辑**

找到 `crates/kong-proxy/src/lib.rs` 中的 WebSocket 头部处理代码（约第 995-1008 行），修改为完整透传所有 WebSocket 握手头：

```rust
// 7. WebSocket proxy: forward Upgrade/Connection and all WebSocket handshake headers — WebSocket 代理：透传升级头和所有 WebSocket 握手头
{
    let is_websocket = session
        .req_header()
        .headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if is_websocket {
        let _ = upstream_request.insert_header("upgrade", "websocket");
        let _ = upstream_request.insert_header("connection", "upgrade");

        // 透传 WebSocket 握手必需头（原代码缺少这些，导致握手失败）
        for header_name in &[
            "sec-websocket-key",
            "sec-websocket-version",
            "sec-websocket-protocol",
            "sec-websocket-extensions",
        ] {
            if let Some(val) = session.req_header().headers.get(*header_name) {
                let _ = upstream_request.insert_header(*header_name, val.to_str().unwrap_or(""));
            }
        }
    }
}
```

- [ ] **Step 3: 验证编译通过**

Run: `cd /Users/dawxy/proj/kong-rust/.claude/worktrees/silly-williams && cargo check -p kong-proxy`
Expected: 编译成功

- [ ] **Step 4: 提交**

```bash
git add crates/kong-proxy/src/lib.rs
git commit -m "fix(proxy): forward all WebSocket handshake headers for proper upgrade / 修复 WebSocket 握手头透传，确保协议升级正常工作"
```

---

## Task 7: WebSocket 代理端到端验证

- [ ] **Step 1: 如果有运行环境，进行手动测试**

1. 启动一个 WebSocket echo server（如 `websocat -s 9999`）
2. 配置 Kong-Rust Service 指向 echo server
3. 用 WebSocket 客户端通过 Kong-Rust 代理连接

如果 Pingora 底层不支持 WebSocket 升级，需要考虑替代方案：
- 在 L4 Stream 代理层处理 WebSocket
- 或在 Pingora 的 `upstream_response_filter` 中检测 101 并切换处理模式

- [ ] **Step 2: 记录发现和下一步**

如果 WebSocket 仍然不工作，记录具体的失败现象（如：上游是否收到了升级请求？返回了什么状态码？），以便后续定位。

---

## 完成后更新 spec-workflow

- [ ] **更新 tasks.md** — 添加新任务条目并标记完成
- [ ] **记录 Implementation Log** — 使用 `mcp__spec-workflow__log-implementation` 工具
- [ ] **更新 structure.md** — 如果新增了文件
