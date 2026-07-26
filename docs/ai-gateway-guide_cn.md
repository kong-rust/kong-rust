# Kong-Rust AI Gateway 使用指南

本文档是 Kong-Rust AI Gateway 的用户面向使用指南，涵盖快速开始、插件配置参考、Admin API 以及常见部署模式。

---

## 目录

1. [快速开始](#1-快速开始)
2. [核心概念](#2-核心概念)
3. [插件配置参考](#3-插件配置参考)
4. [Admin API 参考](#4-admin-api-参考)
5. [多 Provider 负载均衡](#5-多-provider-负载均衡)
6. [双协议支持](#6-双协议支持)
7. [插件组合示例](#7-插件组合示例)
8. [智能模型路由](#8-智能模型路由)
9. [支持的 Provider](#9-支持的-provider)
10. [精确 prompt-token 计数](#10-精确-prompt-token-计数tokenizer-registry)
11. [调用统计与成本估算](#11-调用统计与成本估算)

---

## 1. 快速开始

以下演示最简路径：通过 ai-proxy 插件把一条 Route 接入 OpenAI。

### 1.1 创建 AI Provider

AI Provider 存储 LLM 服务商的连接参数和鉴权凭证。

```bash
curl -s -X POST http://localhost:8001/ai-providers \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "openai-prod",
    "provider_type": "openai",
    "auth_config": {
      "header_name": "Authorization",
      "header_value": "Bearer sk-..."
    }
  }'
```

响应示例（`auth_config` 中的敏感字段被自动脱敏为 `***`）：

```json
{
  "id": "a1b2c3d4-...",
  "name": "openai-prod",
  "provider_type": "openai",
  "auth_config": {
    "header_name": "Authorization",
    "header_value": "***"
  },
  "enabled": true
}
```

### 1.2 创建 AI Model

AI Model 描述使用哪个 Provider 的哪个具体模型。`name` 是逻辑名称（可被多个 Model 共用以构成 Model Group），`model_name` 是发往 Provider 的实际模型标识符。

```bash
PROVIDER_ID="a1b2c3d4-..."   # 上一步返回的 id

curl -s -X POST http://localhost:8001/ai-models \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"gpt-4o\",
    \"provider_id\": \"${PROVIDER_ID}\",
    \"model_name\": \"gpt-4o\",
    \"priority\": 1,
    \"weight\": 100
  }"
```

### 1.3 创建 Route

```bash
curl -s -X POST http://localhost:8001/routes \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-chat",
    "paths": ["/v1/chat/completions"],
    "methods": ["POST"],
    "strip_path": false,
    "response_buffering": false
  }'
```

### 1.4 为 Route 挂载 ai-proxy 插件

```bash
ROUTE_ID="<上一步返回的 route id>"

curl -s -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"ai-proxy\",
    \"route\": {\"id\": \"${ROUTE_ID}\"},
    \"config\": {
      \"model_group\": \"gpt-4o\",
      \"model_source\": \"config\",
      \"route_type\": \"llm/v1/chat\",
      \"client_protocol\": \"openai\"
    }
  }"
```

> **注意**：`model_group` 会解析上面创建的 AI Model 实体，并从服务端 AI Provider 记录读取凭证。Kong 官方兼容的 `model` record 仍可用于内联 Provider 配置。

### 1.5 发送请求

```bash
curl -s -X POST http://localhost:8000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-4o",
    "messages": [
      {"role": "user", "content": "Hello, who are you?"}
    ]
  }'
```

响应符合 OpenAI Chat Completions API 格式，同时响应头 `X-Kong-LLM-Model` 会注明实际使用的模型。

---

## 2. 核心概念

### AI Provider

AI Provider 代表一个 LLM 服务商的连接配置，包含：

- **provider_type**：服务商类型标识，决定使用哪个内置驱动（`openai` / `anthropic` / `gemini` / `openai_compat`）
- **auth_config**：鉴权参数（API Key、HTTP Header 名、Query 参数等）
- **endpoint_url**：自定义上游地址（默认使用各 Provider 的公网地址）

Admin API 在读取 Provider 时会自动遮蔽 `auth_config` 中的敏感字段（`header_value`、`param_value` 等）。

### AI Model / Model Group

AI Model 是"逻辑模型"到"物理 Provider 模型"的映射：

- **name**：逻辑名称。**多个 AI Model 可以共用同一个 `name`**，构成一个 Model Group，用于负载均衡（按 `weight`）和故障切换（按 `priority`，值小者优先）。
- **model_name**：发往 Provider 的实际模型标识符（如 `gpt-4o`、`claude-3-5-sonnet-20241022`）。
- **priority / weight**：控制 Model Group 内的路由策略（优先级 + 加权）。

### AI Virtual Key

AI Virtual Key 是一种面向用户/团队的虚拟 API Key，用于：

- 代理流量认证（挂载 `ai-key-auth` 插件后生效，见 [3.5](#35-ai-key-auth)）
- 允许访问的模型白名单（`allowed_models`，已生效）
- 细粒度的 TPM / RPM 配额控制（`tpm_limit` / `rpm_limit`，当前仅存储，尚未生效）
- 预算上限（`budget_limit`）和使用量追踪（`budget_used`，当前仅存储，尚未生效）

Virtual Key 格式为 `sk-kr-<uuid32>`，创建时一次性返回原始密钥，此后只存储 SHA256 哈希。

### 五个插件及优先级

插件按优先级从高到低执行（数字大者先执行）：

| 插件 | 优先级 | 职责 |
|---|---|---|
| ai-key-auth | 774 | 认证：虚拟密钥校验、模型白名单、身份注入 |
| ai-prompt-guard | 773 | 安全检查：拒绝/允许模式匹配、消息长度限制 |
| ai-cache | 772 | 语义缓存：计算缓存键、命中时短路 |
| ai-rate-limit | 771 | 限流：RPM / TPM 计数、预扣修正 |
| ai-proxy | 770 | 核心代理：协议转换、上游路由、token 统计 |

---

## 3. 插件配置参考

### 3.1 ai-proxy

核心插件，负责将客户端的 OpenAI / Anthropic 格式请求转换为目标 Provider 的协议，发送请求并转换响应。

#### 配置字段

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `model` | object | `null` | Kong 官方模型 record（`provider`、可选 `name` 和 `options`），用于内联 Provider 配置 |
| `model_group` | string | `null` | Kong-Rust 扩展：从 AI Model 与 AI Provider 实体解析的逻辑模型组名称 |
| `model_source` | string | `"config"` | 模型来源：`config`（从插件配置取）或 `request`（从请求体 `model` 字段取） |
| `route_type` | string | `"llm/v1/chat"` | 路由类型：`llm/v1/chat` 或 `llm/v1/completions` |
| `client_protocol` | string | `"openai"` | 客户端协议：`openai` 或 `anthropic` |
| `response_streaming` | string | `"allow"` | 流式策略：`allow`（尊重客户端请求）/ `deny`（强制非流式）/ `always`（强制流式）|
| `max_request_body_size` | integer | `128` | 最大请求体大小（KB），超出返回 413 |
| `model_name_header` | boolean | `true` | 是否在响应头 `X-Kong-LLM-Model` 中返回实际模型名 |
| `timeout` | integer | `60000` | 上游超时（毫秒） |
| `retries` | integer | `1` | 上游重试次数 |
| `log_payloads` | boolean | `false` | 是否记录请求/响应体（调试用） |
| `log_statistics` | boolean | `true` | 是否在日志中记录 token 统计 |
| `model_routes` | array | `[]` | 智能路由规则（正则匹配 + 加权选择，见下方"智能路由"章节） |
| `auth` | object | `null` | 与 `model.provider` 搭配使用的 Kong 官方认证 record |
| `provider` | object | `null` | 旧版 Kong-Rust 内联 Provider 配置（见下方）；使用 `model_group` 或 `model_routes` 时可省略 |

非空 `model_group` 会优先于内联 `model` / `provider` 字段，Provider 选择与凭证统一来自服务端 AI 实体。页面新建的配置统一使用 `model_group`。

#### 内联 Provider 配置（`provider` 字段）

| 字段 | 类型 | 说明 |
|---|---|---|
| `provider_type` | string | Provider 类型：`openai` / `anthropic` / `gemini` / `openai_compat` |
| `auth_config` | object | 鉴权参数，格式与 AI Provider 实体的 `auth_config` 相同 |
| `endpoint_url` | string | 可选，自定义上游 URL（覆盖 Provider 默认地址） |

#### 示例配置

**OpenAI 标准接入：**

```json
{
  "model": {
    "provider": "openai",
    "name": "gpt-4o"
  },
  "model_source": "config",
  "route_type": "llm/v1/chat",
  "client_protocol": "openai",
  "response_streaming": "allow",
  "timeout": 30000,
  "log_statistics": true,
  "auth": {
    "header_name": "Authorization",
    "header_value": "Bearer sk-..."
  }
}
```

**允许客户端自选模型（`model_source=request`）：**

```json
{
  "model_source": "request",
  "route_type": "llm/v1/chat",
  "client_protocol": "openai",
  "provider": {
    "provider_type": "openai",
    "auth_config": {
      "header_name": "Authorization",
      "header_value": "Bearer sk-..."
    }
  }
}
```

客户端在请求体中指定 `"model": "gpt-4o-mini"` 即可，网关会透传该模型名到 OpenAI。

---

### 3.2 ai-rate-limit

对 AI 请求实施 RPM（每分钟请求数）和 TPM（每分钟 Token 数）限流。采用滑动窗口（60 秒），TPM 使用预扣 + 修正机制，保证计量准确。

#### 配置字段

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `limit_by` | string | `"consumer"` | 限流维度：`consumer` / `route` / `global` / `virtual_key` |
| `tpm_limit` | integer | `null` | Token Per Minute 上限，`null` 表示不限 |
| `rpm_limit` | integer | `null` | Request Per Minute 上限，`null` 表示不限 |
| `header_name` | string | `"X-AI-Key"` | 读取 Virtual Key 的请求头名称（`limit_by=virtual_key` 时生效） |
| `error_code` | integer | `429` | 超限时返回的 HTTP 状态码 |
| `error_message` | string | `"AI rate limit exceeded"` | 超限时返回的错误消息 |

#### 示例配置

**按 Route 限流，每分钟最多 100 次请求、10 万 Token：**

```json
{
  "limit_by": "route",
  "rpm_limit": 100,
  "tpm_limit": 100000,
  "error_code": 429,
  "error_message": "Too many requests, please slow down"
}
```

**按 Consumer 限流（不限 RPM，仅限 TPM）：**

```json
{
  "limit_by": "consumer",
  "tpm_limit": 50000
}
```

---

### 3.3 ai-cache

对相同问题的 AI 请求进行缓存，降低 LLM 调用成本。当前版本实现了缓存键计算基础设施（SHA256），Redis 后端集成在后续版本提供。

#### 配置字段

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `cache_ttl` | integer | `300` | 缓存 TTL（秒） |
| `cache_key_strategy` | string | `"last_question"` | 缓存键策略：`last_question`（仅最后一条 user 消息）/ `all_questions`（所有 user 消息拼接）|
| `skip_header` | string | `"X-AI-Skip-Cache"` | 客户端发送此 Header 时跳过缓存查找 |

#### 两种缓存键策略对比

| 策略 | 适用场景 | 说明 |
|---|---|---|
| `last_question` | 单轮问答、FAQ 场景 | 仅取最后一条 `role=user` 消息的内容做 SHA256 |
| `all_questions` | 多轮对话、上下文敏感场景 | 将所有 `role=user` 消息拼接后做 SHA256 |

#### 示例配置

```json
{
  "cache_ttl": 600,
  "cache_key_strategy": "last_question",
  "skip_header": "X-AI-Skip-Cache"
}
```

客户端强制绕过缓存：

```bash
curl -X POST http://localhost:8000/v1/chat/completions \
  -H 'X-AI-Skip-Cache: true' \
  -H 'Content-Type: application/json' \
  -d '{"messages": [{"role": "user", "content": "..."}]}'
```

---

### 3.4 ai-prompt-guard

对用户输入（`role=user` 的消息）进行安全审查，支持拒绝模式（黑名单）、允许模式（白名单）和消息长度限制。

#### 配置字段

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `deny_patterns` | string[] | `[]` | 拒绝模式列表（正则表达式），匹配任意一条则触发 |
| `allow_patterns` | string[] | `[]` | 允许模式列表（正则表达式），配置后消息必须匹配至少一条，否则触发 |
| `max_message_length` | integer | `32768` | 单条消息最大字节数 |
| `action` | string | `"block"` | 触发后行为：`block`（拦截请求）/ `log_only`（仅记录日志，不拦截）|
| `error_code` | integer | `400` | 拦截时返回的 HTTP 状态码 |
| `error_message` | string | `"request blocked by ai-prompt-guard"` | 拦截时返回的错误消息 |

> **注意**：`deny_patterns` 和 `allow_patterns` 同时配置时，先执行 deny 检查，再执行 allow 检查（必须通过两者）。`allow_patterns` 为空时白名单逻辑不生效。

#### 示例配置

**屏蔽敏感话题，限制消息长度：**

```json
{
  "deny_patterns": [
    "(?i)(password|secret|api.?key)",
    "(?i)(hack|exploit|injection)",
    "忽略.*前面.*指令"
  ],
  "max_message_length": 4096,
  "action": "block",
  "error_code": 400,
  "error_message": "Your request contains prohibited content"
}
```

**白名单模式（只允许特定主题）：**

```json
{
  "allow_patterns": [
    "(?i)(product|service|support|help)",
    "(?i)(how to|what is|explain)"
  ],
  "action": "block",
  "error_message": "Only product-related questions are supported"
}
```

**审计模式（仅记录，不拦截）：**

```json
{
  "deny_patterns": ["(?i)(competitor|alternative)"],
  "action": "log_only"
}
```

---

### 3.5 ai-key-auth

用 AI Virtual Key 对代理流量做认证，并校验密钥的模型白名单。优先级 774，先于其他所有 AI 插件执行，因此下游插件（限流、日志、用量）可以读到已认证的身份。

未挂载该插件的 Route 行为完全不变（不需要密钥）。

#### 配置字段

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `key_header` | string | `"X-AI-Key"` | 兜底凭证头名称，在 `Authorization` 与 `x-api-key` 之后尝试 |
| `error_format` | string | `"auto"` | 错误体风格：`auto`（按客户端协议自适应）/ `openai` / `anthropic` |

#### 凭证携带方式

插件按以下顺序读取凭证，取到第一个即使用：

1. `Authorization: Bearer <key>` — OpenAI SDK 默认方式（scheme 大小写不敏感；非 Bearer 的 Authorization 如 Basic 会被跳过）
2. `x-api-key: <key>` — Anthropic SDK 默认方式
3. `key_header` 指定的自定义头（默认 `X-AI-Key`）

因此 OpenAI 与 Anthropic 官方 SDK 都无需改动即可直接使用。

#### 校验规则与响应

| 场景 | 状态码 | 错误 code |
|---|---|---|
| 未携带凭证 | 401 | `missing_api_key` |
| 密钥不存在 / `enabled=false` / 已过 `expires_at` | 401 | `invalid_api_key` |
| 请求 `model` 不在 `allowed_models` 内 | 403 | `model_not_allowed` |

> **安全设计**：密钥不存在、已禁用、已过期三种情况返回完全相同的响应，避免密钥状态被探测。

`error_format=auto` 的判定顺序：凭证来自 `x-api-key` → Anthropic 风格；请求路径含 `/v1/messages` → Anthropic 风格；否则 OpenAI 风格。

OpenAI 风格错误体：

```json
{"error": {"message": "invalid API key", "type": "invalid_request_error", "code": "invalid_api_key"}}
```

Anthropic 风格错误体：

```json
{"type": "error", "error": {"type": "authentication_error", "message": "invalid API key"}}
```

#### 模型白名单（allowed_models）

- 为空数组或未设置 → 不限制
- 以 `*` 结尾的项按前缀匹配：`gpt-4*` 匹配 `gpt-4`、`gpt-4o`、`gpt-4-turbo`，但不匹配 `gpt-3.5-turbo`
- 其余项精确匹配（大小写敏感）
- 多项为 OR 语义，命中任意一项即放行；单独一个 `*` 等同不限制
- **仅当请求体含 `model` 字段时校验**：`model_source=config` 部署下客户端不传 `model` 属于正常用法，此时跳过白名单检查

#### 身份注入

认证通过后，插件向请求上下文注入：

- `AiAuthContext { virtual_key_id, key_name, consumer_id }` — 供下游 AI 插件读取
- `consumer_id`（密钥绑定 Consumer 时）— 使 `ai-rate-limit` 的 `limit_by=consumer` 与 Consumer 一致性哈希开始生效
- `authenticated_credential` — 进入 access log

#### 示例：挂载到 Route

```bash
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-key-auth",
    "route": {"id": "<ROUTE_ID>"},
    "config": {}
  }'
```

创建密钥并调用：

```bash
# 创建密钥（原始密钥只在此刻返回一次）
curl -X POST http://localhost:8001/ai-virtual-keys \
  -H 'Content-Type: application/json' \
  -d '{"name": "team-a", "allowed_models": ["gpt-4*"]}'

# 携带密钥调用
curl -X POST http://localhost:8000/ai/demo/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer sk-kr-...' \
  -d '{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}'
```

#### 缓存与生效延迟

认证查找带进程内缓存（含无效密钥的负缓存），TTL 1 秒。通过 Admin API 创建、更新、轮换、删除密钥时缓存立即失效，因此这些操作**即时生效**；TTL 只是带外变更（多节点、直接写库）的兜底窗口。

#### DB-less 模式

声明式配置中的 `ai_virtual_keys` 需要自行提供 `key_hash`（Admin API 的 `key_hash` 是服务端生成的，声明式加载不会生成）：

```bash
# 由原始密钥计算 key_hash
printf 'sk-kr-your-key' | shasum -a 256
```

```yaml
ai_virtual_keys:
  - name: team-a
    key_hash: <上面算出的 sha256 十六进制>
    key_prefix: sk-kr-y
    enabled: true
    allowed_models: ["gpt-4*"]
```

> **限制**：Hybrid（CP/DP）模式下 CP→DP 的配置同步当前不包含任何 AI 实体，因此 DP 节点上没有 Virtual Key 数据。

---

## 4. Admin API 参考

所有 AI Gateway 专属端点均以 `/ai-` 前缀开头，基础路径为 Admin API 地址（默认 `http://localhost:8001`）。

### AI Provider

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/ai-providers` | 列出所有 Provider（支持分页、tag 过滤） |
| `POST` | `/ai-providers` | 创建 Provider |
| `GET` | `/ai-providers/{id_or_name}` | 获取单个 Provider |
| `PATCH` | `/ai-providers/{id_or_name}` | 更新 Provider 部分字段 |
| `PUT` | `/ai-providers/{id_or_name}` | 替换（upsert）Provider |
| `DELETE` | `/ai-providers/{id_or_name}` | 删除 Provider |
| `GET` | `/ai-providers/{id}/ai-models` | 列出该 Provider 下的所有 AI Model |

> 所有读取响应中，`auth_config` 的敏感字段（`header_value`、`param_value`、`aws_secret_access_key`、`gcp_service_account_json`）均被替换为 `"***"`。

### AI Model

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/ai-models` | 列出所有 AI Model |
| `POST` | `/ai-models` | 创建 AI Model |
| `GET` | `/ai-models/{id}` | 获取单个 AI Model |
| `PATCH` | `/ai-models/{id}` | 更新 AI Model 部分字段 |
| `PUT` | `/ai-models/{id}` | 替换（upsert）AI Model |
| `DELETE` | `/ai-models/{id}` | 删除 AI Model |
| `GET` | `/ai-model-groups` | 列出所有不同的 Model 逻辑名称（即所有 Model Group） |

### AI Virtual Key

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/ai-virtual-keys` | 列出所有 Virtual Key |
| `POST` | `/ai-virtual-keys` | 创建 Virtual Key（一次性返回原始密钥 `key` 字段） |
| `GET` | `/ai-virtual-keys/{id_or_name}` | 获取单个 Virtual Key |
| `PATCH` | `/ai-virtual-keys/{id_or_name}` | 更新 Virtual Key 配置 |
| `DELETE` | `/ai-virtual-keys/{id_or_name}` | 删除 Virtual Key |
| `POST` | `/ai-virtual-keys/{id}/rotate` | 轮换密钥（生成新密钥，返回新的原始 `key`） |

> **安全说明**：`key_hash` 字段在所有响应中均被移除。原始密钥（`key` 字段）仅在 `POST /ai-virtual-keys` 和 `POST /ai-virtual-keys/{id}/rotate` 的成功响应中出现一次，请妥善保存。

---

## 5. 多 Provider 负载均衡

通过给多个 AI Model 设置相同的 `name`，它们自动构成一个 Model Group，ai-proxy 在路由时按 `priority` + `weight` 选择后端。

### 场景：OpenAI 主力 + Anthropic 备份

**前提**：

- 创建两个 Provider：`openai-prod` 和 `anthropic-prod`
- 创建两个 AI Model，`name` 均为 `gpt4-tier`，分别指向不同 Provider

```bash
# Provider 1 — OpenAI
curl -X POST http://localhost:8001/ai-providers \
  -H 'Content-Type: application/json' \
  -d '{"name": "openai-prod", "provider_type": "openai", "auth_config": {"header_name": "Authorization", "header_value": "Bearer sk-openai-..."}}'

# Provider 2 — Anthropic
curl -X POST http://localhost:8001/ai-providers \
  -H 'Content-Type: application/json' \
  -d '{"name": "anthropic-prod", "provider_type": "anthropic", "auth_config": {"header_name": "x-api-key", "header_value": "sk-ant-..."}}'

# AI Model A — 主力（priority=1，高权重）
OPENAI_ID="<openai provider id>"
curl -X POST http://localhost:8001/ai-models \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"gpt4-tier\",
    \"provider_id\": \"${OPENAI_ID}\",
    \"model_name\": \"gpt-4o\",
    \"priority\": 1,
    \"weight\": 90
  }"

# AI Model B — 同优先级分流目标（priority=1，低权重）
ANTHROPIC_ID="<anthropic provider id>"
curl -X POST http://localhost:8001/ai-models \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"gpt4-tier\",
    \"provider_id\": \"${ANTHROPIC_ID}\",
    \"model_name\": \"claude-3-5-sonnet-20241022\",
    \"priority\": 1,
    \"weight\": 10
  }"
```

在 ai-proxy 插件中引用逻辑名称 `gpt4-tier`：

```json
{
  "model_group": "gpt4-tier",
  "model_source": "config"
}
```

网关将按 `priority` 选择最优 Provider，同 priority 内按 `weight` 交错加权轮转。
`weight` 范围为 `0..=10000`，各模型权重总和无需等于 100；权重只表示相对比例。
如果需要主备而非分流，请为备份模型设置更低的 `priority`。

### 查看 Model Group

```bash
curl http://localhost:8001/ai-model-groups
# 返回所有不同的 name，即所有 Model Group 列表
```

---

## 6. 双协议支持

Kong-Rust AI Gateway 支持同时暴露两种客户端协议：

- **OpenAI 协议**（`client_protocol=openai`）：客户端使用 `POST /v1/chat/completions` 格式
- **Anthropic 协议**（`client_protocol=anthropic`）：客户端使用 `POST /v1/messages` 格式

无论客户端使用哪种协议，网关内部统一转换为目标 Provider 的格式。

### 示例：同一后端，两条路由，两种协议

**Route 1 — OpenAI 协议入口：**

```bash
# 创建路由
curl -X POST http://localhost:8001/routes \
  -H 'Content-Type: application/json' \
  -d '{"name": "ai-openai", "paths": ["/v1/chat/completions"], "methods": ["POST"], "strip_path": false}'

# 挂载 ai-proxy，client_protocol=openai
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-proxy",
    "route": {"name": "ai-openai"},
    "config": {
      "model": {"provider": "anthropic", "name": "claude-main"},
      "client_protocol": "openai",
      "auth": {"header_name": "x-api-key", "header_value": "sk-ant-..."}
    }
  }'
```

**Route 2 — Anthropic 协议入口：**

```bash
# 创建路由
curl -X POST http://localhost:8001/routes \
  -H 'Content-Type: application/json' \
  -d '{"name": "ai-anthropic", "paths": ["/v1/messages"], "methods": ["POST"], "strip_path": false}'

# 挂载 ai-proxy，client_protocol=anthropic
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-proxy",
    "route": {"name": "ai-anthropic"},
    "config": {
      "model": {"provider": "anthropic", "name": "claude-main"},
      "client_protocol": "anthropic",
      "auth": {"header_name": "x-api-key", "header_value": "sk-ant-..."}
    }
  }'
```

**Anthropic 协议请求示例：**

```bash
curl -X POST http://localhost:8000/v1/messages \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "claude-3-5-sonnet-20241022",
    "max_tokens": 1024,
    "messages": [
      {"role": "user", "content": "Hello, Claude!"}
    ]
  }'
```

---

## 7. 插件组合示例

下面展示一个完整的生产级配置，将全部 4 个 AI 插件组合使用。

### 目标

- **ai-prompt-guard**：屏蔽敏感词，限制消息长度（安全第一）
- **ai-cache**：对相同问题缓存 5 分钟（降低成本）
- **ai-rate-limit**：每个 Consumer 每分钟最多 60 次请求、6 万 Token（配额管理）
- **ai-proxy**：路由到 OpenAI gpt-4o（核心代理）

### 步骤

**第一步**：创建 Route

```bash
curl -X POST http://localhost:8001/routes \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-full-stack",
    "paths": ["/ai/chat"],
    "methods": ["POST"],
    "strip_path": false
  }'
```

**第二步**：挂载 ai-prompt-guard（优先级 773，最先执行）

```bash
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-prompt-guard",
    "route": {"name": "ai-full-stack"},
    "config": {
      "deny_patterns": [
        "(?i)(password|secret|api.?key|token)",
        "(?i)(ignore.*previous.*instruction|jailbreak)"
      ],
      "max_message_length": 8192,
      "action": "block",
      "error_message": "Request blocked for security reasons"
    }
  }'
```

**第三步**：挂载 ai-cache（优先级 772）

```bash
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-cache",
    "route": {"name": "ai-full-stack"},
    "config": {
      "cache_ttl": 300,
      "cache_key_strategy": "last_question"
    }
  }'
```

**第四步**：挂载 ai-rate-limit（优先级 771）

```bash
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-rate-limit",
    "route": {"name": "ai-full-stack"},
    "config": {
      "limit_by": "consumer",
      "rpm_limit": 60,
      "tpm_limit": 60000,
      "error_message": "Rate limit exceeded, try again later"
    }
  }'
```

**第五步**：挂载 ai-proxy（优先级 770，最后执行）

```bash
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-proxy",
    "route": {"name": "ai-full-stack"},
    "config": {
      "model": {"provider": "openai", "name": "gpt-4o"},
      "model_source": "config",
      "route_type": "llm/v1/chat",
      "client_protocol": "openai",
      "response_streaming": "allow",
      "log_statistics": true,
      "auth": {
        "header_name": "Authorization",
        "header_value": "Bearer sk-..."
      }
    }
  }'
```

### 请求流程

```
客户端 POST /ai/chat
  → ai-prompt-guard (773): 内容安全检查 → 违规则 400 返回
  → ai-cache (772):        缓存键计算，命中则直接返回缓存
  → ai-rate-limit (771):   RPM/TPM 检查 → 超限则 429 返回
  → ai-proxy (770):        转换协议，发往 OpenAI，返回结果
  → ai-cache (772) log:    缓存回写（Redis 集成后生效）
  → ai-rate-limit (771) log: TPM 预扣修正
  → ai-proxy (770) log:    写入 token 统计日志
```

---

## 8. 智能模型路由

ai-proxy 支持通过 `model_routes` 配置实现 AI 网关级别的智能路由：根据请求中的 model 名称，用正则匹配决定路由到哪个 provider + 模型，支持加权分配。

### 8.1 配置结构

```json
{
  "model_routes": [
    {
      "pattern": "正则表达式（匹配请求中的 model 名）",
      "targets": [
        {
          "provider_type": "openai",
          "model_name": "gpt-4o",
          "endpoint_url": null,
          "auth_config": { "header_value": "sk-..." },
          "weight": 80
        }
      ]
    }
  ]
}
```

- **`pattern`**：正则表达式，匹配客户端请求体中的 `model` 字段。按规则顺序匹配，第一条命中即生效。
- **`targets`**：匹配后的候选目标列表。多个 target 时按 `weight` 交错加权轮转。
- **`weight`**：加权值，默认 `1`，单项最大 `10000`。同规则内多个 target 的 weight 决定相对流量比例，总和无需等于 100。

> **注意**：配置了 `model_routes` 后，`provider` 字段可省略。路由结果直接决定使用哪个 provider。

### 8.2 使用场景

**场景 1 — A/B 测试（80% OpenAI / 20% Azure）：**

```json
{
  "model_routes": [
    {
      "pattern": "^gpt-4",
      "targets": [
        { "provider_type": "openai", "model_name": "gpt-4o", "weight": 80,
          "auth_config": { "header_value": "sk-openai-xxx" } },
        { "provider_type": "openai_compat", "model_name": "gpt-4o", "weight": 20,
          "endpoint_url": "https://my-azure.openai.azure.com",
          "auth_config": { "header_value": "azure-key-xxx" } }
      ]
    }
  ]
}
```

**场景 2 — 多 Provider 统一入口：**

```json
{
  "model_source": "request",
  "model_routes": [
    {
      "pattern": "^gpt",
      "targets": [
        { "provider_type": "openai", "model_name": "gpt-4o",
          "auth_config": { "header_value": "sk-openai" } }
      ]
    },
    {
      "pattern": "^claude",
      "targets": [
        { "provider_type": "anthropic", "model_name": "claude-3-opus-20240229",
          "auth_config": { "header_value": "sk-ant-xxx" } }
      ]
    },
    {
      "pattern": "^qwen",
      "targets": [
        { "provider_type": "openai_compat", "model_name": "qwen-turbo",
          "endpoint_url": "https://dashscope.aliyuncs.com",
          "auth_config": { "header_value": "sk-qwen-xxx" } }
      ]
    },
    {
      "pattern": ".*",
      "targets": [
        { "provider_type": "openai", "model_name": "gpt-4o-mini",
          "auth_config": { "header_value": "sk-default" } }
      ]
    }
  ]
}
```

客户端发送 `model: "claude-3-opus"` → 自动路由到 Anthropic；发送 `model: "gpt-4o"` → 路由到 OpenAI；未匹配的 → 使用 gpt-4o-mini 兜底。

**场景 3 — 成本优化（不同前缀路由到不同价位模型）：**

```json
{
  "model_source": "request",
  "model_routes": [
    { "pattern": "^cheap-", "targets": [
        { "provider_type": "openai", "model_name": "gpt-3.5-turbo",
          "auth_config": { "header_value": "sk-xxx" } }
    ]},
    { "pattern": "^smart-", "targets": [
        { "provider_type": "anthropic", "model_name": "claude-3-opus-20240229",
          "auth_config": { "header_value": "sk-ant-xxx" } }
    ]},
    { "pattern": ".*", "targets": [
        { "provider_type": "openai", "model_name": "gpt-4o-mini",
          "auth_config": { "header_value": "sk-xxx" } }
    ]}
  ]
}
```

---

## 9. 支持的 Provider

| Provider | `provider_type` | 默认端点 | 鉴权方式 |
|---|---|---|---|
| OpenAI | `openai` | `api.openai.com` | `Authorization: Bearer <key>` |
| Anthropic | `anthropic` | `api.anthropic.com` | `x-api-key: <key>` |
| Google Gemini | `gemini` | `generativelanguage.googleapis.com` | `?key=<key>` 或 `Authorization: Bearer <token>` |
| 通义千问（阿里云） | `openai_compat` | `dashscope.aliyuncs.com` | `Authorization: Bearer <key>` |
| 混元（腾讯云） | `openai_compat` | 自定义 | `Authorization: Bearer <key>` |
| 任意 OpenAI 兼容服务 | `openai_compat` | 自定义（`endpoint_url`） | `Authorization: Bearer <key>` |

### 各 Provider auth_config 示例

**OpenAI / openai_compat：**

```json
{
  "header_name": "Authorization",
  "header_value": "Bearer sk-..."
}
```

**Anthropic：**

```json
{
  "header_name": "x-api-key",
  "header_value": "sk-ant-..."
}
```

**Gemini（Query 参数鉴权）：**

```json
{
  "param_name": "key",
  "param_value": "AIzaSy..."
}
```

**自定义兼容服务（如本地 Ollama、vLLM）：**

```bash
curl -X POST http://localhost:8001/ai-providers \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "local-ollama",
    "provider_type": "openai_compat",
    "endpoint_url": "http://localhost:11434",
    "auth_config": {}
  }'
```

## 10. 精确 prompt-token 计数(Tokenizer Registry)

### 10.1 概述

为了让 `ai-rate-limit` 的 TPM 预扣和 balancer 的 `by_token_size` 路由用上精确值,Kong-Rust 内置了一套统一的 token 计数器:

```
任何模型:
  has_non_text == true   → 远端 count API → HF 兜底 → tiktoken 兜底 → 字符估算
  has_non_text == false  → HF 本地编码 → tiktoken 兜底 → 字符估算
```

`has_non_text` 由 `has_non_text_content(request)` 判定 — 包含 image_url / tools / function_call / response_format 等任一非纯文本字段即为 true。

### 10.2 路径细节

| Provider | 纯文本主路径 | 非文本主路径 |
|----------|-----------|-----------|
| OpenAI(`gpt-4o*`/`gpt-4*`/`gpt-3.5*`) | HF `Xenova/gpt-4o` 等 → tiktoken 兜底 | `POST /v1/responses/input_tokens` → HF → tiktoken |
| OpenAI o1/o3/o4 | tiktoken-rs(暂无 Xenova 端口) | `POST /v1/responses/input_tokens` → tiktoken |
| Anthropic Claude | `POST /v1/messages/count_tokens` | 同左 |
| Google Gemini | `POST /v1beta/models/{model}:countTokens` | 同左 |
| HuggingFace 开源(LLaMA/Qwen/Mistral 等) | HF 本地 tokenizer.json | 同左(多模态先只算文本) |
| OpenAI 兼容(vLLM/Ollama) | tiktoken-rs | tiktoken-rs |

### 10.3 LRU 缓存

三家远端 client 共享一个 moka LRU,key = `(provider, model, has_non_text, sha256(prompt))`,默认容量 1024、TTL 60 秒。本地路径(tiktoken / HF)不走缓存(已经够快)。

### 10.4 HF 首次冷启动(不阻塞)

模型 `Qwen/Qwen2.5-7B` 首次见到时:
1. `try_get` 同步返回 None
2. 后台 `tokio::spawn` 单飞下载 `tokenizer.json` 到 cache 目录
3. 本次请求降级到字符估算(不阻塞)
4. 下载完成后,后续请求同步命中 Loaded → 1-10ms 编码

并发同 repo 的多个请求合并为一次下载(DashMap CAS)。

### 10.5 配置(kong.conf)

```ini
ai_tokenizer_enabled = true
ai_tokenizer_per_request_deadline_ms = 300       # 整体超时
ai_tokenizer_remote_count_timeout_ms = 1000      # 远端 HTTP 单次超时
ai_tokenizer_cache_capacity = 1024
ai_tokenizer_cache_ttl_seconds = 60
ai_tokenizer_offline = false                     # true 时只读 HF 磁盘缓存,不下载
ai_tokenizer_cache_dir = /var/lib/kong/tokenizers   # 可选,默认 ~/.cache/kong-rust/tokenizers

# 远端 API key — 不配置则该 provider 不启用远端,自动降级到本地路径
ai_tokenizer_openai_api_key = sk-...
ai_tokenizer_anthropic_api_key = sk-ant-...
ai_tokenizer_gemini_api_key = AIzaSy...

# Endpoint 默认指向官方,只在自定义代理时覆盖
# ai_tokenizer_openai_endpoint = https://api.openai.com
```

### 10.6 已知限制

- HF 多模态(image_url / input_audio)token 暂时只算文本部分;后续会按各模型 vision tower 公式补充 patch token 计算
- OpenAI 远端 API 需要正式 OpenAI API key(非 Azure)
- 离线模式仅读磁盘缓存,缺失则 HF 路径降级一次

### 10.7 by_token_size 路由(可选)

`AiModel.max_input_tokens` 字段 + balancer `select_for(prompt_tokens)` 实现"短 prompt 走小模型,长 prompt 自动升档"。同 priority 内若所有候选都被 token 阈值过滤,自动 fallback 到下一档:

```sql
INSERT INTO ai_models (name, model_name, priority, weight, max_input_tokens) VALUES
  ('chat-group', 'gpt-3.5-turbo',  20, 1, 4096),       -- 短 prompt 优先
  ('chat-group', 'gpt-4o',         10, 1, 128000);     -- 长 prompt 升档
```

## 11. 调用统计与成本估算

Kong-Rust 会为最终 Route 插件链中包含已启用 `ai-proxy` 的每个请求记录一条仅含
元数据的调用事实。正常请求、网关拒绝、网关错误、上游错误、客户端断开和流中断
均纳入统计；未匹配 Route 或插件链不含 `ai-proxy` 的请求不计入。

调用统计面向运营分析，不是供应商账单、零丢失审计账本，也不是 Virtual Key
预算执行的记账路径。代理热路径只尝试一次非阻塞入队；队列满、数据库长期不可用
或进程崩溃时可能丢失事实，writer 状态和指标会暴露这些降级。

### 11.1 Usage 与结果口径

Provider 官方 usage 优先。官方字段缺失时，Kong-Rust 可使用请求侧 tokenizer
估值；只有 prompt 和 completion 都已知时才派生 `total_tokens`。未知值返回
`null`，不会伪造为 0。

各 Provider 的标准化规则如下：

- OpenAI 和 OpenAI-compatible 直接使用 prompt/completion usage，保留官方
  total，并在 Provider 返回时保留 cached/reasoning 明细；
- Anthropic 的 prompt 包含 input、cache-creation input 与 cache-read input，
  output 使用最终累计值；
- Gemini 的 completion 包含 candidate 与 thinking token，同时单独保留 thinking
  和 cached token 明细。

`usage.source` 取值为 `provider`、`estimated`、`mixed` 或 `unavailable`。
结果 `outcome` 取值为 `success`、`gateway_rejected`、`gateway_error`、
`upstream_error`、`client_disconnected` 或 `stream_interrupted`。E2E 包含
完整网关生命周期；只有观察到首个可解析流事件时才有 TTFT。`cache_status` 描述
Kong-Rust AI 响应缓存，取值为 `not_configured`、`unavailable`、`bypass`、
`miss` 或 `hit`，不表示 Provider prompt cache。

### 11.2 精确价格与成本状态

单价单位是 USD / 1M tokens。Input 和 output 分方向按以下顺序解析：

1. AI Model 对应方向的显式覆盖价（`input_cost` 或 `output_cost`），显式 0 合法；
2. 内置版本化价表中的精确模型 ID 或显式 alias；
3. 未匹配。

`openai_compat` 不会自动套用 OpenAI 价格。Model 创建/更新接受精确十进制字符串，
并兼容既有有限 JSON number 输入。`GET /ai-models` 保留原有 number 类型的
`input_cost`、`output_cost`，新增 `input_cost_decimal`、
`output_cost_decimal`，并返回由服务端解析的 `effective_pricing`。覆盖价留空即
回退内置价。

标准成本公式为：

```text
(prompt_tokens × input_price + completion_tokens × output_price) / 1,000,000
```

单价和成本均使用 Decimal 计算，API 固定返回 12 位小数字符串，例如
`"0.001100000000"`。事实会固化请求时使用的价格版本、快照日期和有效期，因此后续
修改 Model 或内置价表不会改写历史成本。

内置价表版本 `2026-07-26.1` 包含：

| Provider | 模型 / 显式 alias | UTC 生效时间 | Input | Output |
|---|---|---|---:|---:|
| OpenAI | `gpt-5.6-sol` / `gpt-5.6` | 2026-07-26 起 | 5.00 | 30.00 |
| OpenAI | `gpt-5.6-terra` | 2026-07-26 起 | 2.50 | 15.00 |
| OpenAI | `gpt-5.6-luna` | 2026-07-26 起 | 1.00 | 6.00 |
| Anthropic | `claude-fable-5` | 2026-07-26 起 | 10.00 | 50.00 |
| Anthropic | `claude-opus-4-8` | 2026-07-26 起 | 5.00 | 25.00 |
| Anthropic | `claude-sonnet-5` | 2026-07-26 至 2026-09-01 | 2.00 | 10.00 |
| Anthropic | `claude-sonnet-5` | 2026-09-01 起 | 3.00 | 15.00 |
| Anthropic | `claude-haiku-4-5-20251001` / `claude-haiku-4-5` | 2026-07-26 起 | 1.00 | 5.00 |
| Gemini | `gemini-3.6-flash` | 2026-07-26 起 | 1.50 | 7.50 |
| Gemini | `gemini-3.5-flash` | 2026-07-26 起 | 1.50 | 9.00 |
| Gemini | `gemini-3.5-flash-lite` | 2026-07-26 起 | 0.30 | 2.50 |

价表中的三款 GPT-5.6 仅支持不超过 272,000 prompt tokens；超过阈值时，除非
Model 同时覆盖两个方向，否则标记为 `unsupported`。Provider prompt-cache
计价、非标准 service tier、内置工具、非文本模态和其他附加价格同样暂不支持：
可用 usage 仍会保存，但不会把基础项的部分成本冒充为完整成本。

`pricing.status` 取值为 `matched`、`unmatched`、`unsupported` 或
`not_applicable`；`cost.status` 取值为 `calculated`、`estimated`、
`not_incurred` 或 `unavailable`。机器可读原因数组会解释“不支持”或“不可计算”。
只有确认没有调用 Provider 的请求才返回 0 且为 `not_incurred`；未知成本为
`null`。

### 11.3 Admin API

使用 `GET /ai-usage` 查询请求事实：

```bash
curl -G http://localhost:8001/ai-usage \
  --data-urlencode 'start=2026-07-25T00:00:00Z' \
  --data-urlencode 'end=2026-07-26T00:00:00Z' \
  --data-urlencode 'provider_type=openai' \
  --data-urlencode 'size=100'
```

默认时间窗为最近 24 小时。显式 `start` 与 `end` 必须同时提供、使用 RFC 3339、
遵循 `[start, end)`，且最长 90 天。`size` 默认 100，范围为 1～1000。结果固定按
`(started_at DESC, id DESC)` 排序。

首个响应会返回不透明的 `snapshot` 和 `offset`。下一页传入 `offset`；并发新增
事实不会混入既定 snapshot。客户端不应解码或自行构造游标。

通用精确过滤器包括：

```text
request_id, route_id, service_id, provider_id, provider_type,
requested_model, model_group, actual_model, virtual_key_id, consumer_id,
status_code, outcome, stream, cache_status, usage_source,
pricing_status, cost_status
```

`request_id` 必须是大小写敏感的 32 位小写十六进制精确值。API 固定查询默认
workspace，并拒绝 `workspace_id` 参数。

使用 `GET /ai-usage/summary` 查询总计和一种可选 breakdown：

```bash
curl -G http://localhost:8001/ai-usage/summary \
  --data-urlencode 'start=2026-07-19T00:00:00Z' \
  --data-urlencode 'end=2026-07-26T00:00:00Z' \
  --data-urlencode 'breakdown=day' \
  --data-urlencode 'timezone=Asia/Shanghai'
```

Breakdown 支持 `hour`、`day`、`provider`、`actual_model`、`model_group`、
`virtual_key`、`route` 或 `service`。时间 breakdown 接受 IANA `timezone`；
分类 breakdown 接受 `limit`（默认 10，最大 100）和
`order_by=cost_usd|total_tokens|requests`。Hour breakdown 最长 31 天。

汇总 token 是“已知小计”，同时返回已知/未知请求数和覆盖率；
`cost_usd_calculable_sum` 也只是“可计算成本小计”。Pricing/cost 状态计数用于
揭示缺失或不支持的数据，因此两者都不应标成完整供应商账单。明细和汇总可共享同一
`snapshot` 对账。

两个端点都会按运行模式返回 `meta.mode`、`meta.ephemeral`，以及适用时的节点、
容量、最早记录和重启语义。稳定错误包括：

| HTTP | `error_code` | 含义 |
|---:|---|---|
| 400 | `analytics_invalid_query` | 参数、时间范围、过滤或游标非法 |
| 409 | `analytics_snapshot_expired` | DB-less ring 在翻页期间发生变化 |
| 501 | `analytics_unsupported_in_hybrid` | CP/DP Hybrid 不支持 analytics |
| 503 | `analytics_query_timeout` | 查询达到 5 秒上限 |
| 503 | `analytics_query_unavailable` | Store 暂时不可用 |

### 11.4 Kong Manager

进入 **AI Gateway → 调用统计**。“用量分析”提供 24 小时、7 天、30 天和自定义
时间范围，展示可计算成本、请求数、已知 token 小计及覆盖率、成本/token 趋势、
实际模型与 Virtual Key 排行；“调用日志”提供元数据事实、精确过滤、稳定翻页和
详情视图。

过滤条件和浏览器 IANA 时区会写入 URL，刷新、前进后退和分享链接后仍能保留。
Models 与 Virtual Keys 表格提供“查看用量”下钻。未知值显示为 `—`，真实 0 与
“未定价/不可计算”明确区分。DB-less 与 Hybrid 会展示专用状态，不会伪装成零调用。

### 11.5 运行模式与配置

| 模式 | 存储与查询行为 |
|---|---|
| Traditional + PostgreSQL | 批量持久化到 `ai_usage_logs`，`ephemeral=false` |
| Traditional + `database=off` | 本节点有界内存 ring，满容量淘汰，重启清空 |
| Hybrid control/data plane | 禁用采集和上传；Admin 查询可达时返回 501 |

DB-less 不会把事实写入声明式配置 Store，也不会跨节点聚合。发生淘汰后会保守地
让既定 snapshot 失效并返回 409，而不是返回不完整页面。

在 `kong.conf` 中配置 writer 和 DB-less ring：

```ini
ai_usage_queue_capacity = 8192
ai_usage_batch_size = 256
ai_usage_flush_interval_ms = 500
ai_usage_shutdown_timeout_ms = 5000
ai_usage_dbless_capacity = 10000
```

五项配置都必须大于 0；`ai_usage_queue_capacity` 和
`ai_usage_dbless_capacity` 最大为 `1000000`，`ai_usage_batch_size` 最大为
`1129` 且不得大于队列容量。批量上限确保单条 PostgreSQL INSERT 不超过协议参数
数量限制；非法值会导致启动失败。PostgreSQL 写入使用有界退避重试，优雅关闭只在
配置的超时内 drain。

`GET /status` 包含 `ai_usage_writer`。启用 Prometheus status 端点后，还会暴露
`kong_ai_usage_writer_*` counters，以及 `queue_depth` / `queue_capacity`
gauges。至少监控丢失事实、队列满、写入失败、重试耗尽、关闭超时和 DB-less 淘汰。

### 11.6 隐私、保留与运维限制

调用事实只保存元数据，不包含 prompt、响应正文、请求/响应 header、Authorization
或 API Key、Provider 认证配置、Virtual Key 明文或 `key_hash`。为便于诊断，可以
保存 Virtual Key 名称和非敏感 prefix。事实固化 Route、Service、Provider、Model
和身份快照，因此删除这些配置实体不会删除历史事实。

首版 PostgreSQL 实现没有自动 retention、分区、归档、导出或删除 API，
`ai_usage_logs` 会持续增长。请监控表/索引大小、数据库容量、writer 失败和查询
延迟；在专用保留功能交付前，应建立外部运维治理。API 查询窗口最多 90 天，但该
限制不会删除更早数据。

首版也不提供 DB-less 跨节点聚合、Hybrid DP→CP 上传、供应商账单对账、折扣/税费/
多币种或硬预算执行。
