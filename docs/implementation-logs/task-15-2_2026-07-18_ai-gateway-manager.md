# Task 15.2: AI Gateway Manager 与数据库 Model Group 运行时

## 目标

让用户只通过 Kong Manager 页面即可创建 AI Provider、Model Group 和可运行的
Chat Completions 代理路由，同时保证 Provider 凭据只保存在服务端。

## 实现

- 新增 `ModelGroupResolver`，从 PostgreSQL 或 DB-less DAO 加载同名
  `ai_models` 及其 `ai_providers`，按 priority、weight 和
  `max_input_tokens` 选择实际模型。
- `ai-proxy` 的解析顺序为插件内 `model_routes`、内联 Provider、数据库
  Model Group。OpenAI Responses 透传也会把顶层 `model` 改为解析后的真实模型名。
- 新增 `004_ai_model_max_input_tokens` forward migration，并补齐 PostgreSQL
  schema、DB-less 实体加载、endpoint/FK 索引和分页行为。
- Kong Manager 新增 `/ai-gateway` Overview、Providers、Models 和 Virtual Keys
  页面。Route 向导创建 Service、Route 和 route-scoped `ai-proxy` 插件；任何一步
  失败都会清理本次已创建的资源。
- Provider 读取继续遮蔽敏感字段；页面编辑不会把 `***` 当成新凭据提交。
  Virtual Key 明文只在创建或轮换响应中显示一次。
- 修复代理响应路径直接修改 Pingora `ResponseHeader.headers` 导致 preserved-case
  map 失配的问题。所有请求/响应 Header 变更改用 `insert_header` /
  `remove_header`，避免 HTTP/1 序列化时 panic 和客户端收到空响应。

## 验证

- `cargo test -p kong-ai --locked`
- `cargo test -p kong-db --lib --locked`
- `cargo test -p kong-admin --test ai_proxy_schema --locked`
- `cargo test -p kong-proxy --locked`
- `cargo check -p kong-server --locked`
- `pnpm lint`
- `pnpm build`
- AI Gateway Playwright E2E：页面 CRUD、Provider 依赖保护、Virtual Key
  一次性展示与轮换、Route 创建、真实代理 POST、上游模型名改写和资源清理
- PostgreSQL 实例确认 migration `004_ai_model_max_input_tokens` 已执行，列类型为
  `integer`

## 当前边界

- 页面 Route 向导当前只创建 `llm/v1/chat`；Responses 和 Anthropic 客户端协议仍通过
  Admin API 配置。
- Virtual Key 尚未接入代理认证、allowed-model、RPM/TPM 或预算执行。
- Model Group 的 `max_input_tokens` 过滤发生在 Provider 选定前，因此使用
  provider 无关字符估值；Provider/Model 选定后才使用 Tokenizer Registry 精细计数。
- 未对付费第三方 Provider 发送真实请求；端到端代理使用本地
  OpenAI-compatible mock。
