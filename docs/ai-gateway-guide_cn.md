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
    "strip_path": false
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
      \"model\": \"gpt-4o\",
      \"model_source\": \"config\",
      \"route_type\": \"llm/v1/chat\",
      \"client_protocol\": \"openai\",
      \"provider\": {
        \"provider_type\": \"openai\",
        \"auth_config\": {
          \"header_name\": \"Authorization\",
          \"header_value\": \"Bearer sk-...\"
        }
      }
    }
  }"
```

> **注意**：当前 MVP 阶段 ai-proxy 的 Provider 凭证直接内联在插件 `config.provider` 中（`model_source=config` 路径）。后续版本将支持通过 `model` 字段引用已创建的 AI Provider / AI Model 实体。

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

- 细粒度的 TPM / RPM 配额控制
- 预算上限（`budget_limit`）和使用量追踪（`budget_used`）
- 允许访问的模型白名单（`allowed_models`）

Virtual Key 格式为 `sk-kr-<uuid32>`，创建时一次性返回原始密钥，此后只存储 SHA256 哈希。

### 四个插件及优先级

插件按优先级从高到低执行（数字大者先执行）：

| 插件 | 优先级 | 职责 |
|---|---|---|
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
| `model` | string | `""` | 模型逻辑名称（引用 AI Model 的 `name`）；`model_source=config` 时必填 |
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
| `provider` | object | `null` | 内联 Provider 配置（见下方）；配置了 `model_routes` 时可省略 |

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
  "model": "gpt-4o",
  "model_source": "config",
  "route_type": "llm/v1/chat",
  "client_protocol": "openai",
  "response_streaming": "allow",
  "timeout": 30000,
  "log_statistics": true,
  "provider": {
    "provider_type": "openai",
    "auth_config": {
      "header_name": "Authorization",
      "header_value": "Bearer sk-..."
    }
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

# AI Model B — 备份（priority=2，低权重）
ANTHROPIC_ID="<anthropic provider id>"
curl -X POST http://localhost:8001/ai-models \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"gpt4-tier\",
    \"provider_id\": \"${ANTHROPIC_ID}\",
    \"model_name\": \"claude-3-5-sonnet-20241022\",
    \"priority\": 2,
    \"weight\": 10
  }"
```

在 ai-proxy 插件中引用逻辑名称 `gpt4-tier`：

```json
{
  "model": "gpt4-tier",
  "model_source": "config"
}
```

网关将按 `priority` 选择最优 Provider，同 priority 内按 `weight` 加权路由。

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
      "model": "claude-main",
      "client_protocol": "openai",
      "provider": {
        "provider_type": "anthropic",
        "auth_config": {"header_name": "x-api-key", "header_value": "sk-ant-..."}
      }
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
      "model": "claude-main",
      "client_protocol": "anthropic",
      "provider": {
        "provider_type": "anthropic",
        "auth_config": {"header_name": "x-api-key", "header_value": "sk-ant-..."}
      }
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
      "model": "gpt-4o",
      "model_source": "config",
      "route_type": "llm/v1/chat",
      "client_protocol": "openai",
      "response_streaming": "allow",
      "log_statistics": true,
      "provider": {
        "provider_type": "openai",
        "auth_config": {
          "header_name": "Authorization",
          "header_value": "Bearer sk-..."
        }
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
- **`targets`**：匹配后的候选目标列表。多个 target 时按 `weight` 加权轮询选择。
- **`weight`**：加权值，默认 `1`。同规则内多个 target 的 weight 决定流量分配比例。

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

## 11. ai-proxy 按 token 大小路由

§10.7 的 token 预算原语已经接通到 ai-proxy 的 `model_routes` — 仅靠路由配置(无需写 DAO)就能拿到成本感知路由能力。

### 11.1 工作机制

`access` 阶段:

1. 解码 ChatRequest,通过 tokenizer registry 估算 `prompt_tokens`(provider 启发式推断、registry 自带 deadline)
2. 估值传给 `ModelRouter::resolve_for(model_name, prompt_tokens)`
3. `max_input_tokens` 不够的 target 被过滤掉
4. 按 `priority` 降序尝试;某档全被预算过滤掉 → fallback 下一档
5. 留下的候选在档内做加权 round-robin

`prompt_tokens` 不可用时(registry 关闭、body 解析失败等)自动跳过过滤,行为等价于 `resolve(model_name)`。

### 11.2 配置

`ModelTargetConfig` 新增 2 个字段:

| 字段 | 类型 | 默认 | 用途 |
|------|------|------|------|
| `priority` | i32 | 0 | 高优先级先选;同 priority 内做加权 RR |
| `max_input_tokens` | i32? | 未设置 | 单 target 的 prompt 上限,超过则被过滤 |

插件顶层新增 1 个开关:

| 字段 | 类型 | 默认 | 用途 |
|------|------|------|------|
| `enable_token_size_routing` | bool | true | 设为 false 跳过估值 + 过滤 |

示例:小模型优先,长 prompt fallback 大窗口模型。

```yaml
plugins:
  - name: ai-proxy
    config:
      model: chat-default
      enable_token_size_routing: true
      model_routes:
        - pattern: "^chat"
          targets:
            - provider_type: openai
              model_name: gpt-4o-mini
              priority: 10
              max_input_tokens: 8000
              auth_config: { header_value: sk-... }
            - provider_type: openai
              model_name: gpt-4o
              priority: 0
              max_input_tokens: 128000
              auth_config: { header_value: sk-... }
```

500 token 的 prompt 命中 `gpt-4o-mini`;5 万 token 的 prompt 升档到 `gpt-4o`。所有 cap 都不够时返回 400。

### 11.3 可观测性

每个命中 `model_routes` 的请求都会带上两个响应头:

- `X-Kong-AI-Selected-Model` — 实际选中的上游 model 名(如 `gpt-4o-mini`)
- `X-Kong-AI-Prompt-Tokens` — 路由决策用的 token 估值(仅在启用 token-size 路由时输出)

这两个头是稳定契约,可用于集成测试断言、日志管线、Tracing。

## 12. ai-semantic-cache 插件

按 prompt **embedding 相似度** 缓存 LLM 响应(不是字符串精确匹配)。"What's the weather" 和 "How is the weather" 这种语义等价的两个问题会命中同一份缓存。

### 12.1 生命周期

1. **access** — 从 ChatRequest 提取 cache-key 文本(`LastMessage` / `AllMessages` / `FirstUserMessage`)→ 调 embedding 端点 → vector store 余弦 KNN → 命中(cosine ≥ `similarity_threshold`)→ short-circuit 返回缓存内容,带头 `X-Kong-AI-Cache: HIT-SEMANTIC`
2. **body_filter** — miss 时累积上游响应,end_of_stream 时若 status=200 → 把 (query_vector, response_body) 入 store(带 TTL)

### 12.2 配置

```yaml
plugins:
  - name: ai-semantic-cache
    config:
      embedding_provider: openai_compat
      embedding_endpoint_url: https://api.openai.com    # 或 Azure / vLLM / Ollama
      embedding_api_key: sk-...
      embedding_model: text-embedding-3-small
      embedding_timeout_ms: 200
      similarity_threshold: 0.92                        # 余弦阈值
      cache_ttl_seconds: 3600
      max_cache_entries: 10000
      cache_key_strategy: AllMessages                   # 或 LastMessage / FirstUserMessage
      vector_store: InMemory                            # Redis 后端 = TODO
      skip_header: X-AI-Skip-Cache
```

插件优先级 `773` — 高于 `ai-cache`(772,字符串精确)和 `ai-rate-limit`(771)。

### 12.3 Vector store 后端

- **`InMemory`**(MVP):`Mutex<Vec<VectorEntry>>` 暴力余弦,LRU 按 `last_used` 淘汰,TTL 惰性清理。≤10 000 条目时单次查询 ~ms 级。
- **`Redis`**:`VectorStore` trait 已就位;Redis 后端实现作为 `tasks.md` 阶段 19B 的 follow-up。在它落地之前,任何非 `InMemory` 的值会 fallback 到 `InMemory` 并打 warn 日志。

### 12.4 可观测性头

- `X-Kong-AI-Cache: HIT-SEMANTIC` — 语义命中,响应来自缓存
- `X-Kong-AI-Cache-Similarity: 0.9842` — 命中条目的余弦分数(仅命中时)
- `X-Kong-AI-Cache: MISS-SEMANTIC` — 未达阈值,请求继续到上游,响应已写回缓存

发送 `X-AI-Skip-Cache: 1` 头可单次绕过缓存查找 + 写回。

### 12.5 失败语义

- embedding 端点超时 / 5xx → 跳过缓存查找,请求按无插件正常进上游
- 非 200 响应永远不写回
- TTL 过期条目在下一次 insert/search 时惰性清理

## 13. ai-proxy 语义路由

按 prompt **语义** 选 target,而不是按 prompt 的正则 / 关键词。叠加在 token-size 路由之上 — 先 `max_input_tokens` 过滤,再在剩余候选里语义选择。

### 13.1 工作机制

1. 第一次匹配某条 `model_routes` 规则时,ai-proxy 把所有 `target.semantic_routing_examples` embed 一遍并按 `(endpoint, embedding_model, examples)` 哈希缓存索引
2. 后续每个请求:提取 prompt 文本 → embed 一次 → 对每个候选 target 计算与其 examples 向量的 **最大** cosine → 选最高分(≥ `semantic_routing_min_score`)
3. 任何失败(embedding 服务挂、未配 examples、所有分数低于阈值)→ fallback 加权 round-robin

### 13.2 配置

```yaml
plugins:
  - name: ai-proxy
    config:
      model: smart-router
      enable_semantic_routing: true
      semantic_routing_endpoint_url: https://api.openai.com
      semantic_routing_api_key: sk-...
      semantic_routing_embedding_model: text-embedding-3-small
      semantic_routing_timeout_ms: 200
      semantic_routing_min_score: 0.5

      model_routes:
        - pattern: ".*"
          targets:
            - provider_type: openai
              model_name: gpt-4o-mini
              semantic_routing_examples:
                - "summarize this paragraph"
                - "translate to french"
            - provider_type: anthropic
              model_name: claude-3-opus
              semantic_routing_examples:
                - "write a rust function"
                - "debug this stack trace"
            - provider_type: openai
              model_name: gpt-4o
              semantic_routing_examples:
                - "describe this image"
                - "caption the photo"
```

首次请求:6 次 example-embed 调用(一次性预热)。后续每个请求:1 次 prompt-embed + 暴力 cosine 计算。

### 13.3 与 token-size 路由组合

两者可同时启用,执行顺序:

1. 正则匹配 `model_routes` 规则
2. token-size 过滤:`max_input_tokens < prompt_tokens` 的 target 被丢弃(最高 priority 档优先;耗尽则降档)
3. 在剩余候选里语义选择:挑最高 cosine target
4. 第 3 步任何失败 → fallback 该档加权 RR

### 13.4 失败语义

- 单条 example 预热失败:打 warn 日志,该 example 被跳过;该 target 仍可通过 fallback 路径选中
- 没有任何 target 配 examples → 语义路由静默禁用(等价于不启用)
- 实时 prompt embedding 超时 → fallback 加权 RR
- 所有 cosine 分数低于 `semantic_routing_min_score` → fallback 加权 RR

`semantic_routing_min_score` 存在的目的就是防止"低置信度的语义匹配"覆盖合理的加权默认值。
