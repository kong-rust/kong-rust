# Kong-Rust AI Gateway Usage Guide

This document is the user-facing guide for Kong-Rust AI Gateway, covering quick start, plugin configuration reference, Admin API, and common deployment patterns.

---

## Table of Contents

1. [Quick Start](#1-quick-start)
2. [Core Concepts](#2-core-concepts)
3. [Plugin Configuration Reference](#3-plugin-configuration-reference)
4. [Admin API Reference](#4-admin-api-reference)
5. [Multi-Provider Load Balancing](#5-multi-provider-load-balancing)
6. [Dual Protocol Support](#6-dual-protocol-support)
7. [Plugin Combination Examples](#7-plugin-combination-examples)
8. [Intelligent Model Routing](#8-intelligent-model-routing)
9. [Supported Providers](#9-supported-providers)

---

## 1. Quick Start

The following demonstrates the shortest path: attaching an ai-proxy plugin to a Route to connect it to OpenAI.

### 1.1 Create an AI Provider

An AI Provider stores the connection parameters and authentication credentials for an LLM service.

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

Example response (sensitive fields in `auth_config` are automatically masked to `***`):

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

### 1.2 Create an AI Model

An AI Model describes which specific model to use from which Provider. `name` is the logical name (multiple Models can share the same `name` to form a Model Group), and `model_name` is the actual model identifier sent to the Provider.

```bash
PROVIDER_ID="a1b2c3d4-..."   # id returned from the previous step

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

### 1.3 Create a Route

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

### 1.4 Attach the ai-proxy Plugin to the Route

```bash
ROUTE_ID="<route id returned from the previous step>"

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

> **Note**: In the current MVP phase, ai-proxy Provider credentials are inlined directly in the plugin `config.provider` (the `model_source=config` path). Future versions will support referencing AI Provider / AI Model entities via the `model` field.

### 1.5 Send a Request

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

The response conforms to the OpenAI Chat Completions API format, and the response header `X-Kong-LLM-Model` indicates the actual model used.

---

## 2. Core Concepts

### AI Provider

An AI Provider represents the connection configuration for an LLM service, including:

- **provider_type**: Service type identifier that determines which built-in driver to use (`openai` / `anthropic` / `gemini` / `openai_compat`)
- **auth_config**: Authentication parameters (API Key, HTTP Header name, Query parameter, etc.)
- **endpoint_url**: Custom upstream address (defaults to each Provider's public endpoint)

When reading Providers via the Admin API, sensitive fields in `auth_config` (`header_value`, `param_value`, etc.) are automatically masked.

### AI Model / Model Group

An AI Model is a mapping from a "logical model" to a "physical Provider model":

- **name**: Logical name. **Multiple AI Models can share the same `name`**, forming a Model Group used for load balancing (by `weight`) and failover (by `priority`, lower value takes precedence).
- **model_name**: The actual model identifier sent to the Provider (e.g. `gpt-4o`, `claude-3-5-sonnet-20241022`).
- **priority / weight**: Controls routing strategy within a Model Group (priority + weighted).

### AI Virtual Key

An AI Virtual Key is a virtual API key for users/teams, used for:

- Fine-grained TPM / RPM quota control
- Budget caps (`budget_limit`) and usage tracking (`budget_used`)
- Model whitelist (`allowed_models`) restricting which models can be accessed

Virtual Keys have the format `sk-kr-<uuid32>`. The raw key is returned once at creation time; only its SHA256 hash is stored thereafter.

### Four Plugins and Their Priorities

Plugins execute in descending priority order (higher number executes first):

| Plugin | Priority | Responsibility |
|---|---|---|
| ai-prompt-guard | 773 | Security check: deny/allow pattern matching, message length limit |
| ai-cache | 772 | Semantic cache: compute cache key, short-circuit on hit |
| ai-rate-limit | 771 | Rate limiting: RPM / TPM counting, pre-deduction correction |
| ai-proxy | 770 | Core proxy: protocol conversion, upstream routing, token accounting |

---

## 3. Plugin Configuration Reference

### 3.1 ai-proxy

The core plugin responsible for converting client OpenAI / Anthropic format requests into the target Provider's protocol, forwarding the request, and converting the response.

#### Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `model` | string | `""` | Logical model name (references the AI Model `name`); required when `model_source=config` |
| `model_source` | string | `"config"` | Model source: `config` (from plugin config) or `request` (from request body `model` field) |
| `route_type` | string | `"llm/v1/chat"` | Route type: `llm/v1/chat` or `llm/v1/completions` |
| `client_protocol` | string | `"openai"` | Client protocol: `openai` or `anthropic` |
| `response_streaming` | string | `"allow"` | Streaming policy: `allow` (honor client request) / `deny` (force non-streaming) / `always` (force streaming) |
| `max_request_body_size` | integer | `128` | Maximum request body size (KB); returns 413 if exceeded |
| `model_name_header` | boolean | `true` | Whether to return the actual model name in the response header `X-Kong-LLM-Model` |
| `timeout` | integer | `60000` | Upstream timeout (milliseconds) |
| `retries` | integer | `1` | Upstream retry count |
| `log_payloads` | boolean | `false` | Whether to log request/response bodies (for debugging) |
| `log_statistics` | boolean | `true` | Whether to log token statistics |
| `model_routes` | array | `[]` | Intelligent routing rules (regex matching + weighted selection, see the "Intelligent Routing" section below) |
| `provider` | object | `null` | Inline Provider configuration (see below); may be omitted when `model_routes` is configured |

#### Inline Provider Configuration (`provider` field)

| Field | Type | Description |
|---|---|---|
| `provider_type` | string | Provider type: `openai` / `anthropic` / `gemini` / `openai_compat` |
| `auth_config` | object | Authentication parameters, same format as the AI Provider entity's `auth_config` |
| `endpoint_url` | string | Optional, custom upstream URL (overrides Provider default address) |

#### Example Configurations

**Standard OpenAI integration:**

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

**Allow clients to select their own model (`model_source=request`):**

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

The client specifies `"model": "gpt-4o-mini"` in the request body, and the gateway passes that model name through to OpenAI.

---

### 3.2 ai-rate-limit

Enforces RPM (Requests Per Minute) and TPM (Tokens Per Minute) rate limiting on AI requests. Uses a sliding window (60 seconds); TPM uses a pre-deduction + correction mechanism to ensure accurate metering.

#### Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `limit_by` | string | `"consumer"` | Rate limit dimension: `consumer` / `route` / `global` / `virtual_key` |
| `tpm_limit` | integer | `null` | Tokens Per Minute limit; `null` means unlimited |
| `rpm_limit` | integer | `null` | Requests Per Minute limit; `null` means unlimited |
| `header_name` | string | `"X-AI-Key"` | Request header name for reading the Virtual Key (effective when `limit_by=virtual_key`) |
| `error_code` | integer | `429` | HTTP status code returned when limit is exceeded |
| `error_message` | string | `"AI rate limit exceeded"` | Error message returned when limit is exceeded |

#### Example Configurations

**Rate limit by Route: max 100 requests and 100k tokens per minute:**

```json
{
  "limit_by": "route",
  "rpm_limit": 100,
  "tpm_limit": 100000,
  "error_code": 429,
  "error_message": "Too many requests, please slow down"
}
```

**Rate limit by Consumer (no RPM limit, TPM only):**

```json
{
  "limit_by": "consumer",
  "tpm_limit": 50000
}
```

---

### 3.3 ai-cache

Caches AI responses to identical questions to reduce LLM call costs. The current version implements the cache key computation infrastructure (SHA256); Redis backend integration will be provided in a future release.

#### Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `cache_ttl` | integer | `300` | Cache TTL (seconds) |
| `cache_key_strategy` | string | `"last_question"` | Cache key strategy: `last_question` (only the last user message) / `all_questions` (all user messages concatenated) |
| `skip_header` | string | `"X-AI-Skip-Cache"` | Skip cache lookup when the client sends this header |

#### Comparison of Cache Key Strategies

| Strategy | Use Case | Description |
|---|---|---|
| `last_question` | Single-turn Q&A, FAQ scenarios | SHA256 of only the last `role=user` message content |
| `all_questions` | Multi-turn conversation, context-sensitive scenarios | SHA256 of all `role=user` messages concatenated |

#### Example Configuration

```json
{
  "cache_ttl": 600,
  "cache_key_strategy": "last_question",
  "skip_header": "X-AI-Skip-Cache"
}
```

Client forcing cache bypass:

```bash
curl -X POST http://localhost:8000/v1/chat/completions \
  -H 'X-AI-Skip-Cache: true' \
  -H 'Content-Type: application/json' \
  -d '{"messages": [{"role": "user", "content": "..."}]}'
```

---

### 3.4 ai-prompt-guard

Performs security review on user input (`role=user` messages), supporting deny patterns (blacklist), allow patterns (whitelist), and message length limits.

#### Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `deny_patterns` | string[] | `[]` | Deny pattern list (regex); triggers if any one matches |
| `allow_patterns` | string[] | `[]` | Allow pattern list (regex); when configured, messages must match at least one, otherwise triggers |
| `max_message_length` | integer | `32768` | Maximum bytes per message |
| `action` | string | `"block"` | Action on trigger: `block` (reject request) / `log_only` (log only, do not block) |
| `error_code` | integer | `400` | HTTP status code returned when blocked |
| `error_message` | string | `"request blocked by ai-prompt-guard"` | Error message returned when blocked |

> **Note**: When both `deny_patterns` and `allow_patterns` are configured, the deny check runs first, then the allow check (the message must pass both). When `allow_patterns` is empty, the whitelist logic does not apply.

#### Example Configurations

**Block sensitive topics and limit message length:**

```json
{
  "deny_patterns": [
    "(?i)(password|secret|api.?key|token)",
    "(?i)(ignore.*previous.*instruction|jailbreak)"
  ],
  "max_message_length": 4096,
  "action": "block",
  "error_code": 400,
  "error_message": "Your request contains prohibited content"
}
```

**Allowlist mode (only allow specific topics):**

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

**Audit mode (log only, do not block):**

```json
{
  "deny_patterns": ["(?i)(competitor|alternative)"],
  "action": "log_only"
}
```

---

## 4. Admin API Reference

All AI Gateway-specific endpoints are prefixed with `/ai-`. The base path is the Admin API address (default `http://localhost:8001`).

### AI Provider

| Method | Path | Description |
|---|---|---|
| `GET` | `/ai-providers` | List all Providers (supports pagination, tag filtering) |
| `POST` | `/ai-providers` | Create a Provider |
| `GET` | `/ai-providers/{id_or_name}` | Get a single Provider |
| `PATCH` | `/ai-providers/{id_or_name}` | Update Provider fields partially |
| `PUT` | `/ai-providers/{id_or_name}` | Replace (upsert) a Provider |
| `DELETE` | `/ai-providers/{id_or_name}` | Delete a Provider |
| `GET` | `/ai-providers/{id}/ai-models` | List all AI Models under this Provider |

> In all read responses, sensitive fields in `auth_config` (`header_value`, `param_value`, `aws_secret_access_key`, `gcp_service_account_json`) are replaced with `"***"`.

### AI Model

| Method | Path | Description |
|---|---|---|
| `GET` | `/ai-models` | List all AI Models |
| `POST` | `/ai-models` | Create an AI Model |
| `GET` | `/ai-models/{id}` | Get a single AI Model |
| `PATCH` | `/ai-models/{id}` | Update AI Model fields partially |
| `PUT` | `/ai-models/{id}` | Replace (upsert) an AI Model |
| `DELETE` | `/ai-models/{id}` | Delete an AI Model |
| `GET` | `/ai-model-groups` | List all distinct Model logical names (i.e., all Model Groups) |

### AI Virtual Key

| Method | Path | Description |
|---|---|---|
| `GET` | `/ai-virtual-keys` | List all Virtual Keys |
| `POST` | `/ai-virtual-keys` | Create a Virtual Key (raw key returned once in the `key` field) |
| `GET` | `/ai-virtual-keys/{id_or_name}` | Get a single Virtual Key |
| `PATCH` | `/ai-virtual-keys/{id_or_name}` | Update Virtual Key configuration |
| `DELETE` | `/ai-virtual-keys/{id_or_name}` | Delete a Virtual Key |
| `POST` | `/ai-virtual-keys/{id}/rotate` | Rotate the key (generates a new key, returns the new raw `key`) |

> **Security note**: The `key_hash` field is removed from all responses. The raw key (the `key` field) appears only once — in the successful response of `POST /ai-virtual-keys` and `POST /ai-virtual-keys/{id}/rotate`. Store it securely.

---

## 5. Multi-Provider Load Balancing

By giving multiple AI Models the same `name`, they automatically form a Model Group. The ai-proxy selects the backend by `priority` + `weight` during routing.

### Scenario: OpenAI Primary + Anthropic Backup

**Prerequisites**:

- Create two Providers: `openai-prod` and `anthropic-prod`
- Create two AI Models both with `name` set to `gpt4-tier`, each pointing to a different Provider

```bash
# Provider 1 — OpenAI
curl -X POST http://localhost:8001/ai-providers \
  -H 'Content-Type: application/json' \
  -d '{"name": "openai-prod", "provider_type": "openai", "auth_config": {"header_name": "Authorization", "header_value": "Bearer sk-openai-..."}}'

# Provider 2 — Anthropic
curl -X POST http://localhost:8001/ai-providers \
  -H 'Content-Type: application/json' \
  -d '{"name": "anthropic-prod", "provider_type": "anthropic", "auth_config": {"header_name": "x-api-key", "header_value": "sk-ant-..."}}'

# AI Model A — Primary (priority=1, high weight)
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

# AI Model B — Backup (priority=2, low weight)
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

Reference the logical name `gpt4-tier` in the ai-proxy plugin:

```json
{
  "model": "gpt4-tier",
  "model_source": "config"
}
```

The gateway selects the best Provider by `priority`; within the same priority, traffic is distributed by `weight`.

### View Model Groups

```bash
curl http://localhost:8001/ai-model-groups
# Returns all distinct names, i.e., the list of all Model Groups
```

---

## 6. Dual Protocol Support

Kong-Rust AI Gateway supports exposing two client protocols simultaneously:

- **OpenAI protocol** (`client_protocol=openai`): clients use `POST /v1/chat/completions` format
- **Anthropic protocol** (`client_protocol=anthropic`): clients use `POST /v1/messages` format

Regardless of which protocol the client uses, the gateway internally converts it to the target Provider's format.

### Example: Same Backend, Two Routes, Two Protocols

**Route 1 — OpenAI protocol endpoint:**

```bash
# Create route
curl -X POST http://localhost:8001/routes \
  -H 'Content-Type: application/json' \
  -d '{"name": "ai-openai", "paths": ["/v1/chat/completions"], "methods": ["POST"], "strip_path": false}'

# Attach ai-proxy, client_protocol=openai
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

**Route 2 — Anthropic protocol endpoint:**

```bash
# Create route
curl -X POST http://localhost:8001/routes \
  -H 'Content-Type: application/json' \
  -d '{"name": "ai-anthropic", "paths": ["/v1/messages"], "methods": ["POST"], "strip_path": false}'

# Attach ai-proxy, client_protocol=anthropic
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

**Anthropic protocol request example:**

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

## 7. Plugin Combination Examples

The following demonstrates a complete production-grade configuration combining all 4 AI plugins.

### Goal

- **ai-prompt-guard**: Block sensitive content, limit message length (security first)
- **ai-cache**: Cache identical questions for 5 minutes (cost reduction)
- **ai-rate-limit**: Max 60 requests and 60k tokens per minute per Consumer (quota management)
- **ai-proxy**: Route to OpenAI gpt-4o (core proxy)

### Steps

**Step 1**: Create the Route

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

**Step 2**: Attach ai-prompt-guard (priority 773, executes first)

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

**Step 3**: Attach ai-cache (priority 772)

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

**Step 4**: Attach ai-rate-limit (priority 771)

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

**Step 5**: Attach ai-proxy (priority 770, executes last)

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

### Request Flow

```
Client POST /ai/chat
  → ai-prompt-guard (773): content security check → returns 400 if violation
  → ai-cache (772):        compute cache key, return cached response on hit
  → ai-rate-limit (771):   RPM/TPM check → returns 429 if exceeded
  → ai-proxy (770):        convert protocol, forward to OpenAI, return result
  → ai-cache (772) log:    write back to cache (effective after Redis integration)
  → ai-rate-limit (771) log: TPM pre-deduction correction
  → ai-proxy (770) log:    write token statistics to log
```

---

## 8. Intelligent Model Routing

ai-proxy supports gateway-level intelligent routing via the `model_routes` configuration: based on the model name in the request, regex matching determines which provider + model to route to, with weighted distribution support.

### 8.1 Configuration Structure

```json
{
  "model_routes": [
    {
      "pattern": "regex (matches the model name in the request)",
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

- **`pattern`**: Regex that matches the `model` field in the client request body. Rules are matched in order; the first match wins.
- **`targets`**: List of candidate targets after a match. When multiple targets are present, one is selected by weighted round-robin based on `weight`.
- **`weight`**: Weight value, default `1`. The weights of multiple targets within the same rule determine the traffic distribution ratio.

> **Note**: When `model_routes` is configured, the `provider` field may be omitted. The routing result directly determines which provider to use.

### 8.2 Use Cases

**Case 1 — A/B testing (80% OpenAI / 20% Azure):**

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

**Case 2 — Multi-provider unified entry point:**

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

Client sends `model: "claude-3-opus"` → automatically routed to Anthropic; sends `model: "gpt-4o"` → routed to OpenAI; unmatched → falls back to gpt-4o-mini.

**Case 3 — Cost optimization (route different prefixes to different pricing tiers):**

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

## 9. Supported Providers

| Provider | `provider_type` | Default Endpoint | Authentication |
|---|---|---|---|
| OpenAI | `openai` | `api.openai.com` | `Authorization: Bearer <key>` |
| Anthropic | `anthropic` | `api.anthropic.com` | `x-api-key: <key>` |
| Google Gemini | `gemini` | `generativelanguage.googleapis.com` | `?key=<key>` or `Authorization: Bearer <token>` |
| Alibaba Cloud Qwen | `openai_compat` | `dashscope.aliyuncs.com` | `Authorization: Bearer <key>` |
| Tencent Cloud Hunyuan | `openai_compat` | custom | `Authorization: Bearer <key>` |
| Any OpenAI-compatible service | `openai_compat` | custom (`endpoint_url`) | `Authorization: Bearer <key>` |

### auth_config Examples per Provider

**OpenAI / openai_compat:**

```json
{
  "header_name": "Authorization",
  "header_value": "Bearer sk-..."
}
```

**Anthropic:**

```json
{
  "header_name": "x-api-key",
  "header_value": "sk-ant-..."
}
```

**Gemini (query parameter authentication):**

```json
{
  "param_name": "key",
  "param_value": "AIzaSy..."
}
```

**Custom compatible service (e.g., local Ollama, vLLM):**

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

## 10. Precise Prompt-Token Counting (Tokenizer Registry)

### 10.1 Overview

To give `ai-rate-limit` a precise TPM pre-debit and the balancer a working `by_token_size` route, Kong-Rust ships a unified token counter:

```
For every model:
  has_non_text == true   → remote count API → HF fallback → tiktoken fallback → char estimate
  has_non_text == false  → HF local encoding → tiktoken fallback → char estimate
```

`has_non_text` is decided by `has_non_text_content(request)` — true when the request includes any of: `image_url`, `tools`, `function_call`, `response_format`, `input_audio`, etc.

### 10.2 Per-provider routing

| Provider | Text path | Non-text path |
|----------|-----------|---------------|
| OpenAI (`gpt-4o*` / `gpt-4*` / `gpt-3.5*`) | HF `Xenova/gpt-4o` etc. → tiktoken | `POST /v1/responses/input_tokens` → HF → tiktoken |
| OpenAI o1/o3/o4 | tiktoken-rs (no Xenova port yet) | `POST /v1/responses/input_tokens` → tiktoken |
| Anthropic Claude | `POST /v1/messages/count_tokens` | same |
| Google Gemini | `POST /v1beta/models/{model}:countTokens` | same |
| HuggingFace open-source (LLaMA/Qwen/Mistral) | HF local tokenizer.json | same (multimodal counts text only for now) |
| OpenAI-compat (vLLM/Ollama) | tiktoken-rs | tiktoken-rs |

### 10.3 Shared LRU

All three remote clients share a moka LRU. Key = `(provider, model, has_non_text, sha256(prompt))`, default capacity 1024, TTL 60s. Local paths (tiktoken / HF) are not cached.

### 10.4 HF first-touch (non-blocking)

When a brand-new repo (e.g. `Qwen/Qwen2.5-7B`) is hit:
1. `try_get` returns None synchronously.
2. A `tokio::spawn` task downloads `tokenizer.json` (single-flight CAS, concurrent calls merge into one).
3. This request degrades to char estimation (does not block).
4. Subsequent requests hit Loaded synchronously (1-10ms encode).

### 10.5 Config (kong.conf)

```ini
ai_tokenizer_enabled = true
ai_tokenizer_per_request_deadline_ms = 300
ai_tokenizer_remote_count_timeout_ms = 1000
ai_tokenizer_cache_capacity = 1024
ai_tokenizer_cache_ttl_seconds = 60
ai_tokenizer_offline = false

# Per-provider API keys (omit to disable that remote path)
ai_tokenizer_openai_api_key = sk-...
ai_tokenizer_anthropic_api_key = sk-ant-...
ai_tokenizer_gemini_api_key = AIzaSy...
```

### 10.6 Known limitations

- HF multimodal token accounting deferred — only text is counted; `image_url`/`input_audio` tokens require per-model vision-tower patch formulas.
- OpenAI count endpoint requires a real OpenAI API key (Azure has its own variant).
- Offline mode reads the HF disk cache only; misses degrade once.

### 10.7 by_token_size routing

Set `AiModel.max_input_tokens` per model and the balancer's `select_for(prompt_tokens)` filters candidates that don't fit. When the entire priority tier is filtered out, it falls back to the next tier — short prompts route to small models for cost, long prompts auto-escalate.
