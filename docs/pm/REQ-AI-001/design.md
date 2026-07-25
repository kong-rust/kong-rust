# REQ-AI-001 方案设计 — Virtual Key 运行时认证

> Virtual Key Runtime Authentication — Solution Design
>
> - **状态：** ✅ 已实现（2026-07-25）
> - **需求分析：** [analysis.md](analysis.md)（FR-1~9 与验收标准以其为准）
> - **设计日期：** 2026-07-25（基于代码现状二次核实）
> - **实现记录：** [task-15-4](../../implementation-logs/task-15-4_2026-07-25_virtual-key-auth.md)
>
> **实现期偏差（以实现记录为准）**：R1 的结论是 `PgDao::page` 支持 filters，但
> `ALLOWED_FILTER_COLUMNS` 白名单不含 `key_hash` 且**静默丢弃**未白名单的过滤条件。
> 处理方式为白名单加 `key_hash` + 在 `find_by_hash` 内本地复核返回记录的 hash
> （防御任何丢弃过滤条件的 DAO），未走原备选方案（本地 trait + 直接 SQL）。

## 1. 方案概述

新增独立插件 `ai-key-auth`（priority **774**，先于 ai-prompt-guard 773 执行）+ 共享认证器 `VirtualKeyAuthenticator`（kong-ai 新模块），复用现有插件短路机制在 access 阶段完成认证：

```
请求 → [代理层已缓冲 request_body / 已填充 request_headers·request_path]
     → ai-key-auth (774)  access:
         1. 凭证提取: authorization Bearer → x-api-key → 自定义 header
         2. SHA256(凭证) → VirtualKeyAuthenticator (moka 缓存, TTL 1s)
                              └─ miss → Dao::page(filters=[("key_hash", h)])
         3. 校验 enabled / expires_at            → 失败: 401 短路
         4. 校验 allowed_models(前缀通配, OR)     → 失败: 403 短路
         5. 注入 AiAuthContext + ctx.consumer_id + authenticated_credential
     → ai-prompt-guard (773) → ai-cache (772) → ai-rate-limit (771) → ai-proxy (770)
```

管理面失效路径：Admin CUD/rotate handler 成功后直接调 `authenticator.invalidate_all()`（AdminState 持有同一实例）；TTL 1s 作为兜底，满足 FR-7「≤1s 生效」。

可行性已核实的关键前提：

- 代理层在插件 access **之前**无条件预读请求体（`preload_request_body_for_plugins`，kong-proxy/src/lib.rs:747-757 → access 于 780），`ctx.request_body: Option<String>` 可直接读——白名单校验无需额外 body 处理
- `ctx.request_headers`（key 已 lowercase）、`ctx.request_path` 在 access 阶段均已填充
- `kong-admin` 已依赖 `kong-ai`（Cargo.toml:11），AdminState 可直接持有 kong-ai 类型
- `Dao::page` 的 `PageParams.filters: Vec<(String, String)>` 等值过滤在 DblessDao 已实现（内存过滤）；PgDao 侧待编码时确认（见 §8 风险 R1）

## 2. 关键设计决策

### D1 缓存与失效：moka(TTL 1s) + Admin 直接失效双保障

- `moka::sync::Cache<String, Option<AiVirtualKey>>`，key = key_hash，`time_to_live = 1s`，容量上限 10_000。缓存 `None`（负缓存）防止无效 key 反复打 DB；容量上限防恶意随机 key 填充。
- 热 key 代价 = 每节点每秒最多 1 次索引查询（`key_hash` 有 UNIQUE 索引），可接受；因此**不做**逐条失效的精细化管理，管理面变更一律 `invalidate_all()`（key 实体低频变更、缓存重建成本 ≈ 0）。
- 主动失效**不走** `refresh_tx`/100ms debounce 通道（该通道面向代理路由缓存重建，且 AI 实体分支缺失）；由 `ai_virtual_keys.rs` 的 create/update/delete/rotate 成功分支与 DB-less `post_config` 成功分支直接调用，语义即时、改动最小。

### D2 hash 查找：走通用 `Dao::page(filters)`，不改 kong-db

`VirtualKeyAuthenticator::find_by_hash` 用 `page(PageParams{ size: 100, filters: vec![("key_hash", hash)] , ..})` 翻页循环（正常首页即命中）。PG 与 DB-less 同一条代码路径：

- PG：filters 应转为 `WHERE key_hash = $n`，UNIQUE 索引 O(1)
- DB-less：内存等值过滤（`dbless.rs:385-397`），key 数量级小 + 1s 缓存，全表代价可接受

`AiVirtualKeyExt` trait 本单**不实现**（`update_budget` 属 REQ-AI-003；届时在 kong-ai 内为 `PgDao<AiVirtualKey>` impl 本地 trait 做原子 SQL 累加）。

### D3 错误体协议判定（FR-6「auto」的具体规则）

插件 config `error_format: "auto" | "openai" | "anthropic"`（默认 auto）。auto 判定按序：

1. 凭证提取自 `x-api-key`（且非 Bearer）→ **anthropic**
2. `ctx.request_path` 含 `/v1/messages` → **anthropic**（覆盖「无凭证 401」时无 header 信号的场景）
3. 其余 → **openai**

不读同 route 的 ai-proxy config（跨插件读配置需要代理层穿透 `resolved_plugins`，改动面大且插件间产生耦合；请求特征已足够判别，`error_format` 显式配置兜底特殊部署）。

错误体规范：

| 场景 | HTTP | OpenAI 格式 | Anthropic 格式 |
|---|---|---|---|
| 无凭证 | 401 | `{"error":{"message":"missing API key","type":"invalid_request_error","code":"missing_api_key"}}` | `{"type":"error","error":{"type":"authentication_error","message":"missing API key"}}` |
| key 无效/禁用/过期 | 401 | 同上，message=`"invalid API key"`、code=`"invalid_api_key"` | 同上，message=`"invalid API key"` |
| 模型不在白名单 | 403 | `{"error":{"message":"model `X` is not allowed for this API key","type":"invalid_request_error","code":"model_not_allowed"}}` | `{"type":"error","error":{"type":"permission_error","message":"model `X` is not allowed..."}}` |

短路时显式设置 `exit_headers = {"Content-Type": "application/json"}`（不依赖 `send_short_circuit_response` 的默认值）。

### D4 白名单校验的边界语义

- 仅当**请求体 JSON 中存在 `model` 字符串字段**时执行白名单校验；请求体缺失、非 JSON、或无 `model` 字段时**跳过校验放行**——因为 `model_source=config` 部署下客户端合法地不传 model（模型由网关配置决定），此时白名单无对象可校验。该边界写入 guide 文档。
- 匹配实现：`fn model_allowed(patterns: &[String], model: &str) -> bool`——按 FR-3：`p == "*"` 或 `p.strip_suffix('*')` 前缀匹配或全等，任一命中即 true。纯字符串操作，不引入 regex。

### D5 身份注入的三个落点

认证成功后（FR-4）：

1. `ctx.extensions.insert(AiAuthContext { virtual_key_id, key_name, consumer_id })`——下游 AI 插件（003 限流/预算、002 usage 落库）的主通道
2. `ctx.consumer_id = key.consumer_id`（如绑定）——激活 `ai-rate-limit limit_by=consumer` 与 balancer `HashOn::Consumer`
3. `ctx.authenticated_credential = Some(json!({"id":.., "name":..}))`、绑定 consumer 时 `ctx.authenticated_consumer = Some(json!({"id":..}))`——喂给 access log 现有读取点（kong-proxy/src/lib.rs:1807-1810）

注意：插件链按 `(route_id, service_id)` 预计算、`consumer_id` 恒以 None 参与解析——本单**不做** consumer 级插件链重解析（analysis.md 非目标）。

## 3. 模块详设

### 3.1 `crates/kong-ai/src/auth.rs`（新）

```rust
/// Shared virtual-key authenticator — 虚拟密钥共享认证器（插件与 Admin 失效共用）
pub struct VirtualKeyAuthenticator {
    dao: Arc<dyn Dao<AiVirtualKey>>,
    cache: moka::sync::Cache<String, Option<AiVirtualKey>>, // key = key_hash, TTL 1s
}

pub enum AuthError { MissingKey, InvalidKey }   // 不区分不存在/禁用/过期

impl VirtualKeyAuthenticator {
    pub fn new(dao: Arc<dyn Dao<AiVirtualKey>>) -> Self;
    /// raw key → SHA256 → 缓存/DB 查找 → enabled/expires 校验
    pub async fn authenticate(&self, raw_key: &str) -> Result<AiVirtualKey, AuthError>;
    pub fn invalidate_all(&self);
    async fn find_by_hash(&self, hash: &str) -> Result<Option<AiVirtualKey>>;  // D2
}

/// 插件间传递的认证上下文 — inserted into ctx.extensions
#[derive(Clone)]
pub struct AiAuthContext { pub virtual_key_id: Uuid, pub key_name: String, pub consumer_id: Option<Uuid> }

pub fn model_allowed(patterns: &[String], model: &str) -> bool;  // D4
```

`expires_at` 为 epoch 秒（`Option<i64>`），与 `chrono::Utc::now().timestamp()` 比较；SHA256 复用 kong-ai 已有 `sha2` 依赖，与 Admin `generate_key()` 的 lowercase hex 编码保持一致。

### 3.2 `crates/kong-ai/src/plugins/ai_key_auth.rs`（新）

```rust
#[derive(Deserialize)]
#[serde(default)]
pub struct AiKeyAuthConfig {
    pub key_header: String,     // 默认 "X-AI-Key"（第三优先级的自定义 header）
    pub error_format: String,   // "auto" | "openai" | "anthropic"，默认 "auto"
}

pub struct AiKeyAuthPlugin { authenticator: Arc<VirtualKeyAuthenticator> }
// PluginHandler: name="ai-key-auth", version="0.1.0", priority=774
```

`access` 流程即 §1 流程图第 1~5 步；凭证提取记录来源（`Bearer`/`x-api-key`/custom）供 D3 判定；`Bearer ` 前缀大小写不敏感。错误短路复用 `short_circuited + exit_status + exit_body + exit_headers` 惯例（先例 ai_prompt_guard.rs:61-70）。日志仅输出 key_prefix / key_name，绝不输出原文与完整 hash。

### 3.3 装配与登记（改动点清单）

| 位置 | 改动 |
|---|---|
| `crates/kong-ai/src/lib.rs`、`plugins/mod.rs` | 导出 `auth` 模块与新插件 |
| `crates/kong-server/src/main.rs` | `init_proxy_and_admin` 中构造 `Arc<VirtualKeyAuthenticator>`（DB-less line ~771 / PG 分支各一次，复用已有 `ai_virtual_keys` DAO）；`build_plugin_registry` 增参传入并 `register("ai-key-auth", ...)`；同一 Arc 存入 AdminState |
| `crates/kong-admin/src/lib.rs` | `AdminState` 增字段 `virtual_key_auth: Arc<VirtualKeyAuthenticator>` |
| `crates/kong-admin/src/handlers/ai_virtual_keys.rs` | create/update/delete/rotate 成功分支调用 `invalidate_all()`（update/delete 现为单表达式返回，需拆出 status 判断） |
| `crates/kong-admin/src/handlers/mod.rs` | `post_config`（DB-less 全量重建，line ~941）成功后 `invalidate_all()` |
| `crates/kong-config/src/config.rs` | `BUNDLED_PLUGINS` 增 `"ai-key-auth"`（否则 `is_valid_plugin_name` 拒绝创建；`ai-cache`/`ai-rate-limit` 缺失问题仍归 REQ-AI-006） |
| `crates/kong-admin/src/handlers/schemas.rs` | ① `rust_native_plugin_schema` 增 `ai-key-auth` 分支（key_header / error_format 两字段，仿 ai-proxy 写法）② 本文件私有 `BUNDLED_PLUGINS`（line 230-246）增名 ③ `get_known_config_fields` 增 `["key_header","error_format"]` |

### 3.4 前端（FR-8，同单交付）

| 文件 | 改动 |
|---|---|
| `useEndpointPublisher.ts` | 表单态增 `requireAuth: boolean`；`createEndpoint` 在 ai-proxy 插件之后创建 `ai-key-auth` 插件（route 级、config `{}` 走默认）；`CreatedResources` 增 `authPlugin` 槽位；`rollbackCreated` 逆序首位删除它 |
| `Endpoints.vue` / `EndpointIdentityForm.vue` | 发布向导「启用 Virtual Key 认证」开关（默认关）；端点列表可加认证徽章（可选增强） |
| `VirtualKeys.vue` + `Overview.vue` + `useAiGatewayI18n.ts` | 警告 banner 文案改写：「认证与模型白名单已生效；TPM/RPM/预算控制待 REQ-AI-003」（appearance 可从 warning 调为 info），两处文案 + 中英 i18n 同步 |
| `EndpointPlayground.vue` + `crates/kong-admin/src/handlers/ai_endpoint_test.rs` | Playground 增可选「API Key」输入（password 型）；请求体扩展为 `{path, request, api_key?}`；Admin 中转时若带 key 则设置 `Authorization: Bearer <key>` 转发；key 不写日志；Copy curl 同步带 `-H "Authorization: Bearer ..."` |
| `types.ts` | 相应类型补充 |

### 3.5 文档（FR-9）

`ai-gateway-guide.md` / `_cn.md` 新增「Virtual Key 认证」章节：启用方式（向导/手动挂插件）、凭证携带三种方式、白名单通配符规则与 model 缺省边界（D4）、错误体格式、DB-less 声明式配置需自带 `key_hash`（附生成命令示例）；`docs/design.md` AI 网关章节更新；`docs/tasks.md` 增实现条目。

## 4. 测试方案

单元测试（kong-ai）：

- `model_allowed`：精确 / 前缀通配 / 单独 `*` / 空列表 / 大小写敏感性
- 凭证提取优先级与 `Bearer` 大小写；error_format auto 判定三规则
- `authenticate`：有效 / 不存在 / disabled / expired / 负缓存命中；`invalidate_all` 后立即拿到新值

集成测试（`crates/kong-ai/tests/ai_key_auth_test.rs`，仿 ai_proxy_test 的 mock 上游模式）：

1. 未挂插件的 route 行为不变（回归）
2. 挂插件后：无 key 401 / 错误 key 401 / 禁用 401 / 过期 401，两种 error_format 的错误体断言
3. Bearer 与 x-api-key 两种携带方式均通过；白名单：命中精确、命中通配、被拒 403、请求体无 model 放行
4. 认证通过后下游可读 `AiAuthContext`、`ctx.consumer_id`（结合 ai-rate-limit limit_by=consumer 验证激活效果）
5. rotate/disable 后 ≤1s 失效（调 Admin API 后轮询断言）
6. DB-less：声明式配置载入 key（自带 hash）→ 认证通过；`post_config` 重载后旧 key 失效
7. PG 与 DB-less 双模式跑通（沿用 `make test` / `make test-dbless` 矩阵）

前端验证：`pnpm lint`、`pnpm build`、浏览器流程（向导开启认证发布 → Playground 无 key 401 → 带 key 200 → 回滚验证）。

## 5. 风险与开放点

| # | 风险 | 应对 |
|---|---|---|
| R1 | `PgDao::page` 对 `filters` 的支持未核实（探索仅确认 DblessDao） | 编码首日验证；若不支持，则在 kong-ai 内为 `PgDao<AiVirtualKey>` impl 本地 `AiVirtualKeyExt::get_by_hash` 直接 SQL（本地 trait + 外部类型，合法） |
| R2 | `send_short_circuit_response` 默认 Content-Type 未确认 | 显式设置 `exit_headers`（D3），不依赖默认 |
| R3 | 大请求体下重复解析 JSON（key-auth 与 ai-proxy 各解析一次） | MVP 接受（`serde_json::from_str` 只为取 model 字段可用 `RawValue`/局部解析优化，留待性能数据说话） |
| R4 | Playground 经 Admin 转发携带 key，扩大 Admin 面暴露 | Admin API 本身为受信面；key 不落日志、输入框 password 型、不回显存储 |

## 6. 编码任务拆分（🔨 阶段执行顺序）

1. `auth.rs`（authenticator + AiAuthContext + model_allowed）+ 单测 —— 含 R1 验证
2. `ai_key_auth.rs` 插件 + 错误体构造 + 单测
3. 装配与登记（main.rs / AdminState / handlers 失效 / BUNDLED_PLUGINS / schemas）
4. 集成测试（PG + DB-less）
5. 前端四处改动 + `pnpm lint/build` + 浏览器验证
6. 文档（guide 双语 / design.md / tasks.md）+ backlog 状态更新
