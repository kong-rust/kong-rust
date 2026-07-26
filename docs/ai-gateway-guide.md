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
10. [Precise Prompt-Token Counting](#10-precise-prompt-token-counting-tokenizer-registry)
11. [Usage Analytics and Cost Estimation](#11-usage-analytics-and-cost-estimation)
12. [Virtual Key Quota and Budget Enforcement](#12-virtual-key-quota-and-budget-enforcement)

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
    "strip_path": false,
    "response_buffering": false
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
      \"model_group\": \"gpt-4o\",
      \"model_source\": \"config\",
      \"route_type\": \"llm/v1/chat\",
      \"client_protocol\": \"openai\"
    }
  }"
```

> **Note**: `model_group` resolves the AI Model entities created above and reads credentials from their server-side AI Provider records. The official Kong-compatible `model` record remains available for inline provider configurations.

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

- Authenticating proxy traffic (active once the `ai-key-auth` plugin is attached — see [3.5](#35-ai-key-auth))
- Model allow list (`allowed_models`) restricting which models can be accessed (active)
- Fine-grained TPM / RPM quotas (enforced with a per-node 60-second window once the Virtual Key policy chain is attached)
- Lifetime USD budgets and an authoritative ledger (supported in Traditional PostgreSQL mode; see Section 12)

Virtual Keys have the format `sk-kr-<uuid32>`. The raw key is returned once at creation time; only its SHA256 hash is stored thereafter.

### Five Plugins and Their Priorities

Plugins execute in descending priority order (higher number executes first):

| Plugin | Priority | Responsibility |
|---|---|---|
| ai-key-auth | 774 | Authentication: virtual key validation, model allow list, identity injection |
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
| `model` | object | `null` | Official Kong model record (`provider`, optional `name` and `options`) for inline provider configuration |
| `model_group` | string | `null` | Kong-Rust extension: logical AI Model group name resolved from AI Model and AI Provider entities |
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
| `auth` | object | `null` | Official Kong authentication record used with `model.provider` |
| `provider` | object | `null` | Legacy Kong-Rust inline Provider configuration (see below); may be omitted for `model_group` or `model_routes` |

When present, a non-empty `model_group` takes precedence over inline `model` / `provider` fields so provider selection and credentials come from server-side AI entities. New page-created configurations always use `model_group`.

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

Enforces RPM (Requests Per Minute) and TPM (Tokens Per Minute) quotas on AI requests.
The current backend uses a per-process, 60-second fixed window that starts on the first
hit. TPM reserves the prompt estimate at admission and settles against normalized usage
when the request ends. RPM and TPM are admitted atomically within the same window, so a
rejection in either dimension does not leave a partial charge.

#### Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `limit_by` | string | `"consumer"` | Rate limit dimension: `consumer` / `route` / `global` / `virtual_key` |
| `tpm_limit` | integer / null | `null` | Plugin-level TPM limit for `global/route/consumer`, range `1..=2^31-1` |
| `rpm_limit` | integer / null | `null` | Plugin-level RPM limit for `global/route/consumer`, range `1..=2^31-1` |
| `header_name` | string | `"X-AI-Key"` | Deprecated and retained for old configurations; ignored at runtime |
| `error_code` | integer | `429` | Legacy non-Virtual-Key error status |
| `error_message` | string | `"AI rate limit exceeded"` | Legacy non-Virtual-Key error message |

`global/route/consumer` requires at least one plugin-level limit. `virtual_key` requires
both `ai-key-auth` and `ai-proxy`, and both plugin-level `tpm_limit/rpm_limit` values must
be `null`. Limits come exclusively from the authenticated Virtual Key entity. Identity is
the authenticated key UUID; the plugin does not re-read `header_name` or raw credential
headers.

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

**Use the authenticated Virtual Key's own limits:**

```json
{
  "limit_by": "virtual_key"
}
```

See [Section 12](#12-virtual-key-quota-and-budget-enforcement) for key creation, budgets,
headers, and the error contract.

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

### 3.5 ai-key-auth

Authenticates proxy traffic with an AI Virtual Key and enforces the key's model allow list. At priority 774 it runs ahead of every other AI plugin, so downstream plugins (rate limiting, logging, usage) observe an authenticated identity.

Routes without this plugin behave exactly as before — no key required.

#### Configuration Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `key_header` | string | `"X-AI-Key"` | Fallback credential header, tried after `Authorization` and `x-api-key` |
| `error_format` | string | `"auto"` | Error body dialect: `auto` (adapts to the client protocol) / `openai` / `anthropic` |

#### Credential Carriers

The plugin reads the credential in this order and uses the first one found:

1. `Authorization: Bearer <key>` — the OpenAI SDK default (scheme is case-insensitive; non-Bearer schemes such as Basic are skipped)
2. `x-api-key: <key>` — the Anthropic SDK default
3. The header named by `key_header` (default `X-AI-Key`)

Both the official OpenAI and Anthropic SDKs therefore work unmodified.

#### Validation and Responses

| Case | Status | Error code |
|---|---|---|
| No credential presented | 401 | `missing_api_key` |
| Unknown key / `enabled=false` / past `expires_at` | 401 | `invalid_api_key` |
| Requested `model` not in `allowed_models` | 403 | `model_not_allowed` |

> **Security note**: unknown, disabled, and expired keys return an identical response so that key state cannot be probed.

`error_format=auto` resolution order: credential came from `x-api-key` → Anthropic dialect; request path contains `/v1/messages` → Anthropic dialect; otherwise OpenAI.

OpenAI dialect:

```json
{"error": {"message": "invalid API key", "type": "invalid_request_error", "code": "invalid_api_key"}}
```

Anthropic dialect:

```json
{"type": "error", "error": {"type": "authentication_error", "message": "invalid API key"}}
```

#### Model Allow List (allowed_models)

- Empty array or unset → unrestricted
- Entries ending in `*` match by prefix: `gpt-4*` matches `gpt-4`, `gpt-4o`, `gpt-4-turbo`, but not `gpt-3.5-turbo`
- All other entries match exactly (case-sensitive)
- Entries are OR-combined — any hit allows the request; a bare `*` is equivalent to unrestricted
- **Only checked when the request body carries a `model` field**: under `model_source=config` clients legitimately omit `model`, and the allow list is skipped in that case

#### Identity Injection

On success the plugin injects into the request context:

- `AiAuthContext { virtual_key_id, key_name, consumer_id }` — the channel downstream AI plugins read
- `consumer_id` (when the key is bound to a Consumer) — this is what makes `ai-rate-limit`'s `limit_by=consumer` and Consumer hashing take effect
- `authenticated_credential` — surfaced in the access log

#### Example: Attach to a Route

```bash
curl -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-key-auth",
    "route": {"id": "<ROUTE_ID>"},
    "config": {}
  }'
```

Create a key and call the endpoint:

```bash
# Create a key (the raw key is returned only once, here)
curl -X POST http://localhost:8001/ai-virtual-keys \
  -H 'Content-Type: application/json' \
  -d '{"name": "team-a", "allowed_models": ["gpt-4*"]}'

# Call with the key
curl -X POST http://localhost:8000/ai/demo/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer sk-kr-...' \
  -d '{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}'
```

#### Caching and Propagation Delay

Key lookups are cached in-process (including negative results for invalid keys) with a 1-second TTL. Creating, updating, rotating, or deleting a key through the Admin API invalidates the cache immediately, so those operations take effect at once; the TTL only bounds out-of-band changes (multi-node deployments, direct database writes).

#### DB-less Mode

`ai_virtual_keys` entries in a declarative config must carry their own `key_hash` — the Admin API generates it server-side, but declarative loading does not:

```bash
# Compute key_hash from a raw key
printf 'sk-kr-your-key' | shasum -a 256
```

```yaml
ai_virtual_keys:
  - name: team-a
    key_hash: <the sha256 hex from above>
    key_prefix: sk-kr-y
    enabled: true
    allowed_models: ["gpt-4*"]
```

> **Limitation**: in Hybrid (CP/DP) mode, CP→DP config sync currently excludes all AI entities, so DP nodes hold no Virtual Key data.

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
| `GET` | `/ai-virtual-keys/{id}/budget-ledger` | Read the budget account and ledger with status/time/cursor filters |
| `POST` | `/ai-virtual-keys/{id}/budget-reconciliations` | Manually settle or waive an unresolved intent |
| `POST` | `/ai-virtual-keys/{id}/budget-ledger/rebuild` | Verify or CAS-rebuild the budget aggregate |

> **Security note**: The `key_hash` field is removed from all responses. The raw key (the `key` field) appears only once — in the successful response of `POST /ai-virtual-keys` and `POST /ai-virtual-keys/{id}/rotate`. Store it securely.

`budget_used`, `budget_used_decimal`, accounting counts/state/revision, and
`key_hash/key_prefix` are server-owned. Supplying them to create/PATCH returns 400. The
canonical amount fields are fixed-12-decimal strings:
`budget_limit_decimal/budget_used_decimal`.

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

# AI Model B — Same-tier traffic target (priority=1, low weight)
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

Reference the logical name `gpt4-tier` in the ai-proxy plugin:

```json
{
  "model_group": "gpt4-tier",
  "model_source": "config"
}
```

The gateway selects the best Provider by `priority`; within the same priority,
traffic uses interleaved weighted round-robin. Each `weight` must be between
`0` and `10000`, and the total does not need to equal 100 because weights are
relative. For primary/backup behavior instead of traffic splitting, give the
backup model a lower `priority`.

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
      "model": {"provider": "anthropic", "name": "claude-main"},
      "client_protocol": "openai",
      "auth": {"header_name": "x-api-key", "header_value": "sk-ant-..."}
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
      "model": {"provider": "anthropic", "name": "claude-main"},
      "client_protocol": "anthropic",
      "auth": {"header_name": "x-api-key", "header_value": "sk-ant-..."}
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
- **`targets`**: List of candidate targets after a match. Multiple targets use interleaved weighted round-robin based on `weight`.
- **`weight`**: Weight value, default `1`, maximum `10000`. Target weights determine the relative traffic ratio and do not need to add up to 100.

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

## 11. Usage Analytics and Cost Estimation

Kong-Rust records one metadata-only usage fact for every request whose resolved Route
plugin chain contains an enabled `ai-proxy`. This includes successful requests, gateway
rejections, gateway failures, upstream failures, client disconnects, and interrupted
streams. A request that does not match a Route, or whose plugin chain does not contain
`ai-proxy`, is not counted.

Usage analytics is an operational estimate, not a provider invoice, a lossless audit
ledger, or the accounting path used to enforce a Virtual Key budget. The proxy path only
attempts a non-blocking enqueue. A full queue, prolonged database outage, or process crash
can lose facts; these events are exposed through writer status and metrics.

### 11.1 Usage and result semantics

Provider-reported usage takes precedence. When an official field is absent, Kong-Rust can
use the request-side tokenizer estimate, and it derives `total_tokens` only when both
prompt and completion values are known. An unknown value is `null`, never a fabricated
zero.

Provider fields are normalized as follows:

- OpenAI and OpenAI-compatible use prompt/completion usage directly, retain official
  totals, and preserve cached/reasoning breakdowns when reported.
- Anthropic prompt usage includes input, cache-creation input, and cache-read input;
  output uses the final cumulative output value.
- Gemini completion includes candidate and thinking tokens; the thinking and cached
  token counts are also retained as breakdowns.

`usage.source` is `provider`, `estimated`, `mixed`, or `unavailable`. Result
`outcome` is one of `success`, `gateway_rejected`, `gateway_error`,
`upstream_error`, `client_disconnected`, or `stream_interrupted`. E2E latency covers
the complete gateway lifecycle. TTFT is available only when a parseable first streaming
event was observed. `cache_status` describes the Kong-Rust AI response cache and is one of
`not_configured`, `unavailable`, `bypass`, `miss`, or `hit`; it does not describe a
provider prompt cache.

### 11.2 Exact prices and cost states

Prices are USD per one million tokens. Input and output are resolved independently in
this order:

1. the corresponding AI Model override (`input_cost` or `output_cost`), including an
   explicit zero;
2. an exact model ID or explicit alias in the built-in, versioned price catalog;
3. no match.

`openai_compat` never inherits OpenAI prices automatically. Model create/update accepts
an exact decimal string (legacy finite JSON numbers remain accepted for compatibility).
`GET /ai-models` keeps the legacy numeric `input_cost` and `output_cost`, adds
`input_cost_decimal` and `output_cost_decimal`, and returns server-resolved
`effective_pricing`. Leave an override empty to use the catalog.

The standard cost formula is:

```text
(prompt_tokens × input_price + completion_tokens × output_price) / 1,000,000
```

Rates and costs are calculated as Decimal values and API responses use 12 fractional
digits, for example `"0.001100000000"`. The price version, snapshot date, and effective
period used by a request are stored with the fact, so later Model or catalog changes do
not rewrite historical cost.

The bundled catalog version `2026-07-26.1` contains:

| Provider | Model / explicit alias | Effective UTC | Input | Output |
|---|---|---|---:|---:|
| OpenAI | `gpt-5.6-sol` / `gpt-5.6` | from 2026-07-26 | 5.00 | 30.00 |
| OpenAI | `gpt-5.6-terra` | from 2026-07-26 | 2.50 | 15.00 |
| OpenAI | `gpt-5.6-luna` | from 2026-07-26 | 1.00 | 6.00 |
| Anthropic | `claude-fable-5` | from 2026-07-26 | 10.00 | 50.00 |
| Anthropic | `claude-opus-4-8` | from 2026-07-26 | 5.00 | 25.00 |
| Anthropic | `claude-sonnet-5` | 2026-07-26 to 2026-09-01 | 2.00 | 10.00 |
| Anthropic | `claude-sonnet-5` | from 2026-09-01 | 3.00 | 15.00 |
| Anthropic | `claude-haiku-4-5-20251001` / `claude-haiku-4-5` | from 2026-07-26 | 1.00 | 5.00 |
| Gemini | `gemini-3.6-flash` | from 2026-07-26 | 1.50 | 7.50 |
| Gemini | `gemini-3.5-flash` | from 2026-07-26 | 1.50 | 9.00 |
| Gemini | `gemini-3.5-flash-lite` | from 2026-07-26 | 0.30 | 2.50 |

The three GPT-5.6 catalog entries support prompt usage up to 272,000 tokens. A larger
request is `unsupported` unless both Model directions are explicitly overridden.
Provider prompt-cache charges, non-standard service tiers, built-in tools, non-text
modalities, and other additional pricing also remain `unsupported`; available token
usage is retained, but the gateway does not report a partial cost as complete.

`pricing.status` is `matched`, `unmatched`, `unsupported`, or `not_applicable`.
`cost.status` is `calculated`, `estimated`, `not_incurred`, or `unavailable`.
Machine-readable reason arrays explain unsupported or unavailable results. Only a request
known not to have reached a provider has a zero, `not_incurred` cost; unknown cost is
`null`.

### 11.3 Admin API

List request facts with `GET /ai-usage`:

```bash
curl -G http://localhost:8001/ai-usage \
  --data-urlencode 'start=2026-07-25T00:00:00Z' \
  --data-urlencode 'end=2026-07-26T00:00:00Z' \
  --data-urlencode 'provider_type=openai' \
  --data-urlencode 'size=100'
```

The default window is the most recent 24 hours. Explicit `start` and `end` must be
provided together, use RFC 3339, follow `[start, end)`, and span at most 90 days.
`size` defaults to 100 and is limited to 1–1000. Results are ordered by
`(started_at DESC, id DESC)`.

The first response returns opaque `snapshot` and `offset` values. Pass `offset` to obtain
the next stable page; concurrent writes are intentionally excluded from that snapshot.
Do not decode or construct cursor values in clients.

Common exact filters are:

```text
request_id, route_id, service_id, provider_id, provider_type,
requested_model, model_group, actual_model, virtual_key_id, consumer_id,
status_code, outcome, stream, cache_status, usage_source,
pricing_status, cost_status
```

`request_id` is an exact, case-sensitive 32-character lowercase hexadecimal value. The
API always queries the default workspace and rejects `workspace_id`.

Get totals and one optional breakdown with `GET /ai-usage/summary`:

```bash
curl -G http://localhost:8001/ai-usage/summary \
  --data-urlencode 'start=2026-07-19T00:00:00Z' \
  --data-urlencode 'end=2026-07-26T00:00:00Z' \
  --data-urlencode 'breakdown=day' \
  --data-urlencode 'timezone=Asia/Shanghai'
```

Breakdowns are `hour`, `day`, `provider`, `actual_model`, `model_group`,
`virtual_key`, `route`, or `service`. Time breakdowns accept an IANA `timezone`;
categorical breakdowns accept `limit` (default 10, maximum 100) and
`order_by=cost_usd|total_tokens|requests`. Hour breakdowns are limited to 31 days.

Summary token values are known subtotals and include known/unknown request counts and
coverage. `cost_usd_calculable_sum` is only the calculable subtotal. Pricing and cost
status counts show how much data is absent or unsupported, so neither value should be
presented as a complete provider bill. Summary and detail can share the same `snapshot`
for reconciliation.

Both endpoints return `meta.mode`, `meta.ephemeral`, capacity, node, earliest-record, and
restart semantics as applicable. Stable errors include:

| HTTP | `error_code` | Meaning |
|---:|---|---|
| 400 | `analytics_invalid_query` | Invalid parameter, range, filter, or cursor |
| 409 | `analytics_snapshot_expired` | A DB-less ring changed while paging |
| 501 | `analytics_unsupported_in_hybrid` | Analytics is unavailable in CP/DP Hybrid |
| 503 | `analytics_query_timeout` | The five-second query limit was reached |
| 503 | `analytics_query_unavailable` | The Store is temporarily unavailable |

### 11.4 Kong Manager

Open **AI Gateway → Usage Analytics**. **Usage Analysis** provides 24-hour, 7-day,
30-day, and custom ranges; calculable cost, requests, known token subtotals and coverage;
cost/token trends; and top actual-model and Virtual Key rankings. **Call Logs** provides
the metadata facts, exact filters, stable paging, and a detail view.

Filters are stored in the URL, including the browser IANA timezone, so refresh, history,
and shared links preserve the view. Model and Virtual Key tables offer **View usage**
drill-down links. Unknown values display as `—`; a real zero remains distinguishable
from an unpriced or unavailable value. DB-less and Hybrid states are shown explicitly
instead of being rendered as zero activity.

### 11.5 Runtime modes and configuration

| Mode | Storage and query behavior |
|---|---|
| Traditional + PostgreSQL | Batched persistence in `ai_usage_logs`; `ephemeral=false` |
| Traditional + `database=off` | This node's bounded in-memory ring; eviction at capacity and reset on restart |
| Hybrid control/data plane | Collection and upload are disabled; Admin query returns 501 where available |

DB-less does not write usage facts to the declarative configuration Store and does not
aggregate across nodes. Its snapshot is conservatively invalidated after eviction, which
returns 409 rather than a partial page.

Configure the writer and DB-less ring in `kong.conf`:

```ini
ai_usage_queue_capacity = 8192
ai_usage_batch_size = 256
ai_usage_flush_interval_ms = 500
ai_usage_shutdown_timeout_ms = 5000
ai_usage_dbless_capacity = 10000
```

All five values must be greater than zero. `ai_usage_queue_capacity` and
`ai_usage_dbless_capacity` are capped at `1000000`; `ai_usage_batch_size` is capped at
`1129` and must not exceed the queue capacity. The batch cap keeps one PostgreSQL INSERT
below the protocol parameter limit. Invalid values fail startup. PostgreSQL writes retry
with bounded backoff, and graceful shutdown drains only within the configured timeout.

`GET /status` includes `ai_usage_writer`. When the Prometheus status endpoint is enabled,
it also exposes `kong_ai_usage_writer_*` counters and the `queue_depth` /
`queue_capacity` gauges. Monitor at least dropped facts, queue-full drops, write failures,
retry exhaustion, shutdown-timeout drops, and DB-less evictions.

### 11.6 Privacy, retention, and operational limits

Usage facts store metadata only. They do not contain prompts, response bodies,
request/response headers, Authorization or API keys, provider authentication
configuration, Virtual Key plaintext, or `key_hash`. A Virtual Key name and non-secret
prefix may be stored for diagnosis. Facts keep Route, Service, Provider, Model, and
identity snapshots so deleting those configuration entities does not erase history.

The initial PostgreSQL implementation has no automatic retention, partitioning, archive,
export, or deletion API. `ai_usage_logs` therefore grows continuously. Monitor table and
index growth, database capacity, writer failures, and query latency; define external
operational controls until a dedicated retention feature is delivered. API query windows
are capped at 90 days, but that cap does not delete older data.

The initial release also does not provide cross-node DB-less aggregation, Hybrid DP-to-CP
upload, provider invoice reconciliation, discounts/taxes, or multi-currency. Virtual Key
budgets use the independent authoritative ledger described in Section 12; they are not
deducted from `ai_usage_logs`.

---

## 12. Virtual Key Quota and Budget Enforcement

### 12.1 Activation and configuration

A Virtual Key policy runs only when the same effective plugin chain contains all three
enabled plugins:

```text
ai-key-auth → ai-rate-limit(limit_by=virtual_key) → ai-proxy
```

Create a key with quota and a lifetime USD budget. Use an exact string for money:

```bash
curl -s -X POST http://localhost:8001/ai-virtual-keys \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "team-a",
    "rpm_limit": 60,
    "tpm_limit": 100000,
    "budget_limit_decimal": "100.000000000000",
    "allowed_models": ["gpt-4o-mini"]
  }'
```

The raw `key` appears only in this response. `rpm_limit/tpm_limit` accept `null` or an
integer in `1..=2^31-1`; `null` disables that dimension. Setting
`budget_limit_decimal=null` pauses the budget but preserves historical
`budget_used_decimal`. Clearing is rejected with 409 while pending or unresolved intents
exist.

Attach `ai-key-auth` and `ai-proxy` to the same Route, then attach `ai-rate-limit`
without plugin-level limits:

```bash
curl -s -X POST http://localhost:8001/plugins \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "ai-rate-limit",
    "route": {"name": "ai-full-stack"},
    "config": {"limit_by": "virtual_key"}
  }'
```

Missing authenticated identity returns 401. A missing `ai-proxy` returns 500 without
charging quota or creating a budget intent. Call the Route with the one-time key:

```bash
curl -i http://localhost:8000/ai/chat \
  -H 'Authorization: Bearer sk-kr-...' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}'
```

### 12.2 Quota window, headers, and errors

The current Memory adapter is a per-node, fixed 60-second window starting at the first
hit. The same key UUID shares one bucket across all covered Endpoints in the process.
Rename and rotation preserve the UUID and do not reset the window.

Configured dimensions return:

```text
X-RateLimit-Limit-Requests
X-RateLimit-Remaining-Requests
X-RateLimit-Reset-Requests
X-RateLimit-Limit-Tokens
X-RateLimit-Remaining-Tokens
X-RateLimit-Reset-Tokens
```

The corresponding three headers are omitted for an unconfigured dimension. A 429 also
returns `Retry-After`. Remaining is the atomic admission snapshot after reserving the
prompt estimate; final token settlement does not rewrite headers already sent.

A Virtual Key RPM rejection returns:

```json
{
  "error": {
    "message": "Virtual key request rate limit exceeded.",
    "type": "rate_limit_error",
    "param": null,
    "code": "requests_rate_limit_exceeded"
  }
}
```

TPM uses `tokens_rate_limit_exceeded` and
`Virtual key token rate limit exceeded.`. Budget and infrastructure errors are fixed:

| HTTP | code | Meaning |
|---:|---|---|
| 401 | `virtual_key_required` | The policy requires an authenticated Virtual Key |
| 403 | `budget_exhausted` | Persisted used amount has reached or exceeded the limit |
| 429 | `requests_rate_limit_exceeded` | RPM exceeded |
| 429 | `tokens_rate_limit_exceeded` | TPM exceeded |
| 500 | `ai_policy_chain_invalid` | Invalid Virtual Key policy chain |
| 503 | `quota_backend_unavailable` | Quota backend timeout, outage, or overload |
| 503 | `quota_backend_state_invalid` | Corrupt or conflicting quota idempotency state |
| 503 | `quota_backend_unsupported` | Quota is unsupported in this mode |
| 503 | `budget_accounting_unavailable` | Budget primary/owner is temporarily unavailable |
| 503 | `budget_accounting_unresolved` | Accounting requires reconciliation |
| 503 | `budget_accounting_unsupported` | Persistent budgets are unsupported in this mode |
| 503 | `budget_pricing_unavailable` | No safe pricing snapshot can be formed |

OpenAI/Responses clients receive the nested `error` object. When the client protocol is
known to be Anthropic, the gateway uses the Anthropic error envelope.

### 12.3 Lifetime budget semantics

A budget is a lifetime USD **persisted-consumption cutoff**. If
`budget_used >= budget_limit` at admission, the request returns 403 before the provider.
An admitted request settles after the response using the same Decimal price/usage
semantics as the usage fact, but it neither waits for nor depends on the analytics writer.

This is not a strict zero-overspend reservation. One request or several concurrent
in-flight requests can take usage above the limit; the next request is blocked. Manager
may show more than 100%. Raising the limit takes effect at the next authoritative check.
Rename, rotate, disable, and re-enable do not reset usage. Standard list-price estimates
are not provider invoices.

Important derived fields in a Virtual Key response include:

```json
{
  "quota_enforcement": "configured_local",
  "quota_backend": "memory",
  "quota_scope": "node",
  "quota_window_seconds": 60,
  "budget_limit_decimal": "100.000000000000",
  "budget_used_decimal": "83.250000000000",
  "budget_percentage_decimal": "83.250000000000",
  "budget_status": "warning",
  "budget_backend": "postgres",
  "auth_endpoint_count": 2,
  "enforced_endpoint_count": 2,
  "pending_intent_count": 0,
  "unresolved_intent_count": 0
}
```

The Manager Virtual Keys page consumes these server-derived values. It distinguishes
`unconfigured/awaiting_plugin/configured_local_partial/configured_local/unsupported`
quota, and
`unconfigured/paused/awaiting_plugin/active/warning/exhausted/unresolved/unavailable/
unsupported` budget states.

### 12.4 Ledger, reconciliation, and rebuild

Query unresolved entries. `status` can combine
`pending,unresolved,settled,resolved,waived`; `from/to` use RFC 3339:

```bash
curl -s \
  'http://localhost:8001/ai-virtual-keys/<key-uuid>/budget-ledger?status=unresolved&size=50'
```

`data` contains entries, `account` is the current aggregate, and `next_cursor` requests
the next page. Do not parse or construct cursors.

Settle against reviewed actual cost:

```bash
curl -s -X POST \
  http://localhost:8001/ai-virtual-keys/<key-uuid>/budget-reconciliations \
  -H 'Content-Type: application/json' \
  -d '{
    "intent_id": "<intent-uuid>",
    "operation_id": "<stable-operation-uuid>",
    "action": "settle",
    "cost_usd_decimal": "0.123000000000",
    "reason": "provider record reviewed"
  }'
```

To confirm that no cost was incurred, waive and omit the amount:

```json
{
  "intent_id": "<intent-uuid>",
  "operation_id": "<stable-operation-uuid>",
  "action": "waive",
  "reason": "provider confirmed request was not executed"
}
```

Network retries must reuse the same `operation_id`; changing the payload for an existing
ID returns 409. Start with a dry-run aggregate verification:

```bash
curl -s -X POST \
  http://localhost:8001/ai-virtual-keys/<key-uuid>/budget-ledger/rebuild \
  -H 'Content-Type: application/json' \
  -d '{
    "operation_id": "<stable-operation-uuid>",
    "reason": "scheduled ledger verification",
    "dry_run": true
  }'
```

Set `dry_run=false` only after reviewing `comparison`. A real rebuild uses checkpoint +
revision tail and a short key-lock CAS. A continuously changing hot account can return
409; retry later or during a maintenance window.

### 12.5 Runtime and capacity boundaries

| Mode | RPM/TPM | Persistent budget |
|---|---|---|
| Traditional + PostgreSQL | Per-node Memory, 60 seconds, resets on restart | Authoritative PostgreSQL primary ledger |
| standalone DB-less | Per-node ephemeral Memory | Unsupported; a configured budget fails closed |
| Hybrid CP/DP | Capability is unsupported | Unsupported; no DP-to-CP accounting |

Traditional uses separate bounded PostgreSQL pools for budget hot-path,
heartbeat/owner, and Admin/rebuild work, plus owner leases, stale recovery, and
checkpointing. Important defaults:

```ini
ai_quota_memory_max_buckets = 100000
ai_quota_memory_max_records = 2000000
ai_quota_memory_max_records_per_bucket = 100000
ai_quota_memory_max_live_reservations = 200000
ai_quota_memory_recovery_headroom = 50000
ai_quota_max_request_lifetime_ms = 900000
ai_quota_settlement_retry_grace_ms = 300000
ai_quota_cleanup_interval_ms = 30000
ai_quota_cleanup_scan_batch = 4096

ai_budget_pg_pool_size = 10
ai_budget_heartbeat_pg_pool_size = 1
ai_budget_admin_pg_pool_size = 2
ai_budget_max_concurrent_ops = 8
ai_budget_recovery_reserved_ops = 2
ai_budget_recovery_scan_batch = 100
ai_budget_operation_timeout_ms = 2000
ai_budget_lock_timeout_ms = 500
ai_budget_owner_lease_seconds = 30
ai_budget_owner_heartbeat_ms = 5000
ai_budget_intent_stale_grace_seconds = 60
ai_budget_active_intent_capacity = 50000
ai_budget_checkpoint_interval_seconds = 60
ai_budget_checkpoint_soft_tail_events = 10000
ai_budget_checkpoint_hard_tail_events = 100000
```

These defaults are not throughput SLAs. A normal budget request performs multiple
primary operations, and one hot key serializes aggregate updates. The three pools can
still share one PostgreSQL instance, WAL, disks, and CPU. Before production, benchmark
budget QPS, concurrency, hot-key skew, long SSE, ledger rows/day, pool and lock wait,
heartbeat lag, checkpoint tail, and failure recovery.

There is currently no Redis quota backend. N nodes using Memory can theoretically admit
roughly N times the configured quota; REQ-AI-009 will add Redis without changing the
`RateLimitStore` contract. There is also no Elasticsearch/OpenSearch, ClickHouse, or
Kafka usage/log backend; external analytics, retention, and migration belong to
REQ-AI-013. The budget ledger must remain on a strongly consistent Store that satisfies
atomic transactions, idempotency, audit, and reconciliation. Redis or ES is not a direct
replacement.
