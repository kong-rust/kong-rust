# REQ-AI-001 需求分析 — Virtual Key 运行时认证

> Virtual Key Runtime Authentication — Requirement Analysis
>
> - **优先级 / Effort：** P0 / M
> - **需求分析定稿：** 2026-07-25（决策已与需求方确认）
> - **依赖：** 无
> - **需求单索引：** [../backlog.md](../backlog.md)
> - **方案设计：** [design.md](design.md)（🎨 阶段产出）

## 背景与价值

`ai_virtual_keys` 表字段完整（key_hash/allowed_models/expires_at/enabled 等），Admin CRUD + 轮换 + 脱敏、前端管理页均已交付，但代理运行时**没有任何代码读取 key**——认证、模型白名单全部不生效，前端页面挂着「尚未接入代理认证」警告。Key 治理是 LiteLLM/Portkey 等竞品的核心价值，也是「AI 网关」区别于「LLM 反向代理」的分水岭。本需求激活已有的三层存量投资（DB/Admin/前端）。

## 用户故事

作为 AI 网关运维者，我给每个团队/应用发放一把 `sk-kr-*` 虚拟密钥并限定其可用模型；客户端使用 OpenAI/Anthropic SDK 原样携带该密钥调用网关端点；无效、禁用、过期的密钥被拒绝；轮换密钥后旧密钥立即失效，全程无需客户端感知真实 provider 凭据。

## 现状事实与约束（2026-07-25 基于代码核实）

- key 格式 `sk-kr-` + 32 位 UUID hex；DB 存 SHA256 hex（无盐），`key_hash` 有 UNIQUE 索引，按 hash 查找天然 O(1) 且不受时序攻击影响；`key_prefix` 仅 8 字符（恒为 `sk-kr-XX`），只用于展示不用于查找
- 仓库**无任何认证先例**：`key-auth` 只存在于名单字符串，无实现；`RequestCtx.consumer_id` 与 `authenticated_consumer` 在生产代码中**从不写入**——本需求是第一个身份注入点
- `AiVirtualKeyExt` trait（`get_by_hash`/`update_budget`）已声明但全仓零实现、零调用
- AI 实体（providers/models/virtual_keys）的 Admin CUD **未接入** `refresh_tx` 缓存刷新通道；实体缓存先例是 `ModelGroupResolver` 的 2s TTL 轮询
- DB-less 已支持 `ai_virtual_keys` 声明式加载（用户须自带 key_hash）；**CP→DP 同步不含任何 AI 实体**（Hybrid 模式下 DP 无 virtual key 数据）
- 插件短路机制成熟（`short_circuited` + `exit_status/exit_body`），现有 AI 插件优先级 773~770，认证插件应取 774 抢先执行
- 无通用 OpenAI 风格错误 helper，现有插件错误体均为 `{"message": ...}` 非 SDK 标准格式
- `allowed_models` DDL 默认 `'{}'` 空数组——空数组语义必须为「不限制」

## 功能需求

- **FR-1 凭证提取**：按序尝试 `Authorization: Bearer <key>` → `x-api-key` → 自定义 header（默认 `X-AI-Key`，可配置，与 ai-rate-limit 的 `header_name` 默认值对齐），取到首个即用。兼容 OpenAI SDK（Bearer）与 Anthropic SDK（x-api-key）的默认携带方式。
- **FR-2 密钥校验**：SHA256(原文) → `get_by_hash` 查找。无凭证 → 401「missing API key」；查无此 key / `enabled=false` / `expires_at` 已过 → 401「invalid API key」（**不区分具体原因**，防止密钥状态探测）。
- **FR-3 模型白名单**：`allowed_models` 非空时，请求体 `model` 字段须匹配其中至少一项，否则 403（错误信息含被拒的 model 名）；空数组或 NULL 均为不限制。匹配规则：白名单项以 `*` 结尾时为前缀通配（如 `gpt-4*` 匹配 `gpt-4o`、`gpt-4-turbo`），否则精确匹配；白名单为 OR 语义，任一项命中即放行；`*` 仅允许出现在末尾（单独一个 `*` = 全部放行，等同不限制）。
- **FR-4 身份注入**：认证成功后写入 `ctx.extensions` 的 `AiAuthContext { virtual_key_id, key_name, consumer_id }`，并设置 `RequestCtx.consumer_id = key.consumer_id`（如绑定）。副作用：`ai-rate-limit` 的 `limit_by=consumer` 与 access log 的 consumer 字段首次真正可用。
- **FR-5 插件形态**：独立 `ai-key-auth` 插件（priority 774），可挂 route / service / global；未挂插件的 route 行为完全不变。需同步登记：插件注册（main.rs）、`BUNDLED_PLUGINS`、Admin schema（`rust_native_plugin_schema` + config 字段白名单）。
- **FR-6 错误响应**：401/403 错误体**按客户端协议自适应**——OpenAI 协议入口返回顶层 `{"error": {"message", "type", "code"}}`，Anthropic 协议入口返回 `{"type": "error", "error": {"type", "message"}}`；`Content-Type: application/json`。协议判定方式（读取同 route 上 ai-proxy 的 `client_protocol` 配置 / 插件自身 config 显式指定 / 请求特征推断）由方案设计定，判定不出时回退 OpenAI 风格。
- **FR-7 缓存与失效**：认证查找走进程内缓存（命中路径不查 DB）；AI 实体 CUD/rotate 接入 `refresh_tx` 失效通道，单节点上 create/rotate/disable 的生效延迟 ≤ 1s（准即时）；缓存机制细节由方案设计定。
- **FR-8 前端（同单交付）**：
  1. Endpoint 发布向导新增「启用 Virtual Key 认证」开关 → 同时创建 `ai-key-auth` 插件（`CreatedResources` 增加插槽，回滚顺序同步更新）
  2. VirtualKeys 页警告 banner **改写文案**（认证与模型白名单已生效；TPM/RPM/预算待 REQ-AI-003），不是整体移除
  3. Playground（`EndpointPlayground.vue` + Admin 侧 `ai_endpoint_test.rs`）支持输入并透传密钥 header
- **FR-9 文档**：`ai-gateway-guide.md` / `_cn.md` 新增认证章节（含 DB-less 声明式配置需自带 key_hash 的说明）、`design.md` 更新。

## 非功能需求

- 未启用插件的 route 零额外开销；缓存命中路径无 DB 查询
- 日志与错误信息不得出现 key 原文；不区分「不存在/禁用/过期」
- 命名 `ai-key-auth` 不与 Kong 官方插件（`key-auth`）冲突，属 kong-rust 扩展插件

## 非目标（本单不做）

- TPM/RPM/预算执行与 `budget_used` 扣减（REQ-AI-003）
- `anonymous` 匿名降级通行（Kong key-auth 语义，后续按需）
- Hybrid 模式 CP→DP 同步 AI 实体（当前同步通道不含 AI 实体，单独立项）
- consumer 级插件链动态重解析（既有架构限制，见 design.md 10.x）

## 关联缺陷（顺带核实，不在本单修复）

- `ai-rate-limit`、`ai-cache` 不在 `BUNDLED_PLUGINS` 白名单，Admin API 创建这两个插件会被 `is_valid_plugin_name()` 拒绝 → 归入 REQ-AI-006
- `limit_by=consumer` 恒为空桶（`consumer_id` 无写入点）→ FR-4 落地后自动缓解，REQ-AI-003 彻底处理

## 需求分析决策记录（2026-07-25，已与需求方确认）

1. 认证形态：**独立 `ai-key-auth` 插件**（priority 774），不做 ai-proxy 内置开关
2. 错误体格式：**按客户端协议自适应**（OpenAI / Anthropic 双风格，判定不出回退 OpenAI）
3. `allowed_models` 匹配：**支持前缀通配符**（`*` 仅限末尾，任一项命中即放行）

## 验收标准

1. 启用 `ai-key-auth` 的 route 上：缺失 / 无效 / 已禁用 / 已过期的 key 请求被 401 拒绝，错误体为按协议自适应的 SDK 兼容格式；未启用的 route 行为不变
2. OpenAI SDK（Bearer）与 Anthropic SDK（x-api-key）默认携带方式均可通过认证
3. 请求 model 不在 `allowed_models`（非空时）返回 403；空数组/NULL 不限制；前缀通配符按 FR-3 规则生效
4. 合法 key 请求正常代理，下游插件可从 ctx 读到 `AiAuthContext`，绑定 consumer 时 `ctx.consumer_id` 有值
5. Key 轮换/禁用后旧 key 在 ≤1s 内失效（缓存失效路径验证）
6. 前端三处变更可用：向导开关创建认证插件并可回滚、VirtualKeys 页文案更新、Playground 可带 key 调试
7. 集成测试覆盖以上场景，PG 与 DB-less 两种模式均通过
