# Phase 2: AI Gateway 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Kong-Rust 添加完整的 AI Gateway 能力 — 多 Provider LLM 代理 + 双协议暴露 + LB/Fallback + Virtual Key + Token 限流 + 语义缓存 + Prompt Guard

**Architecture:** 新建 `kong-ai` crate，包含共享基础设施（Provider trait、codec、token 计数、限流器）和 4 个独立插件（ai-proxy、ai-rate-limit、ai-cache、ai-prompt-guard）。插件通过现有 `PluginHandler` trait 接入 Pingora 代理流水线。AI 实体（ai_providers、ai_models、ai_virtual_keys）通过 Admin API CRUD 管理。插件间通过 `RequestCtx.extensions` (anymap2) 传递类型化 AI 上下文。

**Tech Stack:** Rust, Pingora, axum (Admin API), sqlx (PostgreSQL), tiktoken-rs, dashmap, sha2, redis (可选), anymap2, serde/serde_json

**Spec:** `docs/superpowers/specs/2026-03-22-phase2-ai-gateway-design.md`

---

## 依赖关系图

```
Task 1: crate 骨架 + RequestCtx 扩展
  ↓
Task 2: AI 数据模型 + DAO（复用 Dao<T> + PgDao<T> 模式）
  ↓
Task 3: 协议类型定义 + SSE 解析器 + Provider trait + OpenAI driver
  ↓
Task 4: Anthropic/Gemini codec 扩展
  ↓
Task 5: ai-proxy 插件核心（单 provider 代理，非流式）
  ↓
Task 6: ai-proxy 流式 SSE 支持
  ↓
Task 7: Anthropic driver + Claude 协议暴露
  ↓
Task 8: Gemini driver + OpenAI Compat driver
  ↓
Task 9: Model Group LB + Fallback
  ↓
Task 10: Token 计数 + 可观测性
  ↓
Task 11: Admin API — AI 实体 CRUD
  ↓
Task 12: ai-rate-limit 插件 (Virtual Key + TPM/RPM)
  ↓
Task 13: ai-cache 插件 (Redis KV + 语义缓存)
  ↓
Task 14: ai-prompt-guard 插件
  ↓
Task 15: 集成测试 + 端到端验证
```

---

## Task 1: Crate 骨架 + RequestCtx extensions 扩展

**Files:**
- Create: `crates/kong-ai/Cargo.toml`
- Create: `crates/kong-ai/src/lib.rs`
- Modify: `Cargo.toml` (workspace root, 行 3–13 members 列表)
- Modify: `crates/kong-core/Cargo.toml` (添加 anymap2 依赖)
- Modify: `crates/kong-core/src/traits/plugin.rs` (行 7–89, RequestCtx 增加 extensions 字段)

- [ ] **Step 1: 在 workspace root Cargo.toml 添加 kong-ai 成员**

在 `Cargo.toml` 的 `members` 列表中追加 `"crates/kong-ai"`。在 `[workspace.dependencies]` 中添加：
```toml
kong-ai = { path = "crates/kong-ai" }
anymap2 = "0.13"
tiktoken-rs = "0.6"
sha2 = "0.10"
dashmap = "6"
redis = { version = "0.27", features = ["tokio-comp"], optional = true }
```

- [ ] **Step 2: 创建 kong-ai crate**

```toml
# crates/kong-ai/Cargo.toml
[package]
name = "kong-ai"
version = "0.1.0"
edition = "2021"

[dependencies]
kong-core = { workspace = true }
kong-db = { workspace = true }
kong-plugin-system = { workspace = true }
kong-config = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
bytes = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }
tiktoken-rs = { workspace = true }
sha2 = { workspace = true }
dashmap = { workspace = true }
anymap2 = { workspace = true }
regex = { workspace = true }
tokio = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["full", "test-util"] }
```

```rust
// crates/kong-ai/src/lib.rs
pub mod models;
pub mod dao;
pub mod provider;
pub mod codec;
pub mod token;
pub mod ratelimit;
pub mod plugins;
```

创建各子模块的空文件：
- `crates/kong-ai/src/models.rs`
- `crates/kong-ai/src/dao.rs`
- `crates/kong-ai/src/provider/mod.rs`
- `crates/kong-ai/src/codec/mod.rs`
- `crates/kong-ai/src/token/mod.rs`
- `crates/kong-ai/src/ratelimit/mod.rs`
- `crates/kong-ai/src/plugins/mod.rs`

每个文件暂时写 `// TODO` 即可。

- [ ] **Step 3: 给 RequestCtx 添加 extensions 字段**

在 `crates/kong-core/Cargo.toml` 中添加 `anymap2 = { workspace = true }`。

修改 `crates/kong-core/src/traits/plugin.rs`：

在 `RequestCtx` struct 中（行 89 之前）添加：
```rust
    /// Type-safe extensions map for cross-plugin typed data (e.g. AI context) — 类型安全的扩展 map，用于跨插件类型化数据（如 AI 上下文）
    pub extensions: anymap2::SendSyncMap,
```

在 `RequestCtx::new()` 中（行 134 之前）添加：
```rust
            extensions: anymap2::SendSyncMap::new(),
```

- [ ] **Step 4: 验证编译**

Run: `cargo check -p kong-ai -p kong-core`
Expected: 编译通过，无 error

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/ Cargo.toml crates/kong-core/
git commit -m "feat: 新建 kong-ai crate 骨架 + RequestCtx extensions 扩展
add kong-ai crate skeleton + RequestCtx extensions field (anymap2)"
```

---

## Task 2: AI 数据模型 + DAO

**Files:**
- Create: `crates/kong-ai/src/models.rs`
- Create: `crates/kong-ai/src/dao.rs`
- Create: `crates/kong-db/migrations/core/002_ai_gateway.sql`
- Test: `crates/kong-ai/tests/dao_test.rs`

- [ ] **Step 1: 编写 AI 实体模型**

```rust
// crates/kong-ai/src/models.rs
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// AI Provider 配置（对应 ai_providers 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,           // openai | anthropic | gemini | openai_compat
    pub endpoint_url: Option<String>,
    pub auth_config: serde_json::Value,  // { header_name, header_value, ... }
    pub default_model: Option<String>,
    pub config: serde_json::Value,       // provider 特定配置
    pub enabled: bool,
    pub ws_id: Option<Uuid>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// AI Model（对应 ai_models 表，同 name 组成 model group）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiModel {
    pub id: Uuid,
    pub name: String,                    // 对客户端暴露的名称
    pub provider_id: Uuid,
    pub model_name: String,              // provider 侧真实模型名
    pub priority: i32,                   // Fallback 优先级 (高=优先)
    pub weight: i32,                     // LB 权重
    pub input_cost: Option<f64>,         // 每百万 token 输入成本 (USD)
    pub output_cost: Option<f64>,        // 每百万 token 输出成本 (USD)
    pub max_tokens: Option<i32>,
    pub config: serde_json::Value,       // 模型级覆盖参数
    pub enabled: bool,
    pub ws_id: Option<Uuid>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// AI Virtual Key（对应 ai_virtual_keys 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiVirtualKey {
    pub id: Uuid,
    pub name: String,
    pub key_hash: String,                // SHA-256(raw_key)
    pub key_prefix: String,              // 前 8 字符
    pub consumer_id: Option<Uuid>,
    pub allowed_models: Vec<String>,
    pub tpm_limit: Option<i32>,
    pub rpm_limit: Option<i32>,
    pub budget_limit: Option<f64>,
    pub budget_used: f64,
    pub enabled: bool,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ws_id: Option<Uuid>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 认证配置（嵌入在 AiProviderConfig.auth_config 中）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    pub header_name: Option<String>,
    pub header_value: Option<String>,
    pub param_name: Option<String>,
    pub param_value: Option<String>,
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub aws_region: Option<String>,
    pub gcp_service_account_json: Option<String>,
}
```

- [ ] **Step 2: 编写 SQL migration**

```sql
-- crates/kong-db/migrations/core/002_ai_gateway.sql

-- AI Provider 配置
CREATE TABLE IF NOT EXISTS ai_providers (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name          TEXT UNIQUE NOT NULL,
    provider_type TEXT NOT NULL,
    endpoint_url  TEXT,
    auth_config   JSONB NOT NULL DEFAULT '{}',
    default_model TEXT,
    config        JSONB NOT NULL DEFAULT '{}',
    enabled       BOOLEAN NOT NULL DEFAULT true,
    ws_id         UUID REFERENCES workspaces(id),
    created_at    TIMESTAMPTZ DEFAULT now(),
    updated_at    TIMESTAMPTZ DEFAULT now()
);

-- AI Model (同 name 组成 model group 用于 LB)
CREATE TABLE IF NOT EXISTS ai_models (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name          TEXT NOT NULL,
    provider_id   UUID NOT NULL REFERENCES ai_providers(id) ON DELETE CASCADE,
    model_name    TEXT NOT NULL,
    priority      INTEGER NOT NULL DEFAULT 0,
    weight        INTEGER NOT NULL DEFAULT 100,
    input_cost    NUMERIC,
    output_cost   NUMERIC,
    max_tokens    INTEGER,
    config        JSONB NOT NULL DEFAULT '{}',
    enabled       BOOLEAN NOT NULL DEFAULT true,
    ws_id         UUID REFERENCES workspaces(id),
    created_at    TIMESTAMPTZ DEFAULT now(),
    updated_at    TIMESTAMPTZ DEFAULT now(),
    UNIQUE(name, provider_id, ws_id)
);

-- AI Virtual API Key
CREATE TABLE IF NOT EXISTS ai_virtual_keys (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name          TEXT UNIQUE NOT NULL,
    key_hash      TEXT UNIQUE NOT NULL,
    key_prefix    TEXT NOT NULL,
    consumer_id   UUID REFERENCES consumers(id) ON DELETE SET NULL,
    allowed_models TEXT[] DEFAULT '{}',
    tpm_limit     INTEGER,
    rpm_limit     INTEGER,
    budget_limit  NUMERIC,
    budget_used   NUMERIC NOT NULL DEFAULT 0,
    enabled       BOOLEAN NOT NULL DEFAULT true,
    expires_at    TIMESTAMPTZ,
    ws_id         UUID REFERENCES workspaces(id),
    created_at    TIMESTAMPTZ DEFAULT now(),
    updated_at    TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_models_name ON ai_models(name);
CREATE INDEX IF NOT EXISTS idx_ai_models_provider_id ON ai_models(provider_id);
CREATE INDEX IF NOT EXISTS idx_ai_virtual_keys_key_hash ON ai_virtual_keys(key_hash);
CREATE INDEX IF NOT EXISTS idx_ai_virtual_keys_consumer_id ON ai_virtual_keys(consumer_id);
```

- [ ] **Step 3: 为 AI 实体实现 Entity trait，复用 PgDao<T>**

**重要**：复用现有 `Dao<T: Entity>` + `PgDao<T>` 模式，不要定义独立的 DAO trait。

在 `crates/kong-ai/src/dao.rs` 中：
1. 为 `AiProviderConfig`、`AiModel`、`AiVirtualKey` 实现 `kong_core::traits::Entity` + `PrimaryKey` trait
2. 定义各实体的 `EntitySchema`（列名 → ColumnType 映射）
3. `AdminState` 中使用 `Arc<dyn Dao<AiProviderConfig>>` 等标准字段

对于 `PgDao<T>` 无法覆盖的特殊查询（如 `get_by_hash`、`list_by_name`、`update_budget`），在 `crates/kong-ai/src/dao.rs` 中定义一个瘦的扩展 trait：

```rust
// 扩展查询 — 仅包含 PgDao<T> 通用 CRUD 不覆盖的操作
#[async_trait::async_trait]
pub trait AiVirtualKeyExt: Send + Sync {
    async fn get_by_hash(&self, key_hash: &str) -> Result<Option<AiVirtualKey>>;
    async fn update_budget(&self, id: &Uuid, cost_delta: f64) -> Result<()>;
}

#[async_trait::async_trait]
pub trait AiModelExt: Send + Sync {
    async fn list_by_model_name(&self, name: &str) -> Result<Vec<AiModel>>;
}
```

为 `PgDao<AiVirtualKey>` 和 `PgDao<AiModel>` 实现这些扩展 trait。

- [ ] **Step 4: 编写 DAO 单测**

`crates/kong-ai/tests/dao_test.rs` — 测试 Entity trait 实现和扩展查询（需要 PostgreSQL，标记 `#[ignore]`），参考 `crates/kong-admin/tests/admin_api_compat.rs` 的测试基础设施。

- [ ] **Step 5: 注册 migration 到 kong-db**

在 `crates/kong-db/src/migrations.rs` 中添加 `002_ai_gateway.sql` 到 migration 列表（编号根据执行时的实际顺序调整）。

- [ ] **Step 6: 验证编译 + 测试**

Run: `cargo check -p kong-ai && cargo test -p kong-ai`
Expected: 编译通过

- [ ] **Step 7: Commit**

```bash
git add crates/kong-ai/src/models.rs crates/kong-ai/src/dao.rs crates/kong-ai/src/dao/ crates/kong-db/migrations/
git commit -m "feat: AI 数据模型 + DAO + migration
add AI entity models (AiProviderConfig, AiModel, AiVirtualKey) + DAO traits + PostgreSQL migration"
```

---

## Task 3: Provider Trait + OpenAI Driver

**Files:**
- Create: `crates/kong-ai/src/provider/mod.rs`
- Create: `crates/kong-ai/src/provider/openai.rs`
- Test: `crates/kong-ai/tests/provider_openai_test.rs`

- [ ] **Step 1: 定义 AiDriver trait 和相关类型**

```rust
// crates/kong-ai/src/provider/mod.rs
pub mod openai;

use async_trait::async_trait;
use bytes::Bytes;
use crate::models::{AiModel, AiProviderConfig};
use crate::codec::{ChatRequest, ChatResponse, SseEvent};
use kong_core::error::Result;

/// 上游连接配置（由 driver 生成）
#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

/// Provider 转换后的请求
#[derive(Debug)]
pub struct ProviderRequest {
    pub body: Bytes,
    pub content_type: String,
    pub extra_headers: Vec<(String, String)>,
}

/// Token 使用量
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// LLM Provider 驱动接口
#[async_trait]
pub trait AiDriver: Send + Sync {
    fn provider_type(&self) -> &str;

    fn transform_request(
        &self,
        request: &ChatRequest,
        model: &AiModel,
        provider_config: &AiProviderConfig,
    ) -> Result<ProviderRequest>;

    fn transform_response(
        &self,
        status: u16,
        body: &Bytes,
        model: &AiModel,
    ) -> Result<ChatResponse>;

    fn transform_stream_event(
        &self,
        event: &SseEvent,
        model: &AiModel,
    ) -> Result<Option<SseEvent>>;

    fn configure_upstream(
        &self,
        model: &AiModel,
        provider_config: &AiProviderConfig,
    ) -> Result<UpstreamConfig>;

    fn extract_usage(&self, body: &Bytes) -> Option<TokenUsage>;
    fn extract_stream_usage(&self, event: &SseEvent) -> Option<TokenUsage>;
}

/// Driver 注册表
pub struct DriverRegistry {
    drivers: std::collections::HashMap<String, std::sync::Arc<dyn AiDriver>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            drivers: std::collections::HashMap::new(),
        };
        registry.register("openai", std::sync::Arc::new(openai::OpenAiDriver));
        registry
    }

    pub fn register(&mut self, name: &str, driver: std::sync::Arc<dyn AiDriver>) {
        self.drivers.insert(name.to_string(), driver);
    }

    pub fn get(&self, provider_type: &str) -> Option<&std::sync::Arc<dyn AiDriver>> {
        self.drivers.get(provider_type)
    }
}
```

- [ ] **Step 2: 编写测试 — OpenAI driver transform_request**

```rust
// crates/kong-ai/tests/provider_openai_test.rs
#[test]
fn test_openai_transform_request_chat() {
    // 构造 ChatRequest，验证 OpenAI driver 几乎透传
    // 验证 stream=true 时自动注入 stream_options.include_usage
}

#[test]
fn test_openai_transform_response() {
    // 构造 OpenAI JSON 响应，验证反序列化为 ChatResponse
}

#[test]
fn test_openai_configure_upstream() {
    // 验证 host=api.openai.com, port=443, path=/v1/chat/completions
    // 验证 Authorization header
}

#[test]
fn test_openai_extract_usage() {
    // 从标准 OpenAI 响应中提取 prompt_tokens/completion_tokens
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p kong-ai -- provider_openai`
Expected: 编译错误（codec 模块尚未实现）

- [ ] **Step 4: 实现 OpenAI driver**

`crates/kong-ai/src/provider/openai.rs` — 最简单的 driver：
- `transform_request`: 序列化 ChatRequest，设置 model 字段，stream=true 时注入 `stream_options.include_usage`
- `transform_response`: 反序列化 JSON → ChatResponse
- `configure_upstream`: scheme="https", host="api.openai.com", port=443, path="/v1/chat/completions"（chat）或 "/v1/completions"（completions），从 auth_config 提取 `Authorization: Bearer <key>`
- `extract_usage`: 从 `response.usage` 字段提取

注意：此 step 依赖 Task 4 的 codec 类型定义。如果 Task 3 和 4 并行开发，先在 codec/mod.rs 中定义最小的 `ChatRequest`/`ChatResponse`/`SseEvent` 类型。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p kong-ai -- provider_openai`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/kong-ai/src/provider/
git commit -m "feat: Provider trait + OpenAI driver
add AiDriver trait, DriverRegistry, and OpenAI driver implementation"
```

---

## Task 4: SSE 解析器 + 协议 Codec

**Files:**
- Create: `crates/kong-ai/src/codec/mod.rs`
- Create: `crates/kong-ai/src/codec/openai_format.rs`
- Create: `crates/kong-ai/src/codec/anthropic_format.rs`
- Create: `crates/kong-ai/src/codec/sse.rs`
- Test: `crates/kong-ai/tests/codec_test.rs`
- Test: `crates/kong-ai/tests/sse_test.rs`

- [ ] **Step 1: 定义 OpenAI 格式类型（内部规范化格式）**

```rust
// crates/kong-ai/src/codec/openai_format.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message { /* role, content, tool_calls, tool_call_id */ }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse { /* id, object, model, choices, usage */ }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOptions { pub include_usage: Option<bool> }

// ... Tool, ToolCall, Choice, Usage 等完整定义
```

- [ ] **Step 2: 编写 SSE 解析器测试**

```rust
// crates/kong-ai/tests/sse_test.rs
#[test]
fn test_sse_parse_single_event() {
    let mut parser = SseParser::new(SseFormat::Standard);
    let events = parser.feed(b"data: {\"id\":\"1\"}\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "{\"id\":\"1\"}");
}

#[test]
fn test_sse_parse_split_across_chunks() {
    // 一个 SSE event 被拆成两个 chunk
    let mut parser = SseParser::new(SseFormat::Standard);
    let events1 = parser.feed(b"data: {\"id\":");
    assert_eq!(events1.len(), 0);  // 不完整，不产出
    let events2 = parser.feed(b"\"1\"}\n\n");
    assert_eq!(events2.len(), 1);
}

#[test]
fn test_sse_parse_done_terminator() {
    let mut parser = SseParser::new(SseFormat::Standard);
    let events = parser.feed(b"data: [DONE]\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "[DONE]");
}

#[test]
fn test_sse_parse_multiple_events_in_one_chunk() { /* ... */ }
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p kong-ai -- sse_test`
Expected: FAIL (SseParser 未实现)

- [ ] **Step 4: 实现 SSE 解析器**

```rust
// crates/kong-ai/src/codec/sse.rs
use bytes::BytesMut;

pub enum SseFormat {
    Standard,       // text/event-stream
    Ndjson,         // application/x-ndjson
}

pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

pub struct SseParser {
    buffer: BytesMut,
    format: SseFormat,
}

impl SseParser {
    pub fn new(format: SseFormat) -> Self { /* ... */ }
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> { /* ... */ }
    pub fn flush(&mut self) -> Vec<SseEvent> { /* ... */ }
}
```

核心逻辑：
- 将 chunk 追加到 buffer
- 按 `\n\n` 分割完整的 event block
- 每个 block 按行解析 `data:` / `event:` / `id:` 字段
- 不完整的部分保留在 buffer 中等待下次 feed

- [ ] **Step 5: 运行 SSE 测试确认通过**

Run: `cargo test -p kong-ai -- sse_test`
Expected: PASS

- [ ] **Step 6: 编写 Anthropic 格式转换测试**

```rust
// crates/kong-ai/tests/codec_test.rs
#[test]
fn test_anthropic_decode_request_to_openai() {
    // Anthropic Messages 格式 → OpenAI ChatRequest
    // 验证 system 字段提取为 messages[0]
    // 验证 tool_use → tool_calls 映射
}

#[test]
fn test_anthropic_encode_response_from_openai() {
    // OpenAI ChatResponse → Anthropic Messages 响应
}
```

- [ ] **Step 7: 实现 Anthropic codec**

`crates/kong-ai/src/codec/anthropic_format.rs`:
- `decode_request(body: &Bytes) -> Result<ChatRequest>`: Anthropic → OpenAI
- `encode_response(response: &ChatResponse) -> Result<Bytes>`: OpenAI → Anthropic
- `encode_stream_event(event: &SseEvent) -> Result<Vec<SseEvent>>`: OpenAI SSE → Anthropic SSE

- [ ] **Step 8: 运行全部 codec 测试**

Run: `cargo test -p kong-ai -- codec`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/kong-ai/src/codec/
git commit -m "feat: SSE 解析器 + OpenAI/Anthropic 协议 codec
add SseParser (cross-chunk reassembly) + OpenAI format types + Anthropic bidirectional codec"
```

---

## Task 5: ai-proxy 插件核心（单 provider，非流式）

**Files:**
- Create: `crates/kong-ai/src/plugins/mod.rs`
- Create: `crates/kong-ai/src/plugins/ai_proxy.rs`
- Create: `crates/kong-ai/src/plugins/context.rs` (AiRequestState)
- Modify: `crates/kong-server/src/main.rs` (行 212–240, build_plugin_registry 注册 ai-proxy)
- Test: `crates/kong-ai/tests/ai_proxy_test.rs`

- [ ] **Step 1: 定义 AiRequestState（跨阶段共享）**

```rust
// crates/kong-ai/src/plugins/context.rs
use crate::provider::{AiDriver, TokenUsage, UpstreamConfig};
use crate::models::{AiModel, AiProviderConfig};
use crate::codec::sse::SseParser;
use bytes::BytesMut;
use std::sync::Arc;
use std::time::Instant;

/// 客户端协议类型
#[derive(Debug, Clone, Copy)]
pub enum ClientProtocol {
    OpenAi,
    Anthropic,
}

/// AI 插件跨阶段共享状态
pub struct AiRequestState {
    pub driver: Arc<dyn AiDriver>,
    pub model: AiModel,
    pub provider_config: AiProviderConfig,
    pub stream_mode: bool,
    pub client_protocol: ClientProtocol,
    pub sse_parser: Option<SseParser>,
    pub usage: TokenUsage,
    pub response_buffer: Option<BytesMut>,
    pub request_start: Instant,
    pub ttft: Option<Instant>,
}
```

- [ ] **Step 2: 编写 ai-proxy access 阶段测试**

```rust
// crates/kong-ai/tests/ai_proxy_test.rs
#[tokio::test]
async fn test_ai_proxy_access_sets_upstream() {
    // 1. 构造含 model="gpt-4" 的 PluginConfig
    // 2. 构造含 OpenAI chat request body 的 RequestCtx
    // 3. 调用 ai_proxy.access(&config, &mut ctx)
    // 4. 验证 ctx.upstream_target_host == "api.openai.com"
    // 5. 验证 ctx.upstream_target_port == 443
    // 6. 验证 ctx.upstream_body 被替换为 provider 格式
    // 7. 验证 ctx.extensions 包含 AiRequestState
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p kong-ai -- ai_proxy_test`
Expected: FAIL

- [ ] **Step 4: 实现 AiProxyPlugin struct**

```rust
// crates/kong-ai/src/plugins/ai_proxy.rs
use kong_core::traits::{PluginHandler, PluginConfig, RequestCtx};
use crate::provider::DriverRegistry;
use crate::models::{AiModel, AiProviderConfig};

pub struct AiProxyPlugin {
    driver_registry: DriverRegistry,
    // model group 和 provider config 从 ctx 中的 shared 数据或直接从 DAO 加载
    // MVP: 从 plugin config JSON 中直接读取 provider 配置
    // 后续: 从 DAO 加载并缓存
}

#[async_trait::async_trait]
impl PluginHandler for AiProxyPlugin {
    fn name(&self) -> &str { "ai-proxy" }
    fn priority(&self) -> i32 { 770 }
    fn version(&self) -> &str { "0.1.0" }
    fn has_body_filter(&self) -> bool { true }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 解析 config JSON 获取 model name, route_type, client_protocol
        // 2. 解析 request body → ChatRequest (根据 client_protocol 选 codec)
        // 3. 查找 model (MVP: 从 config 直接读; 后续: 从 DAO 查 model group)
        // 4. 获取 driver + provider_config
        // 5. driver.transform_request() → ProviderRequest
        // 6. driver.configure_upstream() → UpstreamConfig
        // 7. 设置 ctx.upstream_target_host/port/scheme/path/body/headers
        // 8. 存 AiRequestState 到 ctx.extensions
        Ok(())
    }

    async fn header_filter(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 检测流式/非流式, 初始化 SseParser
        Ok(())
    }

    async fn body_filter(
        &self, config: &PluginConfig, ctx: &mut RequestCtx,
        body: &mut Option<bytes::Bytes>, end_of_stream: bool,
    ) -> Result<()> {
        // 非流式: end_of_stream 时转换响应
        // 流式: 逐 chunk SSE 转换 (Task 6)
        Ok(())
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 序列化 analytics 到 log_serialize
        Ok(())
    }
}
```

- [ ] **Step 5: 实现 access 阶段 + 非流式 body_filter**

填充 access 和 body_filter 的实际逻辑。非流式 body_filter 在 `end_of_stream=true` 时：
1. 从 body 读取完整响应
2. 从 `ctx.extensions` 取出 `AiRequestState`
3. `driver.transform_response()` 转换
4. 根据 `client_protocol` 做最终编码
5. 替换 `*body`

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p kong-ai -- ai_proxy_test`
Expected: PASS

- [ ] **Step 7: 在 kong-server 注册 ai-proxy 插件**

修改 `crates/kong-server/src/main.rs` 的 `build_plugin_registry` 函数：
```rust
// 在 Lua 插件加载之后添加
registry.register("ai-proxy", std::sync::Arc::new(
    kong_ai::plugins::AiProxyPlugin::new(/* ... */)
));
```

在 `crates/kong-server/Cargo.toml` 中添加 `kong-ai = { workspace = true }`。

- [ ] **Step 8: 验证编译**

Run: `cargo check -p kong-server`
Expected: 编译通过

- [ ] **Step 9: Commit**

```bash
git add crates/kong-ai/src/plugins/ crates/kong-server/
git commit -m "feat: ai-proxy 插件核心（非流式 + OpenAI provider）
add ai-proxy PluginHandler with access/body_filter phases, single OpenAI provider, non-streaming"
```

---

## Task 6: ai-proxy 流式 SSE 支持

**Files:**
- Modify: `crates/kong-ai/src/plugins/ai_proxy.rs`
- Test: `crates/kong-ai/tests/ai_proxy_streaming_test.rs`

- [ ] **Step 1: 编写流式 body_filter 测试**

```rust
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming() {
    // 1. 构造包含 OpenAI SSE chunk 的 body
    // 2. 模拟多次 body_filter 调用 (end_of_stream=false)
    // 3. 验证每次输出的 chunk 是合法的 SSE event
    // 4. 最后一次 end_of_stream=true, 验证 flush
    // 5. 验证 AiRequestState.usage 累加正确
    // 6. 验证 response_buffer 包含完整响应
}
```

- [ ] **Step 2: 实现流式 body_filter**

在 `body_filter` 中增加流式分支：
```rust
if ai_state.stream_mode {
    if let Some(ref mut parser) = ai_state.sse_parser {
        let events = if end_of_stream {
            parser.flush()
        } else {
            parser.feed(chunk_bytes)
        };

        let mut output = BytesMut::new();
        for event in events {
            // transform_stream_event
            // extract_stream_usage, 累加
            // client_protocol 转换
            // 写入 response_buffer
            // 拼接 "data: ...\n\n" 到 output
        }
        *body = Some(output.freeze());
    }
}
```

- [ ] **Step 3: 实现 header_filter 流式检测**

在 `header_filter` 中：
```rust
let content_type = ctx.response_headers.get("content-type");
let is_stream = content_type.map_or(false, |ct| ct.contains("text/event-stream") || ct.contains("application/x-ndjson"));

if is_stream {
    ai_state.stream_mode = true;
    ai_state.sse_parser = Some(SseParser::new(SseFormat::Standard));
    ai_state.response_buffer = Some(BytesMut::new());
    // 设置响应 Content-Type
    ctx.response_headers_to_set.push(("content-type".to_string(), "text/event-stream".to_string()));
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p kong-ai -- ai_proxy_streaming`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/src/plugins/ai_proxy.rs crates/kong-ai/tests/
git commit -m "feat: ai-proxy 流式 SSE 支持
add streaming SSE body_filter with cross-chunk reassembly and response buffering"
```

---

## Task 7: Anthropic Driver + Claude 协议暴露

**Files:**
- Create: `crates/kong-ai/src/provider/anthropic.rs`
- Modify: `crates/kong-ai/src/provider/mod.rs` (注册 anthropic driver)
- Modify: `crates/kong-ai/src/plugins/ai_proxy.rs` (access 阶段 Claude 协议解码)
- Test: `crates/kong-ai/tests/provider_anthropic_test.rs`

- [ ] **Step 1: 编写 Anthropic driver 测试**

```rust
#[test]
fn test_anthropic_transform_request() {
    // OpenAI ChatRequest → Anthropic 原生格式
    // system message 提取为独立字段
    // tool_calls → tool_use 格式
}

#[test]
fn test_anthropic_transform_response() {
    // Anthropic 原生响应 → OpenAI ChatResponse
    // content blocks → choices 映射
    // usage: input_tokens/output_tokens → prompt_tokens/completion_tokens
}

#[test]
fn test_anthropic_transform_stream_events() {
    // message_start / content_block_delta / message_delta / message_stop
}

#[test]
fn test_anthropic_configure_upstream() {
    // host=api.anthropic.com, path=/v1/messages
    // x-api-key header + anthropic-version header
}
```

- [ ] **Step 2: 实现 Anthropic driver**

`crates/kong-ai/src/provider/anthropic.rs`:
- `transform_request`: OpenAI messages → Anthropic 格式（system 独立字段，tool_calls 映射）
- `transform_response`: Anthropic content blocks → OpenAI choices
- `transform_stream_event`: 处理 5 种 Anthropic SSE event type
- `configure_upstream`: api.anthropic.com:443, /v1/messages, x-api-key + anthropic-version headers

在 `provider/mod.rs` 的 `DriverRegistry::new()` 中注册。

- [ ] **Step 3: 在 ai-proxy access 阶段添加 Claude 客户端协议支持**

当 `config.client_protocol == "anthropic"` 时：
1. 用 `AnthropicCodec::decode_request()` 将 Claude Messages 请求转为 OpenAI ChatRequest
2. 在 body_filter 响应阶段用 `AnthropicCodec::encode_response()` / `encode_stream_event()` 转回

- [ ] **Step 4: 运行全部测试**

Run: `cargo test -p kong-ai`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/src/provider/anthropic.rs crates/kong-ai/src/plugins/
git commit -m "feat: Anthropic driver + Claude 客户端协议暴露
add Anthropic driver (message format, 5 SSE event types) + /v1/messages client protocol support"
```

---

## Task 8: Gemini Driver + OpenAI Compat Driver

**Files:**
- Create: `crates/kong-ai/src/provider/gemini.rs`
- Create: `crates/kong-ai/src/provider/openai_compat.rs`
- Modify: `crates/kong-ai/src/provider/mod.rs` (注册两个 driver)
- Test: `crates/kong-ai/tests/provider_gemini_test.rs`
- Test: `crates/kong-ai/tests/provider_compat_test.rs`

- [ ] **Step 1: 编写 Gemini driver 测试**

测试覆盖：
- transform_request: OpenAI messages → Gemini `generateContent` 格式（contents 数组，role user/model）
- transform_response: Gemini candidates → OpenAI choices
- configure_upstream: generativelanguage.googleapis.com, `/v1beta/models/{model}:streamGenerateContent?alt=sse`
- extract_usage: `usageMetadata.promptTokenCount` / `candidatesTokenCount`

- [ ] **Step 2: 实现 Gemini driver**

关键差异：
- 使用 `?alt=sse` 获取标准 SSE 格式（简化解析器）
- Auth: API key 作为 query param `?key=xxx` 或 Bearer token
- 没有 system role → 用 `systemInstruction` 字段

- [ ] **Step 3: 编写 OpenAI Compat driver 测试**

验证：
- 继承 OpenAI driver 的转换逻辑
- 允许自定义 endpoint_url
- 从 `config.compat_vendor` 读取厂商标识（qwen / hunyuan）

- [ ] **Step 4: 实现 OpenAI Compat driver**

```rust
// crates/kong-ai/src/provider/openai_compat.rs
pub struct OpenAiCompatDriver;

// 大部分方法委托给 OpenAiDriver
// configure_upstream 使用 provider_config.endpoint_url 替代硬编码的 api.openai.com
```

- [ ] **Step 5: 在 DriverRegistry 注册**

```rust
registry.register("gemini", Arc::new(GeminiDriver));
registry.register("openai_compat", Arc::new(OpenAiCompatDriver));
```

- [ ] **Step 6: 运行全部测试**

Run: `cargo test -p kong-ai`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/kong-ai/src/provider/
git commit -m "feat: Gemini driver + OpenAI Compat driver (Qwen/混元)
add Gemini driver (generateContent + alt=sse) + OpenAI compatible driver for Qwen, Hunyuan"
```

---

## Task 9: Model Group LB + Fallback

**Files:**
- Create: `crates/kong-ai/src/provider/balancer.rs`
- Modify: `crates/kong-ai/src/plugins/ai_proxy.rs` (access 阶段使用 balancer)
- Test: `crates/kong-ai/tests/balancer_test.rs`

- [ ] **Step 1: 编写 LB 测试**

```rust
#[test]
fn test_balancer_weighted_round_robin() {
    // 3 个同 priority model, weight 80/10/10
    // 1000 次 select, 验证比例 ~80%/10%/10%
}

#[test]
fn test_balancer_fallback_on_failure() {
    // priority=10 的 model 连续失败 3 次 → 进入冷却
    // select 自动 fallback 到 priority=5 的 model
}

#[test]
fn test_balancer_cooldown_recovery() {
    // 冷却 30s 后自动恢复
}

#[test]
fn test_balancer_429_immediate_cooldown() {
    // 报告 429 → 立即 10s 冷却
}
```

- [ ] **Step 2: 实现 ModelGroupBalancer**

```rust
// crates/kong-ai/src/provider/balancer.rs
use crate::models::{AiModel, AiProviderConfig};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct ModelGroupBalancer {
    models: Vec<ModelEntry>,
    rr_counter: AtomicU64,
}

struct ModelEntry {
    model: AiModel,
    provider_config: AiProviderConfig,
    health: ModelHealth,
}

struct ModelHealth {
    consecutive_failures: AtomicU32,
    cooldown_until: parking_lot::Mutex<Option<Instant>>,
}

impl ModelGroupBalancer {
    pub fn new(models: Vec<(AiModel, AiProviderConfig)>) -> Self { /* ... */ }

    pub fn select(&self) -> Result<(&AiModel, &AiProviderConfig)> {
        // 1. 按 priority 降序分组
        // 2. 从最高 priority 组开始，过滤掉冷却中的
        // 3. 在可用组内按 weight 加权 round-robin
        // 4. 全部不可用 → fallback 到下一组
        // 5. 全组不可用 → Err
    }

    pub fn report_success(&self, model_id: &Uuid) { /* reset consecutive_failures */ }
    pub fn report_failure(&self, model_id: &Uuid, status: Option<u16>) {
        // 429 → 10s 冷却
        // 连续 3 次失败 → 30s 冷却
    }
}
```

- [ ] **Step 3: 集成到 ai-proxy access 阶段**

修改 ai-proxy 的 access：
1. 从 DAO（或缓存）加载 model group（按 config.model 名查询）
2. 构造 `ModelGroupBalancer`（可缓存在 AiProxyPlugin 中）
3. `balancer.select()` 获取 model + provider
4. 在 log 阶段调用 `report_success/report_failure`

- [ ] **Step 4: 运行测试**

Run: `cargo test -p kong-ai -- balancer`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/src/provider/balancer.rs crates/kong-ai/src/plugins/
git commit -m "feat: Model Group 加权 LB + 优先级 Fallback + 冷却机制
add ModelGroupBalancer with weighted round-robin, priority fallback, and cooldown on 429/consecutive failures"
```

---

## Task 10: Token 计数 + 可观测性

**Files:**
- Create: `crates/kong-ai/src/token/counter.rs`
- Create: `crates/kong-ai/src/token/cost.rs`
- Modify: `crates/kong-ai/src/plugins/ai_proxy.rs` (log 阶段序列化 analytics)
- Test: `crates/kong-ai/tests/token_test.rs`

- [ ] **Step 1: 编写 token 计数测试**

```rust
#[test]
fn test_token_count_from_provider_usage() {
    // provider 返回 usage → 直接使用
}

#[test]
fn test_token_count_tiktoken_fallback() {
    // provider 无 usage → tiktoken 精确计数 (gpt-4)
}

#[test]
fn test_token_count_estimate_fallback() {
    // 非 GPT 模型 → len/4 估算
}

#[test]
fn test_cost_calculation() {
    // input_cost=30, output_cost=60 (per 1M tokens)
    // 150 prompt + 80 completion → (150*30 + 80*60) / 1_000_000
}
```

- [ ] **Step 2: 实现 TokenCounter**

```rust
// crates/kong-ai/src/token/counter.rs
pub struct TokenCounter {
    // tiktoken-rs CoreBPE 缓存（lazy init）
}

impl TokenCounter {
    pub fn count(&self, model: &str, text: &str, provider_usage: Option<u64>) -> u64 {
        provider_usage
            .or_else(|| self.count_tiktoken(model, text))
            .unwrap_or_else(|| Self::count_estimate(text))
    }

    fn count_tiktoken(&self, model: &str, text: &str) -> Option<u64> {
        // tiktoken_rs::get_bpe_from_model(model) → encode → len
    }

    fn count_estimate(text: &str) -> u64 {
        (text.len() as u64 + 3) / 4  // ~4 chars per token
    }
}
```

- [ ] **Step 3: 实现 CostCalculator**

```rust
// crates/kong-ai/src/token/cost.rs
pub fn calculate_cost(usage: &TokenUsage, input_cost: Option<f64>, output_cost: Option<f64>) -> f64 {
    let prompt = usage.prompt_tokens.unwrap_or(0) as f64;
    let completion = usage.completion_tokens.unwrap_or(0) as f64;
    let ic = input_cost.unwrap_or(0.0);
    let oc = output_cost.unwrap_or(0.0);
    (prompt * ic + completion * oc) / 1_000_000.0
}
```

- [ ] **Step 4: 在 ai-proxy log 阶段序列化 analytics**

在 `ctx.log_serialize` 中注入 AI 相关字段：

```json
{
  "ai": {
    "proxy": { "provider": "openai", "model": "gpt-4", "route_type": "llm/v1/chat", "stream": true },
    "usage": { "prompt_tokens": 150, "completion_tokens": 80, "total_tokens": 230, "cost": 0.00069 },
    "latency": { "e2e_ms": 1250, "ttft_ms": 320 }
  }
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p kong-ai -- token`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/kong-ai/src/token/ crates/kong-ai/src/plugins/
git commit -m "feat: Token 计数（三级 fallback）+ 成本计算 + AI analytics 日志
add TokenCounter (provider > tiktoken > estimate), CostCalculator, AI log serialization"
```

---

## Task 11: Admin API — AI 实体 CRUD

**Files:**
- Create: `crates/kong-admin/src/handlers/ai_providers.rs`
- Create: `crates/kong-admin/src/handlers/ai_models.rs`
- Create: `crates/kong-admin/src/handlers/ai_virtual_keys.rs`
- Modify: `crates/kong-admin/src/lib.rs` (AdminState 新增 AI DAO + 路由注册)
- Modify: `crates/kong-admin/Cargo.toml` (添加 kong-ai 依赖)
- Test: `crates/kong-admin/tests/ai_admin_api_test.rs`

- [ ] **Step 1: 在 AdminState 中添加 AI DAO 字段**

```rust
// 在 AdminState struct 中添加
pub ai_provider_dao: Arc<dyn AiProviderDao>,
pub ai_model_dao: Arc<dyn AiModelDao>,
pub ai_virtual_key_dao: Arc<dyn AiVirtualKeyDao>,
```

- [ ] **Step 2: 实现 ai_providers handler**

参考现有的 `handlers/services.rs` 模式，实现：
- `list_ai_providers`: GET /ai-providers (分页)
- `create_ai_provider`: POST /ai-providers
- `get_ai_provider`: GET /ai-providers/{id_or_name}
- `update_ai_provider`: PATCH /ai-providers/{id_or_name}
- `replace_ai_provider`: PUT /ai-providers/{id_or_name}
- `delete_ai_provider`: DELETE /ai-providers/{id_or_name}

**安全**：GET 响应中 mask `auth_config.header_value` 为 `"***"`

- [ ] **Step 3: 实现 ai_models handler**

- `list_ai_models`: GET /ai-models (分页, 支持 ?name= 过滤)
- `create_ai_model`: POST /ai-models
- `get_ai_model`: GET /ai-models/{id}
- `update/replace/delete`: 标准 CRUD
- `list_models_by_provider`: GET /ai-providers/{id}/ai-models

- [ ] **Step 4: 实现 ai_virtual_keys handler**

- `create_ai_virtual_key`: POST /ai-virtual-keys → 生成 raw_key，返回一次性明文，存 SHA-256 hash
- `get_ai_virtual_key`: GET /ai-virtual-keys/{id_or_name} → 不返回 key_hash
- `rotate_key`: POST /ai-virtual-keys/{id}/rotate
- `get_usage`: GET /ai-virtual-keys/{id}/usage

Key 生成逻辑：
```rust
let raw_key = format!("sk-kr-{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
let key_hash = sha256_hex(&raw_key);
let key_prefix = &raw_key[..8];
```

- [ ] **Step 5: 注册路由**

在 `crates/kong-admin/src/lib.rs` 的 router 构建中添加：
```rust
.route("/ai-providers", get(list_ai_providers).post(create_ai_provider))
.route("/ai-providers/:id_or_name", get(get_ai_provider).patch(update_ai_provider).put(replace_ai_provider).delete(delete_ai_provider))
.route("/ai-providers/:id/ai-models", get(list_models_by_provider))
.route("/ai-models", get(list_ai_models).post(create_ai_model))
.route("/ai-models/:id", get(get_ai_model).patch(update_ai_model).put(replace_ai_model).delete(delete_ai_model))
.route("/ai-virtual-keys", get(list_ai_virtual_keys).post(create_ai_virtual_key))
.route("/ai-virtual-keys/:id_or_name", get(get_ai_virtual_key).patch(update_ai_virtual_key).delete(delete_ai_virtual_key))
.route("/ai-virtual-keys/:id/rotate", post(rotate_key))
.route("/ai-virtual-keys/:id/usage", get(get_key_usage))
```

- [ ] **Step 6: 编写 Admin API 测试**

测试覆盖：
- Provider CRUD + auth_config masking
- Model CRUD + model group 查询 (?name=)
- Virtual Key 创建（返回明文）+ 查询（不返回 hash）+ 轮换
- 外键约束：删除 provider → cascade 删除 models
- 409 唯一约束冲突

- [ ] **Step 7: 运行测试**

Run: `cargo test -p kong-admin -- ai_admin`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/kong-admin/ crates/kong-ai/
git commit -m "feat: Admin API — AI Provider/Model/VirtualKey CRUD
add /ai-providers, /ai-models, /ai-virtual-keys endpoints with auth masking and key rotation"
```

---

## Task 12: ai-rate-limit 插件 (Virtual Key + TPM/RPM)

**Files:**
- Create: `crates/kong-ai/src/ratelimit/mod.rs`
- Create: `crates/kong-ai/src/ratelimit/memory.rs`
- Create: `crates/kong-ai/src/plugins/ai_rate_limit.rs`
- Modify: `crates/kong-ai/src/plugins/mod.rs`
- Modify: `crates/kong-server/src/main.rs` (注册 ai-rate-limit)
- Test: `crates/kong-ai/tests/ratelimit_test.rs`

- [ ] **Step 1: 定义 RateLimiter trait + 内存实现**

```rust
// crates/kong-ai/src/ratelimit/mod.rs
pub mod memory;

#[async_trait::async_trait]
pub trait RateLimiter: Send + Sync {
    /// 检查是否超限，返回 (allowed, current_count)
    async fn check(&self, key: &str, limit: u64) -> (bool, u64);
    /// 增加计数
    async fn increment(&self, key: &str, amount: u64);
}
```

```rust
// crates/kong-ai/src/ratelimit/memory.rs
use dashmap::DashMap;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

pub struct MemoryRateLimiter {
    windows: DashMap<String, WindowEntry>,
    window_duration: Duration,
}

struct WindowEntry {
    start: Instant,
    count: AtomicU64,
}
```

- [ ] **Step 2: 编写限流测试**

```rust
#[tokio::test]
async fn test_rpm_limit_blocks_excess() {
    let limiter = MemoryRateLimiter::new(Duration::from_secs(60));
    for _ in 0..10 {
        let (allowed, _) = limiter.check("test-key:rpm", 10).await;
        assert!(allowed);
        limiter.increment("test-key:rpm", 1).await;
    }
    let (allowed, _) = limiter.check("test-key:rpm", 10).await;
    assert!(!allowed);  // 第 11 次被拒
}

#[tokio::test]
async fn test_tpm_prededuct_and_correction() {
    // access 阶段预扣 estimated_prompt_tokens
    // log 阶段修正 (actual - estimated)
}
```

- [ ] **Step 3: 实现 AiRateLimitPlugin**

```rust
// crates/kong-ai/src/plugins/ai_rate_limit.rs
pub struct AiRateLimitPlugin {
    limiter: Arc<dyn RateLimiter>,
    virtual_key_dao: Arc<dyn AiVirtualKeyDao>,
}

impl PluginHandler for AiRateLimitPlugin {
    fn name(&self) -> &str { "ai-rate-limit" }
    fn priority(&self) -> i32 { 771 }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 从 header 提取 virtual key → SHA-256 → 查 DAO
        // 2. 验证 enabled, expires_at, allowed_models
        // 3. RPM check → 超限 429
        // 4. TPM check → 超限 429
        // 5. RPM +1, TPM + estimated_prompt_tokens (预扣)
        // 6. 存 VirtualKeyContext 到 ctx.extensions
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 读 AiRequestState.usage (由 ai-proxy 填充)
        // 2. TPM 修正: increment(actual_tokens - estimated)
        // 3. 异步更新 budget_used
    }
}
```

- [ ] **Step 4: 注册插件 + 运行测试**

Run: `cargo test -p kong-ai -- ratelimit`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/src/ratelimit/ crates/kong-ai/src/plugins/ai_rate_limit.rs crates/kong-server/
git commit -m "feat: ai-rate-limit 插件 (Virtual Key + TPM/RPM 内存限流)
add ai-rate-limit plugin with virtual key auth, RPM/TPM fixed window rate limiting, and prompt token pre-deduction"
```

---

## Task 13: ai-cache 插件 (Redis KV + 语义缓存)

**Files:**
- Create: `crates/kong-ai/src/cache/mod.rs`
- Create: `crates/kong-ai/src/cache/redis_kv.rs`
- Create: `crates/kong-ai/src/cache/semantic.rs`
- Create: `crates/kong-ai/src/plugins/ai_cache.rs`
- Test: `crates/kong-ai/tests/cache_test.rs`

- [ ] **Step 1: 定义 CacheProvider trait**

```rust
// crates/kong-ai/src/cache/mod.rs
#[async_trait::async_trait]
pub trait CacheProvider: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()>;
}

#[async_trait::async_trait]
pub trait SemanticSearch: Send + Sync {
    async fn search(&self, embedding: &[f32], threshold: f32) -> Result<Option<(String, f32)>>;
    async fn upsert(&self, key: &str, embedding: &[f32]) -> Result<()>;
}
```

- [ ] **Step 2: 实现 Redis KV 缓存**

`crates/kong-ai/src/cache/redis_kv.rs` — 使用 `redis` crate 的 async 连接。
Redis 为可选依赖，编译时通过 feature flag 控制。

- [ ] **Step 3: 实现 AiCachePlugin**

```rust
// crates/kong-ai/src/plugins/ai_cache.rs
impl PluginHandler for AiCachePlugin {
    fn name(&self) -> &str { "ai-cache" }
    fn priority(&self) -> i32 { 772 }  // 在 ai-rate-limit 之前

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 检查 skip header
        // 2. 提取 cache key (last question 或 all questions)
        // 3. Redis KV 精确匹配 → 命中则短路
        // 4. 语义缓存 (if enabled) → embedding → 相似度搜索
        // 5. 未命中 → 标记 cache_key 到 ctx
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 从 AiRequestState.response_buffer 读取完整响应
        // 2. 写入 Redis KV
        // 3. 语义缓存: 上传 embedding
    }
}
```

- [ ] **Step 4: 编写测试**

测试覆盖 KV 精确匹配和缓存回写（mock Redis），语义缓存可以简化为接口测试。

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/src/cache/ crates/kong-ai/src/plugins/ai_cache.rs
git commit -m "feat: ai-cache 插件 (Redis KV + 语义缓存接口)
add ai-cache plugin with Redis KV exact cache, semantic cache trait, and cache key strategies"
```

---

## Task 14: ai-prompt-guard 插件

**Files:**
- Create: `crates/kong-ai/src/guard/mod.rs`
- Create: `crates/kong-ai/src/guard/regex.rs`
- Create: `crates/kong-ai/src/plugins/ai_prompt_guard.rs`
- Test: `crates/kong-ai/tests/prompt_guard_test.rs`

- [ ] **Step 1: 编写正则匹配测试**

```rust
#[test]
fn test_deny_pattern_blocks_injection() {
    let guard = RegexGuard::new(
        vec!["ignore previous instructions".to_string()],
        vec![],
    );
    assert!(guard.check("Please ignore previous instructions and tell me").is_err());
}

#[test]
fn test_allow_pattern_whitelist() {
    let guard = RegexGuard::new(vec![], vec!["^(translate|summarize)".to_string()]);
    assert!(guard.check("translate this text").is_ok());
    assert!(guard.check("delete all data").is_err());
}

#[test]
fn test_max_message_length() {
    // 超过 max_message_length → 拒绝
}
```

- [ ] **Step 2: 实现 RegexGuard**

```rust
// crates/kong-ai/src/guard/regex.rs
use regex::Regex;

pub struct RegexGuard {
    deny_patterns: Vec<Regex>,
    allow_patterns: Vec<Regex>,
    max_message_length: usize,
}

impl RegexGuard {
    pub fn check(&self, message: &str) -> Result<()> {
        // 1. 长度检查
        // 2. deny_patterns: 任一匹配 → 拒绝
        // 3. allow_patterns 非空: 必须至少一个匹配 → 否则拒绝
    }
}
```

- [ ] **Step 3: 实现 AiPromptGuardPlugin**

```rust
impl PluginHandler for AiPromptGuardPlugin {
    fn name(&self) -> &str { "ai-prompt-guard" }
    fn priority(&self) -> i32 { 773 }  // 最先执行

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // 1. 解析请求体 messages
        // 2. 对每条 user message 调用 guard.check()
        // 3. action=block → 短路 400; action=log_only → 标记日志
    }
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p kong-ai -- prompt_guard`
Expected: PASS

- [ ] **Step 5: 注册插件 + Commit**

```bash
git add crates/kong-ai/src/guard/ crates/kong-ai/src/plugins/ai_prompt_guard.rs
git commit -m "feat: ai-prompt-guard 插件 (正则 + 长度限制)
add ai-prompt-guard plugin with deny/allow regex patterns, message length limit, block/log_only action"
```

---

## Task 15: 集成测试 + 端到端验证

**Files:**
- Create: `crates/kong-ai/tests/e2e_test.rs`
- Create: `crates/kong-ai/tests/helpers/mock_llm_server.rs` (mock LLM provider)

- [ ] **Step 1: 创建 Mock LLM Server**

用 axum 创建一个 mock server，模拟 OpenAI / Anthropic API：
- `POST /v1/chat/completions` → 返回固定 JSON 响应 (含 usage)
- `POST /v1/chat/completions` stream=true → 返回 SSE 流
- `POST /v1/messages` → 返回 Anthropic 格式响应
- 可以设置延迟、错误码

- [ ] **Step 2: 编写端到端测试**

```rust
#[tokio::test]
async fn test_e2e_openai_nonstreaming() {
    // 1. 启动 mock LLM server
    // 2. 创建 AiProviderConfig 指向 mock server
    // 3. 创建 AiModel
    // 4. 创建 Route + 挂载 ai-proxy 插件
    // 5. 发送 OpenAI chat request
    // 6. 验证响应格式正确, usage 正确
}

#[tokio::test]
async fn test_e2e_openai_streaming() {
    // 同上，stream=true，验证 SSE 格式
}

#[tokio::test]
async fn test_e2e_claude_protocol() {
    // 客户端发 Claude Messages 格式
    // 验证返回 Anthropic 格式响应
}

#[tokio::test]
async fn test_e2e_model_group_failover() {
    // 2 个 model: primary (mock 返回 500) + fallback (mock 返回 200)
    // 验证自动 failover
}

#[tokio::test]
async fn test_e2e_rate_limit() {
    // 设置 RPM=2, 发 3 个请求
    // 验证第 3 个返回 429
}

#[tokio::test]
async fn test_e2e_prompt_guard_blocks() {
    // 发送含 "ignore previous instructions" 的请求
    // 验证返回 400
}

#[tokio::test]
async fn test_e2e_full_pipeline() {
    // 完整插件链: guard → cache → rate-limit → proxy
    // 第一次请求: cache miss → 代理到 mock → 返回
    // 第二次请求: cache hit → 直接返回
}
```

- [ ] **Step 3: 运行全部测试**

Run: `cargo test -p kong-ai`
Expected: ALL PASS

- [ ] **Step 4: 运行整个 workspace 测试确认无回归**

Run: `cargo test --workspace`
Expected: 原有测试不受影响，新增测试全部通过

- [ ] **Step 5: Commit**

```bash
git add crates/kong-ai/tests/
git commit -m "test: AI Gateway 端到端集成测试
add e2e tests covering OpenAI/Anthropic proxy, streaming, model group failover, rate limiting, prompt guard, and full pipeline"
```

---

## 执行注意事项

1. **Task 3 和 Task 4 执行顺序**：Task 3 的 provider driver 依赖 codec 类型（ChatRequest/ChatResponse/SseEvent）。执行时必须先实现 Task 4 的 codec 类型定义（Step 1），再实现 Task 3 的 driver。或者合并为一个 Task：先定义 codec 类型 → SSE 解析器 → Provider trait → OpenAI driver → Anthropic codec。
2. **DAO 模式**：AI 实体必须复用现有 `Dao<T: Entity>` + `PgDao<T>` 模式（Task 2 已说明）。`AdminState` 中使用 `Arc<dyn Dao<AiProviderConfig>>` 等标准字段。
3. **类型一致性**：`ProviderRequest.body` 和 `transform_response` 的 body 参数必须是 `String`/`&str`（不是 `Bytes`），因为 `RequestCtx.upstream_body` 是 `Option<String>`。`transform_response` 的 headers 参数使用 `&HashMap<String, String>`（与 `ctx.response_headers` 一致）。
4. **Redis 依赖**是可选的（feature flag）。Task 12 的内存限流器不需要 Redis。Task 13 的 Redis KV 缓存需要 Redis feature。
5. **测试策略**：单元测试不依赖外部服务（PostgreSQL/Redis）。集成测试（Task 15）使用 mock server。需要真实 DB 的测试标记 `#[ignore]`。cache 相关 e2e 测试使用 mock cache provider。
6. **每个 Task 完成后都应该能编译通过**（`cargo check --workspace`）。
7. **Balancer 中的 Mutex**：`ModelHealth.cooldown_until` 使用 `std::sync::Mutex`（不需要 parking_lot，保护一个 `Option<Instant>` 标准库 Mutex 足够）。
8. **ai-rate-limit TPM 预扣**：ai-rate-limit (priority 771) 先于 ai-proxy (770) 执行。TPM 预扣需要独立解析 `ctx.request_body` 估算 prompt_tokens，这是有意的双重解析（简单估算 vs ai-proxy 的完整解析），成本可接受。
9. **budget_used 精度**：使用 `f64` 映射 SQL `NUMERIC`，对 LLM cost tracking 精度足够（不是金融级计费）。
10. **Deferred 功能**：Admin API analytics 端点（`/ai/analytics/*`）和 Prometheus 指标（`kong_ai_*`）暂不在本计划范围内，后续迭代补充。Migration 编号根据执行时的实际顺序调整。
