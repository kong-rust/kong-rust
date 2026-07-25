# Task 15.4 — Virtual Key 运行时认证（REQ-AI-001）

- **日期：** 2026-07-25
- **需求分析：** [../pm/REQ-AI-001/analysis.md](../pm/REQ-AI-001/analysis.md)
- **方案设计：** [../pm/REQ-AI-001/design.md](../pm/REQ-AI-001/design.md)

## 交付内容

新增 `ai-key-auth` 插件，把已有的 `ai_virtual_keys`（DB 表 + Admin CRUD + Manager 页面）接入运行时。此前这三层投资在代理路径上完全未被读取。

### 后端

| 文件 | 说明 |
|---|---|
| `crates/kong-ai/src/auth.rs`（新建） | `VirtualKeyAuthenticator`（查表 + 缓存 + 失效）、`AiAuthContext`、`model_allowed()` |
| `crates/kong-ai/src/plugins/ai_key_auth.rs`（新建） | 插件本体：凭证提取、校验、错误体构造、身份注入 |
| `crates/kong-server/src/main.rs` | 两个模式分支各构造一个共享 `Arc<VirtualKeyAuthenticator>`，注册插件并存入 AdminState |
| `crates/kong-admin/src/lib.rs` | `AdminState.virtual_key_auth` |
| `crates/kong-admin/src/handlers/ai_virtual_keys.rs` | create/update/delete/rotate 成功后失效认证缓存 |
| `crates/kong-admin/src/handlers/mod.rs` | DB-less `POST /config` 全量重建后失效认证缓存 |
| `crates/kong-admin/src/handlers/ai_endpoint_test.rs` | Playground relay 转发 `api_key` 为 Bearer；手写 Debug 避免凭证进入调试输出 |
| `crates/kong-admin/src/handlers/schemas.rs` | 插件 schema、私有 BUNDLED_PLUGINS、config 字段白名单 |
| `crates/kong-config/src/config.rs` | `BUNDLED_PLUGINS` 增 `ai-key-auth`（否则 Admin API 拒绝创建该插件） |
| `crates/kong-db/src/dao/postgres.rs` | `key_hash` 加入 `ALLOWED_FILTER_COLUMNS` |

### 前端（同需求单交付）

发布向导「要求虚拟密钥」开关（创建/更新/删除/回滚全链路）、`AiEndpoint.requireAuth` 状态派生、Virtual Keys 与 Overview 文案从「尚未接入」改为已生效范围、Playground 可选密钥输入（password 型，Copy curl 同步带上）。

## 关键实现决策

1. **独立插件而非 ai-proxy 内置开关** — 职责单一，未来 MCP / Agent 子网关可复用，未挂插件的 Route 行为完全不变。priority 774 先于全部现有 AI 插件（773~770）。

2. **hash 查表走通用 `Dao::page` filters** — PG 与 DB-less 同一条代码路径，无需为 `AiVirtualKeyExt` 写双实现（该 trait 留给 REQ-AI-003 的 `update_budget`）。
   - **实现中发现**：`PgDao::page` 的 `ALLOWED_FILTER_COLUMNS` 白名单原本只有 `custom_id/username/name/host`，不在其中的过滤条件被**静默丢弃**（返回全表首页而非报错）。除了把 `key_hash` 加入白名单，`find_by_hash` 还会在本地复核返回记录的 `key_hash`——任何 DAO 丢弃过滤条件时都不可能放行无关密钥。该防御路径有专门的集成测试。

3. **缓存 TTL 1s + Admin 即时失效双保障** — 主动失效不走 `refresh_tx`/100ms debounce 通道（该通道面向路由缓存重建，且 AI 实体分支缺失），由 handler 直接调用 `invalidate_all()`。含负缓存，无效密钥不会每次请求打库；容量上限 10k 防随机密钥探测填充。

4. **错误体按客户端协议自适应** — 判定只用请求特征（凭证来自 `x-api-key`、路径含 `/v1/messages`），不跨插件读 ai-proxy 的 `client_protocol`（需要穿透 `resolved_plugins`，改动面大且引入插件耦合）。`error_format` 可显式覆盖。

5. **白名单只在请求体含 `model` 时校验** — `model_source=config` 部署下客户端合法地不传 `model`，若无条件校验会误杀。

6. **不区分失败原因** — 密钥不存在 / 已禁用 / 已过期返回完全相同的 401，避免密钥状态被探测。

## 验证

- **单元测试 26 个**：通配符匹配边界（含前导 `*` 不作通配、大小写敏感）、凭证提取优先级、Bearer 大小写、Basic auth 不误认、error_format 三条判定规则、两种错误体结构。
- **集成测试 25 个**（`crates/kong-ai/tests/ai_key_auth_test.rs`）：拒绝路径、三种凭证载体、白名单、身份注入、缓存命中计数（5 次请求仅 1 次查库）、禁用/轮换/删除后即时失效、DAO 丢弃过滤条件时的防御性校验。
- **真实 E2E 16 项全过**：PostgreSQL 实例 + 真实 Admin API 建资源 + 真实 HTTP 经代理，覆盖未挂插件 Route 不受影响、401/403 各场景与错误体、三种凭证载体、`gpt-4*` 通配放行与 `gpt-3.5-turbo` 拒绝、PATCH 禁用/rotate/DELETE 后立即生效。
- 回归：`cargo test -p kong-ai`、`-p kong-admin` 全绿；`pnpm lint`（含 vue-tsc）、`pnpm build` 通过。

## 顺带发现的既有缺陷（不在本需求范围，已单独记录）

1. **DB-less 代理路由从不加载**：DB-less 分支 `KongProxy::new(&[], ...)` 传空路由集，而唯一的补充路径 `refresh_proxy_cache` 不认 `POST /config` 发出的 `"config"` 实体类型（落到 `_ => {}`）——声明式配置下代理始终返回 `no Route matched`。本任务因此改用 PostgreSQL 模式做 E2E。
2. **`DblessStore::load_from_file` 不支持 YAML**：`.yml`/`.yaml` 分支实际调用 `serde_json::from_str`（源码注释自承「此处简化处理」），YAML 声明式配置无法加载。
3. `ai-rate-limit` 与 `ai-cache` 不在 `kong-config::BUNDLED_PLUGINS` 中，Admin API 创建这两个插件会被 `is_valid_plugin_name()` 拒绝（归入 REQ-AI-006）。

## 后续

`tpm_limit` / `rpm_limit` / `budget_limit` / `budget_used` 仍只存储不生效，归 REQ-AI-003（依赖 REQ-AI-002 的成本数据）。Hybrid 模式 CP→DP 同步不含 AI 实体，DP 节点无 Virtual Key 数据，需单独立项。
