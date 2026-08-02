# REQ-AI-014 方案设计 — 基于 Headroom 的上下文压缩与 CCR

> Headroom-based Context Compression with CCR — Solution Design
>
> - **状态：** ✅ 方案设计定稿（2026-08-01）
> - **实现验证修订：** 2026-08-01；按固定镜像真实 contract 收窄 Chat，补入
>   Responses tool transform，并改用私有网络/mTLS sidecar 边界
> - **需求分析：** [analysis.md](analysis.md)（FR-1~8、14 条验收标准与
>   10 项产品决策以其为准）
> - **实现验收：** [acceptance.md](acceptance.md)（功能通过；生产晋级受镜像漏洞阻塞）
> - **上游锁定：** `headroomlabs-ai/headroom@6d5516dcb878b6ffd139a1c7b3d480a1c8c1beb9`
>   / source `0.33.0` / Apache-2.0
> - **范围：** `kong-config` / `kong-plugin-system` / `kong-ai` / `kong-admin` /
>   `kong-server` / `kong-manager` / 文档与测试

## 1. 方案概述

首版采用 **Kong 原生策略插件 + Headroom 官方 proxy adapter**。策略插件先于
`ai-proxy` 执行，只把经过校验的策略写入 request context；`ai-proxy` 在已经选择
Provider、生成 Provider-native body 和认证头之后，调用 adapter 做健康预检并把
上游目标改成 Headroom。Headroom 收到的动态 Provider origin/path 只由服务端生成，
完成压缩、CCR store、`headroom_retrieve` 拦截和必要的 Provider continuation。

```text
客户端
  │ 原始 OpenAI / Anthropic 请求
  ▼
Kong-Rust
  ├─ ai-key-auth(774)            认证原文
  ├─ ai-prompt-guard(773)        检查原文
  ├─ ai-cache(772)               用原文生成 cache key
  ├─ ai-rate-limit(771)          按原文保守预扣
  ├─ ai-context-compression(770) 保存 route policy
  └─ ai-proxy(769)
       ├─ 解析/选模/Provider-native 转换
       ├─ 固定旁路判定 + Headroom /readyz 短 TTL 预检
       ├─ 清理客户端 x-headroom-* 控制头
       └─ applied 时覆写 target/path；Responses 注入固定 retrieve tool
              │
              ▼
        Headroom sidecar/proxy
          ├─ 压缩 + CCR store
          ├─ 调用 Kong 已选定的 Provider
          ├─ 必要时拦截 retrieve 并续调 Provider
          └─ 返回最终业务响应 + 内部 token headers
              │
              ▼
Kong header/body/log
  ├─ 采集/校验 before/after/saved
  ├─ 移除内部 Headroom headers
  ├─ 继续既有客户端协议转换和 usage 归一化
  └─ 输出低敏状态、指标和 usage 元数据
```

这不是把 Provider 路由权交给 Headroom。Headroom 没有 Provider 列表、模型组或
客户端可控 base URL；它只收到本请求已冻结的一个 origin/path 和既有 Provider
auth header。插件关闭或固定旁路时，原 `ai-proxy` target/body 不变。

## 2. 模块与依赖

### 2.1 `kong-ai::context_compression`

新增后端无关模块：

```text
crates/kong-ai/src/context_compression/
├── mod.rs        # capability、请求/route plan、错误、trait
└── headroom.rs   # 官方 Headroom proxy adapter
```

公共契约不暴露 reqwest、Headroom Python 类型或 store 实现：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionProtocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Debug, Clone)]
pub struct ProviderTarget {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct CompressionRoute {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub control_headers: Vec<(String, String)>,
    pub body_transform: Option<CompressionBodyTransform>,
}

#[async_trait]
pub trait ContextCompressionBackend: Send + Sync {
    fn descriptor(&self) -> CompressionBackendDescriptor;

    async fn prepare_route(
        &self,
        protocol: CompressionProtocol,
        provider: ProviderTarget,
    ) -> Result<CompressionRoute, CompressionBackendError>;
}
```

descriptor 声明 `transparent_ccr=true`、`streaming=false`、store scope
（`local|cluster`）和 backend ID；协议能力由 `prepare_route` 的结构化结果/错误表达。
未来 Rust SDK adapter
只有同时实现 response continuation lifecycle 才能声明 `transparent_ccr`；普通
inline compress adapter 不能冒充等价实现。

crate 依赖保持现有方向：adapter 位于 `kong-ai`，只依赖已经存在的 `reqwest/tokio`；
`kong-server` 负责从 `KongConfig` 构造并注入 `Arc<dyn ContextCompressionBackend>`，
`kong-core` 不依赖 Headroom。

### 2.2 `ai-context-compression` 插件

新增 `plugins/ai_context_compression.rs`：

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiContextCompressionConfig {
    pub min_input_tokens: u64,        // 2000
    pub max_input_bytes: usize,       // 4 MiB
    pub on_unavailable: UnavailablePolicy,
    pub streaming: StreamingPolicy,   // 首版只能 Bypass
    pub expose_metrics_headers: bool, // false
}

pub struct ContextCompressionContext {
    pub policy: AiContextCompressionConfig,
    pub outcome: ContextCompressionOutcome,
}
```

插件阶段职责：

- `access`：反序列化已经由共享校验确认的配置，写入 `ContextCompressionContext`；
- `header_filter`：读取 Headroom response headers、完成数值约束、计算 hop latency、
  设置稳定的 Kong headers（可选）并排队移除所有 Headroom response headers；
- `log`：把低敏结构写入 `log_serialize.ai.context_compression`，不写正文/hash；
- 不实现 `body_filter`，避免和 `ai-proxy` 的协议转换竞争。

### 2.3 `ai-proxy` 集成点

`AiProxyPlugin` 增加可空 backend：

```rust
pub struct AiProxyPlugin {
    // existing fields ...
    context_compression: Option<Arc<dyn ContextCompressionBackend>>,
}

pub fn with_context_compression_backend(
    mut self,
    backend: Arc<dyn ContextCompressionBackend>,
) -> Self;
```

保留 `new()` 和 `with_model_resolver()` 兼容测试/嵌入调用；server 使用 builder 注入
backend。没有配置 URL 时不构造 Headroom backend，插件的 `on_unavailable` 决定
旁路或拒绝。

## 3. 配置设计

### 3.1 `kong.conf`

```text
# 空值表示 Headroom backend 未配置
ai_context_compression_headroom_url =
ai_context_compression_health_timeout_ms = 200
ai_context_compression_health_ttl_ms = 1000
ai_context_compression_store_scope = local
```

校验/构造规则：

- URL 只允许绝对 `http/https`，必须含 host，不接受 userinfo、query、fragment；base
  path 允许非空并在 sidecar health/Provider path 拼接时规范化；
- `health_timeout_ms` 为 `1..=5000`，`health_ttl_ms` 为 `0..=60000`；
- `store_scope=cluster` 只是部署声明，必须由启动时 capability probe 或集成验证确认，
  首版无可验证 probe 时保守回落 `local` 并告警。

### 3.2 插件实体

```json
{
  "name": "ai-context-compression",
  "route": { "id": "..." },
  "enabled": true,
  "config": {
    "min_input_tokens": 2000,
    "max_input_bytes": 4194304,
    "on_unavailable": "pass_through",
    "streaming": "bypass",
    "expose_metrics_headers": false
  }
}
```

共享 `kong-plugin-system::config_validation` 增加 defaults 和 validator；Admin
`/schemas/plugins/ai-context-compression`、`/schemas/plugins/validate`、create/update/
upsert 与 runtime 全部调用同一规则。插件加入 `kong-config` 和 Admin 两份 bundled
列表及 server registry。

## 4. 请求决策状态机

### 4.1 状态

```text
Pending
  ├─ plugin_disabled / no_policy ───────────────► Bypassed
  ├─ body_too_large / below_threshold ─────────► Bypassed
  ├─ streaming / unsupported_provider/protocol/path ─► Bypassed
  ├─ tool_choice_unsupported ──────────────────► Bypassed
  ├─ backend_not_configured/unhealthy
  │     ├─ pass_through ───────────────────────► Bypassed
  │     └─ reject ─────────────────────────────► Rejected(503)
  └─ prepare_route ok ─────────────────────────► Applied
          ├─ compression-failed + business response ► Degraded
          ├─ valid metrics/business response ───────► Applied
          └─ Headroom/Provider error ───────────────► Degraded/Error
```

固定 reason 枚举：

```text
below_threshold, body_too_large, streaming, unsupported_provider,
unsupported_protocol, tool_choice_unsupported, unsupported_path,
backend_not_configured, backend_unhealthy,
backend_invalid_response, compression_failed, applied
```

状态和 reason 使用 Rust enum；日志/API 再序列化为 snake_case，避免任意上游字符串
造成指标高基数。

### 4.2 判定顺序

在两个 `ai-proxy` 请求分支（Responses pass-through 与普通 Chat）共用一个 helper：

1. 获取 `ContextCompressionContext`；没有则不做任何事；
2. 移除客户端请求中除服务端将覆盖项之外的所有 `x-headroom-*` headers；
3. 检查原始 body 字节数；
4. 检查原文 prompt token 估值；
5. 检查 `stream_mode`；
6. 按实际 Provider wire 选择 `CompressionProtocol`；
7. 校验 Provider path 可安全重建；
8. 调 backend `prepare_route`；
9. Responses 先检查 `tool_choice` 是否允许内部 retrieve tool，移除客户端同名定义并
   注入固定扁平 schema；不兼容时旁路且不修改 body；
10. 成功后原子地一次性覆写 target/scheme/path 并追加 control headers；任一步失败
    前不修改原 target，避免半改写。

这里使用 `ai-proxy` 生成的 Provider wire，而不是客户端 wire。例如 Anthropic
客户端请求被路由到 OpenAI Chat Provider 时，因冻结 transport 不能保证 CCR
continuation 而旁路；路由到 Anthropic Messages 时才进入 Headroom，响应仍由原有
driver 转回客户端格式。

## 5. Headroom route adapter

### 5.1 健康缓存

`HeadroomProxyAdapter` 持有一个 reqwest client 和
`tokio::sync::Mutex<HealthSnapshot>`：

```rust
struct HealthSnapshot {
    checked_at: Option<Instant>,
    healthy: bool,
}
```

TTL 内直接返回；TTL 过期后第一个请求持锁执行 `GET /readyz`，其余请求合并等待同一
探测，不形成健康检查惊群。HTTP 200 才视为健康；timeout、非 200 和无效 body 都是
结构化 backend error。`health_ttl_ms=0` 仅用于测试，不建议生产使用。

健康检查只说明 Headroom process 可接收请求，不说明 Provider 或 CCR store 必然
成功，因此它只支持“派发前安全旁路”。请求已进入 sidecar 后不做自动直连重放。

### 5.2 Provider target 重建

adapter 先验证原 target：scheme 是 http/https、host 非空、path 以 `/` 开始且不含
scheme/authority。然后生成：

| Provider wire | 发给 Headroom 的 path | 控制头 |
|---|---|---|
| OpenAI Chat | 不进入 Headroom | `unsupported_protocol` 旁路 |
| OpenAI Responses | `/v1/responses` | `x-headroom-base-url=<origin>` + `x-headroom-original-path=<原 path>` |
| Anthropic Messages | `/v1/messages` | `x-headroom-base-url=<origin + 去掉 messages 后的 prefix>` |

origin 由结构化 scheme/host/port 生成；默认端口省略，IPv6 使用方括号。不得从原始
客户端 header 或未经解析的字符串拼接。path 中的 query 保持在原 upstream URI；
如果不能无歧义拆分，返回 `unsupported_path` 旁路。

所有 applied 请求再加入：

- `x-headroom-stack: kong-rust`；
- 禁止由客户端覆盖 `x-headroom-bypass/mode/base-url/original-path` 或任意其他
  `x-headroom-*` 控制头。

Headroom 在 Provider 调用前会剥离这些 headers；Kong contract test 仍以 mock
Provider 验证，不能只信任上游实现。

### 5.3 Header 操作顺序

Pingora 当前先应用 `upstream_headers_to_set` 再 `upstream_headers_to_remove`。因此：

1. 遍历 `ctx.request_headers` 中所有 `x-headroom-*`；
2. 对服务端会覆盖的固定 header 不加入 remove queue，直接 set 覆盖；
3. 对其他客户端内部 header 加入 remove queue；
4. 最后追加 server-controlled set。

这样既不会让 remove queue 删除刚设置的值，也不会遗留未知 Headroom 控制头。

## 6. CCR store 与安全

### 6.1 支持部署档位

| 档位 | Store | 能力状态 | 用途 |
|---|---|---|---|
| 开发 | Headroom 默认 SQLite，本地目录 | `local_retrievable` | 单进程开发 |
| 单节点生产 | 持久加密卷 SQLite，多 worker 共享 | `local_retrievable` | 单网关节点/sidecar |
| 多节点生产 | Headroom 可替换共享 backend + tenant prefix，或验证过的会话粘滞 | `cluster_retrievable` | 多副本 |

sidecar 环境基线：

```text
HEADROOM_CCR_BACKEND=sqlite
HEADROOM_CCR_SQLITE_PATH=/var/lib/headroom/ccr_store.db
HEADROOM_CCR_TTL_SECONDS=1800
HEADROOM_LOG_MESSAGES=0
HEADROOM_SKIP_UPSTREAM_CHECK=1
HEADROOM_RETRY_MAX_ATTEMPTS=1
```

`HEADROOM_RETRY_MAX_ATTEMPTS=1` 是一次性派发基线：Kong 不在 Headroom 返回失败后
直连重放，Headroom 自身也不得在 Provider 结果未知时隐式重复请求。保持
`HEADROOM_NO_CCR` 未设置，并使用 `--mode token --no-cache --no-rate-limit`，
不启用 memory、learn、budget、MCP。Provider 凭据不通过 sidecar 环境变量配置，
而是由 Kong 每请求
传递。0.33.0 的 proxy token 与 OpenAI Provider `Authorization` 冲突，因此首版不
设置 `HEADROOM_PROXY_TOKEN`；sidecar 网络入口只允许 Kong 节点，retrieve/admin
surface 由 loopback、NetworkPolicy 或 mTLS 隔离，volume 权限为 sidecar 用户独占。

### 6.2 租户隔离

- Kong 不暴露 CCR hash 或 retrieve API；模型只能看到本次/本会话中的 marker；
- 共享 backend 必须使用部署级 tenant prefix，并由 Headroom request-scoped backend
  能力确认；如果 OSS adapter 无法证明 request-scoped tenant store，则多租户共享池
  只能标记 local/trust-domain，不宣称物理隔离；
- hash、原文和 tool result 不能进入 Kong DB、metrics label 或普通日志；
- TTL 到期、容器滚动和 volume 删除分别有清理验证。

## 7. 响应、错误与观测

### 7.1 Headroom metrics header 解析

`header_filter` 只接受十进制 `u64`，同时验证：

```text
after <= before
saved <= before
before - after == saved  （不满足时 metrics_invalid，不自行修正）
```

`x-headroom-compression-failed=true` 把状态改为 `degraded`。任何 header 缺失/坏值不
影响业务 body，只把 token metrics 置空并记录低基数 reason。

所有以 `x-headroom-` 开头的 response header 都加入 remove queue。显式公开时输出：

```text
X-Kong-AI-Context-Compression: applied|bypassed|degraded
X-Kong-AI-Tokens-Before: 1234
X-Kong-AI-Tokens-After: 456
X-Kong-AI-Tokens-Saved: 778
```

### 7.2 错误契约

派发前 `on_unavailable=reject`：

- OpenAI/Responses：HTTP 503，`error.type=server_error`，
  `error.code=context_compression_unavailable`；
- Anthropic client：HTTP 503，`type=error`，
  `error.type=api_error`，同一 code 放入稳定字段/消息；
- 不包含 sidecar URL、Provider auth、reqwest error 或内部 IP。

派发后的 Headroom/Provider response 继续由原 `AiDriver` 处理。连接层结果未知不
重放；这与 REQ-AI-005 未来的 Provider retry 必须共享“是否可能已派发”证据，不能
各自盲重试。

### 7.3 请求日志与 usage fact

`log_serialize.ai.context_compression`：

```json
{
  "status": "applied",
  "reason": "applied",
  "backend": "headroom_proxy",
  "ccr": true,
  "tokens_before": 1234,
  "tokens_after": 456,
  "tokens_saved": 778,
  "compression_ratio": "0.630470",
  "hop_latency_ms": 215
}
```

PostgreSQL usage fact 通过 forward-only migration 增加可空、低敏字段：status/reason、
before/after/saved、hop latency。DB-less ring 使用相同 Rust model。summary API 增加
saved token 总量、applied/bypass/degraded 计数和可用记录的加权压缩率；旧 cursor
和旧行均以 null/unknown 兼容。成本继续按 Provider `usage.input_tokens`，不能用
Headroom estimate 改写账单。

Prometheus 由 REQ-AI-004 的统一导出面消费时使用：

```text
kong_ai_context_compression_requests_total{provider,status,reason}
kong_ai_context_compression_tokens_before_total{provider}
kong_ai_context_compression_tokens_after_total{provider}
kong_ai_context_compression_tokens_saved_total{provider}
kong_ai_context_compression_hop_latency_seconds{provider}
```

若本需求编码时 exporter 尚未有 Rust-native 注册入口，先交付结构化 log/usage facts
并把 Prometheus 行列入同需求未完成项，不能声称已导出。

## 8. Manager 设计

### 8.1 Endpoint publisher

`GatewayEndpoint` 增加 `contextCompressionPlugin?: GatewayPlugin`，draft 增加：

```ts
contextCompression: {
  enabled: boolean
  minInputTokens: number
  maxInputBytes: number
  onUnavailable: 'pass_through' | 'reject'
  exposeMetricsHeaders: boolean
}
```

加载 Endpoint 时按 route ID + plugin name 关联；发布/编辑时：

- 开启且不存在：创建 route-scoped plugin；
- 开启且存在：PATCH enabled/config；
- 关闭且存在：删除 plugin；
- 新建流程失败时将 plugin ID 放入既有 rollback 栈，先删 plugin 再删 route/service；
- 删除 Endpoint 时把新 plugin 纳入清理列表。

UI 在 AI Endpoint 表单“策略”区展示开关和高级字段，并固定提示：

- 仅非流式 OpenAI Responses/Anthropic Messages；Chat、流式和受限 tool choice
  显式旁路；
- TPM/模型窗口按原文保守准入；
- CCR 原文默认保留 30 分钟，实际以 sidecar 为准。

卡片配置态：plugin enabled + server backend configured 为 `active`；plugin disabled/
不存在为 `off`；Admin status capability 未配置为 `unavailable`。Manager 不直连
Headroom，不显示 URL。

### 8.2 插件中心

Admin native schema 使通用 `PluginForm` 自动渲染字段。Endpoint publisher 提供专用
UX，但不另造一份校验规则；前端只做即时提示，后端 schema/runtime 是权威。

### 8.3 调用统计

usage detail/summary type 增加可空 compression 字段；列表显示 saved token 和状态，
summary 增加“上下文节省 token / 压缩率 / 旁路率”。只有 API 返回非空时展示，旧
数据库/DB-less unknown 不显示 0，避免把未知误报为无节省。

## 9. 版本与供应链

开发基线固定源码 commit 与 source version。2026-08-01 已从官方 GHCR registry
解析 OCI index，并校验 amd64 image config 的 revision label 与冻结 commit 一致：

```text
ghcr.io/headroomlabs-ai/headroom@sha256:800a7ead087a791d54b7253c6cd5f98e5964f20fcde42872838f987244e090cc
```

该 digest 是官方 non-root 多架构 index，源码版本 `0.33.0`、revision
`6d5516dcb878b6ffd139a1c7b3d480a1c8c1beb9`；生产不能用 `latest` 或可变 main tag
替代。晋级流程还需保留 Apache-2.0 LICENSE/NOTICE、Syft/CycloneDX SBOM 与
Grype/Trivy 扫描摘要。升级先跑本需求 contract + 冻结 eval，再滚动 sidecar；回滚
只切回旧 digest，CCR store schema 必须前后兼容或使用独立 volume snapshot。

## 10. 测试设计

### 10.1 Rust 单元/contract

- config defaults、边界、未知字段、Admin/runtime 同规则；
- URL/host/port/path 规范化，默认端口、IPv6、自定义 prefix、坏 path；
- health TTL、并发探测合并、timeout/非 200；
- protocol/provider/stream/threshold/bytes 固定旁路矩阵；
- server control headers 覆盖客户端值，未知 `x-headroom-*` 被移除；
- response metrics 合法/缺失/溢出/不一致/compression-failed；
- OpenAI/Anthropic 503 envelope；
- plugin priority：`774 > 773 > 772 > 771 > 770 > 769`。

### 10.2 本地 HTTP contract server

在 `kong-ai` tests 使用 Axum mock Headroom：

- `/readyz`；
- 记录 Kong 发来的 path/headers/body；
- 返回 Provider 形状的业务响应和 Headroom metrics headers；
- 模拟 timeout、503、坏 headers 和 compression-failed。

这组测试证明 Kong adapter 行为，但不证明真实 CCR。

### 10.3 真实 Headroom E2E

用固定 image/digest 启动 Headroom 与可编程 mock Provider：

1. 构造足够大的 tool result，Provider 第一次响应强制调用 `headroom_retrieve`；
2. 断言 Headroom 第二次 Provider 调用含取回的原文；
3. 断言客户端只收到最终业务响应；
4. Responses、Anthropic 分别执行真实双调用；Chat 断言
   `unsupported_protocol` 直连 mock Provider且不泄露内部工具；
5. streaming 与受限 tool choice 断言只直连 mock Provider；
6. 重启 Headroom 后在 TTL 内 retrieve，验证 SQLite volume；
7. sidecar 私网/mTLS、Provider `Authorization` 保真和客户端控制头攻击矩阵。

### 10.4 真实 Kong HTTP 与 Manager

- 8001 创建 Provider/Model/Service/Route/两个插件；8000 发请求，验证原文 guard、
  保守 TPM、Headroom route、响应转换和 usage fact；
- Manager `pnpm lint`、`pnpm build`、聚焦 E2E；
- 使用应用内浏览器创建/编辑 Endpoint，观察插件回显和提示，再用 Playground 发起
  非流式请求。

### 10.5 Eval 与负载

- 冻结 code/log/search/table/RAG/tool JSON/普通 prose 分层语料；
- 同 model/temperature/seed（Provider 支持时）做原文 vs Headroom 对照；
- 报告 token savings 分位数、任务成功、关键事实、安全指令、tool correctness；
- 4k/32k/128k 混合请求压单 sidecar，记录 QPS/CPU/RSS/p95/p99、队列饱和和
  pass-through/reject；
- 未达到 analysis AC-12 时保持 opt-in，并在实现记录写明具体失败集合。

## 11. 实施顺序与回滚

1. backend trait、Headroom adapter、配置和 contract tests；
2. policy plugin、共享 schema 校验、bundled/server 注册；
3. ai-proxy 两个请求分支统一 route helper、header 安全和响应观测；
4. usage model/migration/Admin summary；
5. Manager Endpoint publisher/插件通用表单/调用统计；
6. 双语 guide、部署清单、NOTICE/SBOM；
7. mock contract、真实 Headroom/Kong HTTP、UI、eval/负载。

运行时回滚只需禁用/删除 `ai-context-compression` 插件，ai-proxy 会恢复原 Provider
target；不需要数据库降级。新增 usage 列保持 forward-only、可空，旧二进制可忽略。
sidecar 可在无流量后停止；CCR volume 在确认没有 TTL 内活跃会话前不删除。

## 12. 编码完成检查表

- [x] backend trait/Headroom adapter 与固定 capability
- [x] `kong.conf` 字段、校验、脱敏和 server 注入
- [x] 插件/config validator/Admin schema/bundled/registry
- [x] ai-proxy priority 与请求分支共享 route helper
- [x] 客户端/Provider 两侧 `x-headroom-*` 安全 contract
- [x] 非流式 Responses/Anthropic 真实 CCR contract + Chat 安全旁路
- [x] streaming/threshold/tool choice/target path 旁路 contract
- [x] Gemini/max-body 与更多畸形请求 fixture
- [x] response headers 与结构化 log
- [x] usage migration/API/summary 与 Prometheus exporter
- [x] Manager publisher、配置状态与禁用/删除回滚
- [x] Manager 创建表单浏览器可见流程
- [x] Manager 调用统计压缩字段与完整 CRUD/Playwright
- [x] 中英文 guide、上游许可证说明与固定 digest
- [x] NOTICE/SBOM/漏洞扫描摘要（扫描发现 1 Critical + 2 High，生产晋级阻塞）
- [x] Rust 聚焦测试、受影响 crate check、DB-less 8001 schema/status、Manager
  lint/build/浏览器表单、真实 Headroom contract
- [x] PG/带 Headroom 的 8000/完整 Playwright/eval/负载证据
- [x] `git diff --check` 与实现记录
