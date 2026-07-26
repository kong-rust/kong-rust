# REQ-AI-002 方案设计 — Token 成本核算与用量事实表

> Cost Accounting & Usage Facts — Solution Design
>
> - **状态：** ✅ 方案设计定稿（2026-07-26）
> - **需求分析：** [analysis.md](analysis.md)（FR-1~11、10 条验收标准与 10 项产品决策以其为准）
> - **下一门禁：** 编码实现
> - **范围：** `kong-core` / `kong-plugin-system` / `kong-proxy` / `kong-ai` /
>   `kong-db` / `kong-admin` / `kong-server` / `kong-manager` / 文档与测试

## 1. 方案概述

本需求新增一条独立于请求转发的 best-effort analytics 链路。代理热路径只维护
内存状态和执行一次 `try_send`，数据库写入、查询和汇总均在请求转发之外完成。

```text
KongProxy::new_ctx
  └─ RequestLifecycle（request ID、UTC/单调时钟）
       │
Route 命中并解析有效插件链
  ├─ 立即保存 resolved_plugins
  └─ AiUsageCollector::begin（仅当有效链含 ai-proxy）
       │
rewrite/access/body/transport
  ├─ 通用生命周期：短路、错误、上游尝试、响应/流终态
  └─ AI 草稿：身份、选模、usage、TTFT、计价特征
       │
KongProxy::logging
  └─ AiUsageCollector::finalize（幂等、无 I/O）
       ├─ 归一化 usage → 解析请求时价格 → Decimal 成本
       ├─ Arc<AiUsageFact> 放回 extensions，供 ai-proxy.log 复用
       └─ bounded mpsc::try_send
             │
             ├─ traditional + PostgreSQL → 批量 INSERT ai_usage_logs
             ├─ traditional + DB-less    → 本节点有界内存 ring
             └─ Hybrid CP/DP             → 不启用 collector，查询 501
                         │
                         └─ AiUsageStore
                              ├─ GET /ai-usage
                              ├─ GET /ai-usage/summary
                              └─ Kong Manager「调用统计」
```

核心边界：

- `kong-proxy` 不依赖 `kong-ai`，通用生命周期类型放在 `kong-core`，观察器接口放在
  `kong-plugin-system`；
- AI 事实、归一化、价表、Decimal 计算、PG/内存 Store 与 writer 统一归
  `kong-ai::usage`；`kong-db` 只持有 forward-only migration；
- 不复用通用 `Dao<T>`：它不支持时间窗、倒序复合游标、批写、稳定 snapshot 或
  SQL 聚合；
- `ai_usage_logs` 是请求级事实，不是 provider attempt 表，也不是 REQ-AI-003 的
  原子预算账本。

## 2. 关键设计决策

### D1 使用独立生命周期观察器，最终收口不依赖普通插件 log 顺序

新增同步、不可阻塞的 `RequestLifecycleObserver`。`AiUsageCollector` 在有效插件链
刚解析后初始化草稿，在 `KongProxy::logging()` 已得到最终状态和 Pingora 错误后
定稿。它不作为普通 `ai-proxy.log` 的副作用，原因是：

- 当前 request body 预载、rewrite/access 返回 `Err` 时尚未保存插件链，普通 log
  会整体缺失；
- 普通 log 链在更高优先级插件返回错误时会提前结束，不能承担“一请求一事实”的
  最终保证；
- transport 终态只在外层 `KongCtx` / `logging(error)` 可完整获得。

`ai-proxy.log` 优先读取 collector 已生成的 `AiUsageFact` 并继续填充现有
`log_serialize`；若 Hybrid、嵌入式构造或单测未装 observer，则回退当前
`AiRequestState` 逻辑，不能让既有 `ai.*` 兼容日志消失。
`logging.log_statistics=false` 只关闭这份兼容日志，不关闭事实采集。
`AiUsageContext.finalized` 和数据库 `UNIQUE(request_id)` 提供进程内、落库两层
幂等。

兼容日志开关按
`config.logging.log_statistics.unwrap_or(config.log_statistics)` 解析：Kong 官方
嵌套字段存在时优先，旧版顶层字段作为回退，二者都缺失时沿用默认 `true`。collector
永不读取该开关。当前 `ai-proxy.log` 尚未使用传入配置，编码时需补上上述解析，但
不得借机改变 `log_payloads` 或 REQ-AI-004 的 `ai_metrics` 契约。

### D2 通用生命周期与 AI 状态分层，保持 crate 单向依赖

- `kong-core::traits::plugin`：`RequestLifecycle`、`RequestTerminationHint`、
  `RequestTransportError` 和 route/service/workspace 快照；
- `kong-plugin-system::lifecycle`：`RequestLifecycleObserver`；
- `kong-proxy`：产生并更新通用生命周期，调用观察器；
- `kong-ai::usage`：实现观察器并把 `AiUsageContext` 放入 `RequestCtx.extensions`。

`KongProxy::new()` 保持默认无观察器，通过
`with_lifecycle_observers(Vec<Arc<dyn RequestLifecycleObserver>>)` 装配，避免破坏
大量现有测试构造器。`kong-server` 只在受支持的 traditional 模式注入 collector。

### D3 金额全程使用 Decimal，事实表固定 12 位小数

新增 workspace 依赖 `rust_decimal`，为 `sqlx` 开启 `chrono` 与
`rust_decimal` feature。生产计价不再调用当前基于 `f64` 且把未知当零的
`calculate_cost`：

- 单价、请求成本：Rust `Decimal`；
- PG 单价和成本：`NUMERIC(28,12)`；
- 公式中使用 checked Decimal 运算，结果按 12 位小数、half-up 舍入；
- 除既有 Model number 兼容投影外，API 单价、成本及覆盖率一律为十进制字符串；
- Manager 只格式化服务端字符串，不用 JavaScript 浮点重新求和。

`AiModel.input_cost/output_cost` 同步从 `Option<f64>` 改为 `Option<Decimal>`，
PG 列在 migration 006 中由 `DOUBLE PRECISION` 转成 `NUMERIC(28,12)`。通用
`PgDao` 新增仅供定点金额使用的 `ColumnType::Decimal` / `SqlParam::Decimal`，
不改变其他实体的 Float 契约。Model CUD 的规范输入为 `null` 或非负十进制字符串；
为兼容已有客户端，首版仍接受可无损转为 12 位 Decimal 的有限 JSON number，
handler 在进入 DAO 前统一规范化。

现有 `/ai-models` 的 `input_cost/output_cost` 响应保持 JSON number，作为兼容投影；
同一 response 新增 `input_cost_decimal/output_cost_decimal` 12 位字符串作为精确
表示，所有新增的 effective pricing、事实、成本和汇总金额也只返回字符串。所有
会返回 Model 的公开端点统一经过 `AiModelApiView`，避免某条嵌套 API 泄漏内部
Decimal 序列化形态。Manager 读写 decimal 字符串字段，不用兼容 number 做计算。
负数、NaN、Infinity、超过 `NUMERIC(28,12)` 范围或多于 12 位且无法无损量化的
输入返回字段级 400，不做静默舍入。

`AiModel` 金额字段使用共享 `decimal_12_option` serde adapter，而不是只在 Admin
handler 转换：string/legacy number 都可反序列化为 Decimal，内部序列化使用规范
字符串。DB-less YAML/JSON、DAO materialization 与未来集群实体路径均复用该
adapter；Admin/声明式公开导出再用 view 保留 legacy number，并同时给出 decimal
字符串，保证 standalone 的既有数字配置可继续载入。

### D4 provider usage 先归一化，再计价

扩展现有三字段 `TokenUsage` 为带字段来源和合并语义的 `UsageAccumulator`。
provider codec 只负责生成 `UsageObservation`；collector 统一处理：

- provider 官方 total 优先；
- 缺字段不清零、不伪造官方 0；
- 累计 stream snapshot 取最新，不逐 chunk 相加；
- prompt/completion 的 provider/estimated 来源分别保存，再派生请求级
  `usage_source`；
- reasoning/thinking 与 provider prompt-cache 作为 breakdown 保存，不重复计入
  标准化主字段。

### D5 价表是版本化数据资产，匹配规则无模糊猜测

新增 `crates/kong-ai/data/model_prices.json`，用 `include_str!` 编译进二进制。
文件声明 schema/catalog 版本、快照日期、官方来源 URL、provider、精确模型 ID、
显式 alias、有效期、input/output 价及条件。启动时一次解析和全量校验；数据非法
直接启动失败，不在请求时静默退化。

匹配顺序为：

1. DB-backed `AiModel` 的分方向显式覆盖（`Some(0)` 合法）；
2. `provider_type + actual_model` 的精确 ID；
3. 同一 provider 下价表显式声明的 alias；
4. 价表显式声明且有边界的受控 prefix（首版价目不使用 prefix）；
5. unmatched。

不匹配动态 `latest`，不按 substring/fuzzy 猜测，`openai_compat` 不继承 OpenAI
价。每个方向独立保存 source/version/snapshot/effective period。

Model override 每个方向的版本为
`model:<id-or-inline>:<updated_at-or-load-time>:<direction-price-hash>`，hash 始终
包含规范化 provider/model、方向和该方向价格，避免秒级 `updated_at` 内两次更新
碰撞，也避免只改 output 时无谓改变 input 版本。snapshot date 与 effective-from
使用实体更新时间；缺失时使用声明式配置本次成功载入时间，effective-to 为 null。
这样单向 override 与内置价混用时，两方向仍各有独立且可追溯的版本。

### D6 writer 有界、非阻塞、可观测

默认参数：

| 参数 | 默认值 | 说明 |
|---|---:|---|
| `ai_usage_queue_capacity` | 8192 | `mpsc` 有界队列 |
| `ai_usage_batch_size` | 256 | 单次批写上限 |
| `ai_usage_flush_interval_ms` | 500 | 未满批次的最大等待 |
| `ai_usage_shutdown_timeout_ms` | 5000 | 关闭 drain 上限 |
| `ai_usage_dbless_capacity` | 10000 | DB-less 本节点事实上限 |

PG 写失败采用初次尝试加 3 次有界重试，退避 50/100/200ms；最终失败按行计入
drop 后继续处理后续批次，不把数据库背压传回代理。队列满或 receiver 关闭时
`try_send` 立即失败，代理响应不受影响。

### D7 snapshot 使用写入水位，分页使用 `(started_at, id)` 倒序键

每个事实同时有内部 `ingest_seq`。snapshot 固定 `high_watermark`，后续明细与
汇总只读 `ingest_seq <= high_watermark`；offset 再携带末项
`(started_at, id)`，SQL 使用严格 `<` 和 `LIMIT size + 1`。

PG 多 writer 需避免 identity “先分配、后晚提交”造成旧 snapshot 后来新增行。
每个批写事务在 INSERT 前取得同一事务级 advisory lock：

```sql
SELECT pg_advisory_xact_lock(
  hashtextextended(current_schema() || '.ai_usage_logs', 0)
);
```

因此序号分配与提交按批串行，已提交 `MAX(ingest_seq)` 是安全高水位。锁只覆盖
批写事务，256 行摊薄后不进入代理路径。

### D8 运行模式明确分叉

| 模式 | 写入 | 查询 | API 语义 |
|---|---|---|---|
| traditional + PostgreSQL | PG 批写 | primary pool | 持久、`ephemeral=false` |
| traditional + `database=off` | 内存 ring | 本节点 | 易失、容量/最早时间/重启清空提示 |
| control_plane | 禁用 | 不支持 | 501 `analytics_unsupported_in_hybrid` |
| data_plane | 禁用 | 不支持 | 若 Admin 可达，同样 501；不做 DP→CP 上传 |

DB-less snapshot 同时保存
`high_watermark + eviction_generation + ring_instance_id`；淘汰 generation 变化
或 Store 实例变化即保守返回 409，不继续返回可能不完整/跨重启的数据。

### D9 查询固定默认 workspace

PG 启动时执行：

```sql
SELECT id FROM workspaces WHERE name = 'default';
```

缺失视为 schema 不变量失败，不能硬编码 nil UUID，因为已有 Kong 数据库的 default
workspace 可能不是全零 ID。事实优先使用 Route 的 `ws_id`，缺失时使用解析出的
default；API 始终增加 `workspace_id = default`，并拒绝显式
`workspace_id` 参数。

standalone DB-less 没有 workspace registry，统一归一为 nil UUID 逻辑 default，
且明确不支持 multi-workspace。

### D10 Manager 使用原生 SVG，不新增图表依赖

趋势图用响应式原生 SVG，实现 `role=img`、文字图例、可聚焦数据点、hover/focus
tooltip，并提供语义化表格替代。服务端 Decimal 字符串只可临时转 Number 计算
坐标，标签、KPI 和合计始终显示服务端原值。

## 3. 请求生命周期与事实采集

### 3.1 通用生命周期类型

`RequestCtx` 新增通用、非 AI 的状态：

```rust
pub struct RequestLifecycle {
    pub request_id: String,                 // UUID v4 simple，32 位小写 hex
    pub started_at: DateTime<Utc>,
    pub started_mono: Instant,
    pub finished_at: Option<DateTime<Utc>>,
    pub final_status: Option<u16>,
    pub upstream_status: Option<u16>,
    pub upstream_attempted: bool,
    pub upstream_response_started: bool,
    pub downstream_send_attempted: bool,
    pub downstream_response_completed: bool,
    pub termination_hint: Option<RequestTerminationHint>,
    pub transport_error: Option<RequestTransportError>,
}

pub enum RequestTerminationHint {
    PolicyRejected { phase: Phase, plugin: String },
    GatewayError { phase: LifecyclePhase, component: String },
    UpstreamSemanticError { provider_type: Option<String> },
}
```

`finished_at` 使用 `started_at + started_mono.elapsed()` 构造，避免系统时钟回拨让
finished 早于 started。现有 `KongCtx.request_id/request_start_time` 改为从这一个
source of truth 读取，避免双份时钟漂移。

事实写入前把 `started_at/finished_at` 都向下截断到 UTC 毫秒，
`finished_at=max(finished_at, started_at)`；PG 与 Memory store 均只保存这份规范化
值，排序、过滤与 API 输出不再保留更高精度。`e2e_ms` 仍来自单调时钟，不由两个
wall-clock 字段相减。

`RequestCtx` 同时保存 `route_name/service_name/workspace_id` 快照。Route 匹配和
Service 查找完成后、任何 body/plugin 操作前填充；早期失败也能保留网关维度。

`RequestTerminationHint` 只保存结构化阶段/类别和组件名，不保存原始请求、
provider body 或可能带凭据的完整错误文本。生命周期字段不允许散落直接写，统一
通过 `mark_policy_rejected`、`mark_gateway_error`、`mark_upstream_semantic_error`、
`mark_upstream_started/status`、`mark_downstream_send_attempted/completed` 和
`mark_transport_error` 更新；每个方法幂等，并按 analysis 的 outcome 优先级保留
更高优先级证据。

`PluginExecutor` 在 handler 正常返回 short circuit 时，若插件尚未显式标记，
默认记 `PolicyRejected`；插件返回 `Err` 默认记 `GatewayError`。`ai-proxy` 的
请求解析、配置、选模、request/response 转换失败必须在返回前显式
`mark_gateway_error`；Service 缺失/禁用、body preload 和代理内部失败由
`kong-proxy` 标记 GatewayError。provider 在 2xx stream 内返回协议 error event
标记 `UpstreamSemanticError`。这样分类不依赖插件名字符串，也不会把
`ai-proxy` 自身错误误作策略拒绝。

### 3.2 观察器接口与时序

```rust
pub trait RequestLifecycleObserver: Send + Sync {
    fn on_plugins_resolved(
        &self,
        plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    );

    fn on_request_finalizing(
        &self,
        plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    );
}
```

调用顺序固定为：

1. `new_ctx` 生成 request ID、UTC 起点与单调起点；
2. Route/Service 快照写入 `RequestCtx`；
3. 解析插件链后立刻把 `resolved_plugins` 保存进 `KongCtx`；
4. 在 body preload、rewrite、access、service enabled 检查前调用
   `on_plugins_resolved`；
5. 成功决定发往上游、返回 Pingora 继续代理前设置
   `upstream_attempted=true/attempt_count=1`，因此 DNS/connect 失败也属于已尝试；
6. upstream header/body filter 更新 upstream status/started、协议 stream terminal
   与 usage；filter 发生在 Pingora 实际 write 前，只能标记
   `downstream_send_attempted`，不能声称已成功发送；
7. `logging(error)` 是发送成功或 fatal error 后的唯一收口：`error=None` 才设置
   `downstream_response_completed=true`，Downstream error 保持 false；header-only
   响应同样在此判定。随后填最终 status、transport 分类、finished/e2e，再调用
   `on_request_finalizing`；
8. 最后执行普通插件 log，`ai-proxy.log` 复用已生成事实。

无 Route 的 404 尚未解析出包含 `ai-proxy` 的有效链，不创建事实；普通非 AI Route
同样不创建。

### 3.3 `AiUsageContext`

`kong-ai::usage::collector` 在 begin 时确认链中存在启用的 `ai-proxy`，插入：

```rust
pub struct AiUsageContext {
    pub fact_id: Uuid,                     // UUID v7
    pub ai_proxy_config: AiProxyUsageConfigSnapshot,
    pub requested_model: Option<String>,
    pub model_group: Option<String>,
    pub model_id: Option<Uuid>,
    pub model_name: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_name: Option<String>,
    pub provider_type: Option<String>,
    pub stream: Option<bool>,
    pub valid_stream_event_seen: bool,
    pub stream_terminal: StreamTerminalState,
    pub first_stream_event_at: Option<Instant>,
    pub gateway_cache_status: GatewayCacheStatus,
    pub usage: UsageAccumulator,
    pub pricing_features: PricingFeatures,
    pub input_override: Option<Decimal>,
    pub output_override: Option<Decimal>,
    pub finalized: bool,
}

pub enum StreamTerminalState {
    NotStreaming,
    Pending,
    Complete,
    ProviderFailed,
}
```

增量填充规则：

- body preload 成功后只解析 model/stream 等元数据，不保存 body；
- 该元数据提取由 `ai-proxy.rewrite` 完成；它只能对已预载 body 做 infallible、
  best-effort 浅提取，坏 JSON/坏 config 只留下空字段，绝不短路或返回 Err。正式
  解析错误仍在 `ai-proxy.access` 发生，因此缺 key + 坏 JSON 仍先得到认证 401，
  不改变现有 access priority；所有 rewrite 均先于 access，所以正常请求在
  `ai-key-auth` / Prompt Guard / rate-limit 拒绝前已有最小 AI 草稿；
- `ai-key-auth` 认证出 key 后先写 `AiAuthContext`，再做 allowed-model 校验，使
  model-not-allowed 403 仍能关联 key；`AiAuthContext` 增加安全 `key_prefix`；
- `ai-cache` 写五态 `GatewayCacheStatus`。当前功能未实现：链中无插件为
  `not_configured`，链中有插件为 `unavailable`，不得伪装成 miss；
- `ai-proxy` 选模后写实际 provider/model、模型组、稳定实体 ID 与价格覆盖；
- inline / `model_routes` 临时模型的 `model_id/provider_id` 写 `null`，停止把 nil
  或请求级随机 UUID 当稳定维度；
- body/filter codec 写 usage observation、首个有效 SSE 事件和协议终态。

stream 状态以**上游原始协议事件**为准，转换后的客户端协议事件只用于验证输出，
避免一次 translation 同时记两次。各 surface 的确定性转换为：

| surface | `valid_stream_event_seen` | `Complete` | `ProviderFailed` |
|---|---|---|---|
| OpenAI Chat / compatible | 首个可解析 data chunk（不含注释/heartbeat） | `[DONE]` | OpenAI error event |
| OpenAI Responses | 首个可解析 `response.*` data event | `response.completed` 或 `response.incomplete` | `response.failed` / error event |
| Anthropic Messages | `message_start/content_block_*/message_delta` | `message_stop` | `error` event |
| Gemini | 首个可解析 candidate/usage event | `[DONE]`，或带非空 `finishReason` 的最终 candidate 后正常 EOF | provider error event |

filter 只记录 non-stream/stream/header-only 的 EOS 尝试，不据此判断下游完成；
`logging(error=None)` 才确认整份响应成功发送。流式还必须有协议终态才完整。2xx 流
在 clean upstream EOF 时仍为 `Pending`（包括空流）视为中断；非 2xx JSON/error
body 即使配置了 stream，没有有效 stream event 也按 upstream error，不因缺
`[DONE]` 误判中断。协议终态之后若下游发送失败，仍由更高优先级 client disconnect
覆盖。

### 3.4 结果分类

finalize 是全函数，按需求分析优先级逐行命中唯一 `outcome`：

| 顺序 | 结构化条件 | outcome |
|---:|---|---|
| 1 | downstream transport error，且 downstream 尚未 completed | `client_disconnected` |
| 2 | effective stream 且有效 stream event 已开始，随后 transport/error/EOF 时 terminal 非 Complete；或 2xx 空流 clean EOF | `stream_interrupted` |
| 3 | hint=`PolicyRejected` 且未调用 provider | `gateway_rejected` |
| 4 | hint=`GatewayError`，无论 provider 是否已调用（含 upstream 2xx 后转换/内部失败） | `gateway_error` |
| 5 | 已调用 provider，且 upstream 非 2xx、upstream transport error 或 hint=`UpstreamSemanticError` | `upstream_error` |
| 6 | 最终 downstream 2xx、`logging(error=None)` 已确认 completed，且非流或 stream terminal=Complete | `success` |
| 7 | 任何未被上述覆盖的不一致状态 | `gateway_error`，同时增加 internal invariant counter |

Pingora `ErrorSource` 和 `ErrorType` 做结构化映射，不靠 error message substring：

- Downstream read/write/closed → client disconnect；
- Upstream connect/read/write/TLS/timeout → upstream error；
- 有效 stream event 已开始但未正常终止，或 2xx 空流结束 → stream interrupted；
- `ai-proxy` 在上游前主动返回的解析/配置/选模错误，以及上游 2xx 后的 response
  转换/内部错误，显式标记 GatewayError，而不是按通用短路或 upstream 误归类；
- Internal/Unset 结合 `termination_hint` 和 `upstream_attempted` 判定 gateway 或
  upstream。

无法向客户端写出状态时 `status_code=null`；另保存可空 `upstream_status_code`
便于解释被网关转换后的状态。

### 3.5 finalize 幂等与热路径

finalize 只做：

1. 合并 `RequestLifecycle`、`AiUsageContext`、`AiAuthContext` 与 route/service
   快照；
2. 归一化 usage、解析静态/覆盖价、Decimal 计算；
3. 构造不可变 `Arc<AiUsageFact>` 并放回 extensions；
4. `try_send` 给 writer；
5. 最后设置 `finalized=true`。

不持锁等待、不发网络请求、不执行 SQL。为避免 anymap 可变借用冲突，先结束对
`AiUsageContext` 的借用，再插入 `Arc<AiUsageFact>`。

## 4. Usage 归一化、TTFT 与计价

### 4.1 observation 与字段来源

```rust
pub enum ObservationKind {
    Snapshot,       // 最新快照覆盖同字段
    PartialUpdate,  // 只替换 observation 中的 Some
}

pub enum TokenFieldSource {
    Provider,
    Estimated,
    Mixed,
}

pub struct TokenField {
    pub value: i64,
    pub source: TokenFieldSource,
    pub derived: bool,
}

pub struct UsageObservation {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub kind: ObservationKind,
}
```

Accumulator 规则：

- `None` 从不覆盖已有值，官方值从不被估值覆盖；
- `Snapshot` 是 provider 在该时刻的累计快照，替换对应官方字段；
- `PartialUpdate` 仅更新出现的字段，不做隐式加法；
- 官方 total 存在时原样保存；仅当 prompt/completion 都已知且 total 缺失时用
  checked add 派生。派生字段另存 `derived=true`，其 source 由两个底层字段合成为
  Provider/Estimated/Mixed，不用一个丢失 lineage 的 `Derived` 值；
- prompt/completion/total 分别保存 source，外层 `usage_source`：
  - 无主 token → `unavailable`；
  - 只有 provider/由 provider 派生 → `provider`；
  - 只有估值/由估值派生 → `estimated`；
  - provider 与估值并存 → `mixed`。

codec 可以先把 provider JSON 数字读为 `u64`，但进入 observation 前必须受检转换为
非负 `i64`；TokenizerRegistry 结果、Anthropic/Gemini 分项求和与派生 total 也使用
checked conversion/add。任一 token 为负、超过 `i64::MAX` 或求和溢出时，不让异常
值进入 batch：该请求全部 usage 字段置 null，`usage_source=unavailable`，
`usage_unavailable_reasons=["invalid_token_value"]`，若已调用 provider 则
`cost_status=unavailable` 并加入 `invalid_provider_usage`。事实本身仍入队/落库，
不会因为一条恶意 provider 响应回滚整批。

`usage_unavailable_reasons` 固定枚举至少包含 `not_attempted`、
`provider_usage_missing`、`incomplete_response`、`estimation_unavailable` 与
`invalid_token_value`；去重并稳定排序。未调用 provider 使用 `not_attempted`，
不能用空 reasons 表示 unavailable。

### 4.2 provider 映射

| Provider / surface | 非流式 | 流式合并 | 标准化与 breakdown |
|---|---|---|---|
| OpenAI Chat / compatible | 最终 `usage` snapshot | 末尾带 usage chunk 作为 snapshot | prompt/completion/官方 total；cached 为 prompt 子集，reasoning 为 completion 子集 |
| OpenAI Responses | `response.usage` | `response.completed` 或 `response.incomplete` 的 `response.usage` | input→prompt、output→completion；提取 cached/reasoning；官方 total 优先 |
| Anthropic Messages | 最终 usage | `message_start` 更新 input/cache，最后一个 `message_delta.usage` 覆盖累计 output，`message_stop` 终止 | prompt=`input + cache_creation + cache_read`；completion=output；不得累加多个 delta |
| Gemini | 最终 `usageMetadata` | 每个 metadata 是累计 snapshot，取最新 | prompt=`promptTokenCount`；completion=`candidatesTokenCount + thoughtsTokenCount`；保留 thoughts/cache 与官方 total |

Gemini 缺 `candidatesTokenCount` 时 completion 仍为 null，不能因可选 thoughts 缺失
或存在而伪造主字段；可选 breakdown 在内部求和时可按 0，但事实保留 null 表示
“未报告”。

OpenAI-compatible 沿用 OpenAI Chat 字段映射，但缺失字段保持 null，也不套用
OpenAI 内置价。

### 4.3 估值与 TTFT

- prompt 官方值缺失且 `upstream_attempted=true` 时，使用请求侧
  `TokenizerRegistry` 结果；registry 返回值扩展为 token 数 + tokenizer/字符
  fallback provenance；
- completion 只有在完整、安全观察到最终响应文本且官方值缺失时才估算；
- 上游前拒绝不把请求体 token 当 provider usage，三项保持 null；
- stream 中断只保留已收到的官方 partial；响应不完整时不基于残片估 completion；
- TTFT 起点使用完整请求的 `started_mono`，终点是首个完整、非 heartbeat、
  非 `[DONE]` 的可解析 provider SSE 事件；
- 非流式、首事件未到或失败时 TTFT 为 null。

### 4.4 价表格式与校验

价表外形：

```json
{
  "schema_version": 1,
  "catalog_version": "2026-07-26.1",
  "snapshot_date": "2026-07-26",
  "entries": [{
    "provider_type": "openai",
    "model_ids": ["gpt-5.6-sol"],
    "aliases": ["gpt-5.6"],
    "prefixes": [],
    "input_usd_per_million": "5.000000000000",
    "output_usd_per_million": "30.000000000000",
    "effective_from": "2026-07-26T00:00:00Z",
    "effective_to": null,
    "conditions": { "max_prompt_tokens": 272000 },
    "source_url": "https://developers.openai.com/api/docs/models"
  }]
}
```

加载校验至少覆盖：

- catalog/schema version、日期、有效期顺序和 Decimal；每项价格 scale <= 12 且可
  放入非负、非 NaN 的 `NUMERIC(28,12)`；
- provider + ID/alias/prefix 在重叠有效期内不冲突；
- 所有价格非负，显式 0 合法；
- alias 不含 wildcard；prefix 必须显式、非空且最长优先；
- 禁止 `latest` 等动态 alias；
- analysis FR-4 的全部最低价目、Sonnet 切换边界和 GPT-5.6 条件存在。

价格更新流程：只修改该 JSON，递增 `catalog_version/snapshot_date`，补表驱动测试；
不修改旧事实、不运行历史回填。

### 4.5 计价状态机

`PricingFeatures` 仅保存无敏感内容的布尔/枚举，检测：

- provider cache read/write token 非零；
- GPT-5.6 内置价且已知 prompt > 272,000；
- 非标准 service tier / Batch/Flex/Priority；
- provider 计费的内置工具或非文本模态；
- 其他 codec 已明确识别但首版公式不支持的附加计费。

规则：

| 场景 | pricing status | cost status | cost |
|---|---|---|---|
| 未调用 provider / 未来 gateway cache hit | `not_applicable` | `not_incurred` | `"0.000000000000"` |
| 两方向价完整、usage 全 provider | `matched` | `calculated` | Decimal |
| 两方向价完整、至少一项估算 | `matched` | `estimated` | Decimal |
| 任一方向未匹配 | `unmatched` | `unavailable` | null |
| 已识别不支持的计价特征 | `unsupported` | `unavailable` | null |
| 价格已匹配但主 usage 缺失 | `matched` | `unavailable` | null |

GPT 长上下文条件只在使用内置价时检查；Model 同时覆盖 input/output 时视为用户声明
的扁平价并绕过该阈值。provider cache、内置工具等附加收费即使有两方向覆盖仍是
unsupported。

`cost_unavailable_reasons` 去重、稳定排序，至少包含分析文档定义的五类，并增加
`invalid_provider_usage` 与 `arithmetic_overflow`；
`pricing_unsupported_reasons` 另存具体不支持特征。成本公式：

```text
round_12(
  (prompt_tokens × input_price + completion_tokens × output_price) / 1_000_000
)
```

reasoning/thinking 已在 completion 中，不再次计费；cache token 虽已进入标准化
prompt，但因首版没有 cache 专用价，非零时整次成本不可计算。

每一步乘、加、除和 12 位量化都用 checked Decimal；任一步溢出、结果为负/NaN 或
最终值不能放入 `NUMERIC(28,12)` 时，保留已匹配价格，令
`cost_status=unavailable/cost_usd=null` 并加入 `arithmetic_overflow`，finalize
不得 panic 或返回错误。

summary 的金额和 token 小计可能超过单事实类型：PG 分别读取
`SUM(cost_usd)::text` 与 `SUM(token::numeric)::text`，DB-less 使用同一
`BigDecimal` / `BigInt` 聚合，不用 `rust_decimal` 或 i64 累加。API 的
`cost_usd_calculable_sum` 固定 12 位字符串，aggregate token `known_sum` 为十进制
整数字符串；Manager 仅为 SVG 坐标做有损 Number 投影，标签/KPI 保留原字符串。

## 5. 持久化、Store 与 writer

### 5.1 migration

新增 `crates/kong-db/migrations/core/006_ai_usage_logs.sql`，注册到
`CORE_MIGRATIONS`。迁移先检查现有 `ai_models.input_cost/output_cost` 均有限、
非负、落在 `NUMERIC(28,12)` 范围内，且文本十进制表示可在 12 位小数内无损量化；
发现脏值即以列名和 model ID 终止升级，不把非法价格静默改成 0/null，也不静默
舍入。合法值通过其 PostgreSQL 文本表示转为 `NUMERIC(28,12)`，并为两列增加非负
且非 NaN CHECK。随后创建事实表；`KNOWN_TABLES` 将
`ai_usage_logs` 放在 AI 配置实体和 workspaces 之前。事实表不对 Route、Service、
Provider、Model、Virtual Key、Consumer 或 Workspace 建 FK。

`ai_usage_logs` 列如下：

| 类别 | 列 |
|---|---|
| 标识 | `id UUID PK`、`ingest_seq BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE`、`request_id VARCHAR(32) UNIQUE`、`node_id UUID` |
| 时间 | `started_at TIMESTAMPTZ(3)`、`finished_at TIMESTAMPTZ(3)`、`recorded_at TIMESTAMPTZ(3) DEFAULT clock_timestamp()` |
| workspace/网关 | nullable `workspace_id`；route/service nullable ID + name snapshot |
| AI | provider/model nullable稳定 ID + name/type；`requested_model`、`model_group`、`actual_model`；`attempt_count` |
| 身份 | nullable `virtual_key_id/name/prefix`、`consumer_id` |
| usage | nullable prompt/completion/total/reasoning/cache-read/cache-write BIGINT；三项主字段 source；`usage_source` 与 unavailable reasons |
| 价格 | input/output 各自 `NUMERIC(28,12)` rate、source、version、snapshot date、effective from/to；`pricing_status`、unsupported reasons |
| 成本 | `currency='USD'`、nullable `cost_usd NUMERIC(28,12)`、`cost_status`、unavailable reasons |
| 结果 | nullable final/upstream status；`outcome`、`e2e_ms`、nullable `ttft_ms`、`upstream_attempted`、nullable `stream`、`cache_status` |

实现使用以下精确列名：

```sql
CREATE TABLE ai_usage_logs (
  id                                UUID PRIMARY KEY,
  ingest_seq                        BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
  request_id                        VARCHAR(32) NOT NULL UNIQUE,
  node_id                           UUID NOT NULL,
  started_at                        TIMESTAMPTZ(3) NOT NULL,
  finished_at                       TIMESTAMPTZ(3) NOT NULL,
  recorded_at                       TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
  workspace_id                      UUID,
  route_id                          UUID,
  route_name                        TEXT,
  service_id                        UUID,
  service_name                      TEXT,
  provider_id                       UUID,
  provider_name                     TEXT,
  provider_type                     TEXT,
  model_id                          UUID,
  requested_model                   TEXT,
  model_group                       TEXT,
  actual_model                      TEXT,
  attempt_count                     SMALLINT NOT NULL,
  virtual_key_id                    UUID,
  virtual_key_name                  TEXT,
  virtual_key_prefix                TEXT,
  consumer_id                       UUID,
  prompt_tokens                     BIGINT,
  completion_tokens                 BIGINT,
  total_tokens                      BIGINT,
  reasoning_tokens                  BIGINT,
  cache_read_input_tokens           BIGINT,
  cache_write_input_tokens          BIGINT,
  prompt_tokens_source              TEXT,
  completion_tokens_source          TEXT,
  total_tokens_source               TEXT,
  usage_source                      TEXT NOT NULL,
  usage_unavailable_reasons         TEXT[] NOT NULL DEFAULT '{}',
  input_price_per_million           NUMERIC(28,12),
  input_price_source                TEXT,
  input_price_version               TEXT,
  input_price_snapshot_date         DATE,
  input_price_effective_from        TIMESTAMPTZ,
  input_price_effective_to          TIMESTAMPTZ,
  output_price_per_million          NUMERIC(28,12),
  output_price_source               TEXT,
  output_price_version              TEXT,
  output_price_snapshot_date        DATE,
  output_price_effective_from       TIMESTAMPTZ,
  output_price_effective_to         TIMESTAMPTZ,
  pricing_status                    TEXT NOT NULL,
  pricing_unsupported_reasons       TEXT[] NOT NULL DEFAULT '{}',
  currency                          CHAR(3) NOT NULL DEFAULT 'USD',
  cost_usd                          NUMERIC(28,12),
  cost_status                       TEXT NOT NULL,
  cost_unavailable_reasons          TEXT[] NOT NULL DEFAULT '{}',
  status_code                       SMALLINT,
  upstream_status_code              SMALLINT,
  outcome                           TEXT NOT NULL,
  e2e_ms                            BIGINT NOT NULL,
  ttft_ms                           BIGINT,
  upstream_attempted                BOOLEAN NOT NULL,
  stream                            BOOLEAN,
  cache_status                      TEXT NOT NULL
);
```

约束（实现为命名 CHECK，并由 migration/schema 测试逐项断言）：

- request ID 匹配 `^[0-9a-f]{32}$`；tokens/latency 非负；每个非空 numeric
  price/cost 同时满足 `>= 0` 与 `<> 'NaN'::numeric`；
- `finished_at >= started_at`，status 在 100~599；
- attempt 首版为 0/1，且与 `upstream_attempted` 一致；
- 所有状态枚举使用 CHECK；
- prompt/completion/total 各自满足 token 与 source 同空/同非空；source 只允许
  provider/estimated/mixed；usage_source=unavailable 时三个主字段全空且
  unavailable reasons 非空；其他 usage_source 至少一项主字段非空且 reasons 为空。
  usage_source=provider 要求所有非空字段 source=provider；estimated 要求全部为
  estimated；mixed 要求存在 mixed source，或同时存在 provider 与 estimated；
- 每个方向的 price rate/source/version/snapshot/effective_from 是同空同非空的
  原子 bundle，`effective_to` 可空，否则必须大于 effective_from；
- matched 必须有双向价格；unmatched 必须至少一个方向 bundle 为空；
  not_applicable 双向 bundle 均空；
  unsupported reasons 仅在 unsupported 时非空，其他 pricing status 必为空；
- pricing=not_applicable 当且仅当 cost_status=not_incurred；not_incurred 要求
  cost=0 且 upstream_attempted=false，反过来 upstream_attempted=false 也必须是
  这组 not_applicable/not_incurred 状态；
- calculated 要求 pricing=matched、usage_source=provider、prompt/completion 均
  已知且 cost 非空；estimated 要求 pricing=matched、
  usage_source IN (estimated,mixed)、prompt/completion 均已知且 cost 非空；
  unmatched/unsupported 必须 cost=unavailable；
- unavailable 必须 cost null 且 reasons 非空，其他 cost status 的 reasons 必为空；
- `currency` 必须等于 USD；全部 reasons 数组只含固定枚举且
  `array_position(reasons, NULL) IS NULL`。

schema 保持 analysis 要求的 nullable `workspace_id`，但正常 writer 的运行时不变量
是始终写入已解析的 default/Route workspace；PG 查询只读 default，DB-less 写 nil
逻辑 default。nullable 只用于兼容稀疏历史/未来迁移，不会被当前 API 混入。

首版不在该表预留伪 attempt 列。REQ-AI-005 引入重试时新增以 fact ID 为关联键的
attempt 子事实；请求事实只保存跨 attempt 可安全汇总的字段，不能把多 provider
成本继续塞进当前单一 provider/model 维度。

`ai_models.input_cost/output_cost` 转为 `NUMERIC(28,12)` 后立即验证
`value >= 0 AND value <> 'NaN'::numeric` CHECK；受限 NUMERIC 已排除 Infinity，
迁移前置检查已保证旧行满足约束，Admin 字段级校验与数据库约束构成两层防线。

### 5.2 索引

除两个 UNIQUE 索引外新增：

```text
(workspace_id, started_at DESC, id DESC)
(workspace_id, actual_model, started_at DESC, id DESC) WHERE actual_model IS NOT NULL
(workspace_id, model_group, started_at DESC, id DESC) WHERE model_group IS NOT NULL
(workspace_id, virtual_key_id, started_at DESC, id DESC) WHERE virtual_key_id IS NOT NULL
(workspace_id, route_id, started_at DESC, id DESC) WHERE route_id IS NOT NULL
(workspace_id, service_id, started_at DESC, id DESC) WHERE service_id IS NOT NULL
(workspace_id, provider_id, started_at DESC, id DESC) WHERE provider_id IS NOT NULL
(workspace_id, consumer_id, started_at DESC, id DESC) WHERE consumer_id IS NOT NULL
```

低基数状态/布尔值先使用基础时间索引；requested model/provider type 等索引以真实
`EXPLAIN (ANALYZE, BUFFERS)` 决定，不为所有过滤器盲目放大写成本。首版不分区、
不自动 retention。

### 5.3 `kong-ai::usage` 目录

```text
usage/
├── mod.rs
├── model.rs        # facts、filters、summary、error、mode
├── collector.rs    # lifecycle observer、outcome/finalize
├── normalizer.rs   # observation/accumulator/provider 映射
├── pricing.rs      # catalog、rate resolution、cost
├── store.rs        # AiUsageStore trait
├── postgres.rs     # batch insert/list/summary
├── memory.rs       # DB-less ring/list/summary
├── cursor.rs       # snapshot/offset 编解码与 filter hash
└── writer.rs       # sender、runner、stats
```

```rust
#[async_trait]
pub trait AiUsageStore: Send + Sync {
    fn mode(&self) -> AiUsageMode;
    async fn insert_batch(&self, rows: &[AiUsageFact]) -> Result<BatchWriteResult>;
    async fn snapshot(&self, filter: &AiUsageFilter) -> Result<AiUsageSnapshot>;
    async fn list(&self, query: &AiUsageListQuery) -> Result<AiUsagePage>;
    async fn summary(&self, query: &AiUsageSummaryQuery) -> Result<AiUsageSummary>;
}
```

`AdminState` 不直接放一个会伪造 workspace 的 Unsupported store，而是保存
`AiUsageRuntime::{Supported { store, default_workspace_id, stats },
UnsupportedHybrid}`。两条 handler 先只提取 State/RawQuery 并 match runtime：
Hybrid 在解析 query、workspace、snapshot 之前固定返回 501；Supported 分支才调用
严格 extractor。CP/DP 启动不解析 default workspace，也不创建 writer/collector。

PG 实现只使用 `Database::pool()`，不使用 read replica，保证默认 2 秒可见性及同一
snapshot 的读一致性。批写用 `QueryBuilder<Postgres>` + bind +
`ON CONFLICT (request_id) DO NOTHING`，一次 statement；advisory lock 和 INSERT
在同一事务。lock key 由 `current_database + current_schema + ai_usage_logs` 的
稳定 hash 构造，避免同 cluster 不同 database 无谓串行。
队列容量与 DB-less ring 容量上限均为 1,000,000；单批上限为 1,129 且不能超过
队列容量。事实表每行 58 个 bind，1,129 行保持在 PostgreSQL 单 statement
65,535 个参数的协议上限内；配置装载及 writer/store 构造层都拒绝越界值，避免
运行时 panic、无界 clone 或永久失败批次。

### 5.4 writer 生命周期

`AiUsageWriter` 持有 sender、`accepting: AtomicBool` 和 stats；runner 持有
receiver/store。`kong-server` 用 `AiUsageWriterBgService: BackgroundService`
托管 runner，而不是 detached spawn：

- 首条记录后收集至 256 条或 500ms；
- 写成功增加实际插入 `written`，冲突增加 `duplicate`；
- 队列满、关闭、重试耗尽、关闭超时分别计 drop reason；
- collector 的 `active_contexts` 仅作 gauge，不作为提前退出条件，因为已被 proxy
  接受但尚未解析插件链的请求还没有 begin；
- shutdown 后停止接收新连接，writer 在完整 5s drain window 内继续接受 finalize，
  按原 256/500ms 规则持续 flush，即使某一时刻 active=0/queue=0 也不提前关闭；
- 5s 到期令 `accepting=false` 并结束 runner；队列残留计 `shutdown_timeout`，
  此后在途 finalize 计 `writer_closed`。proxy 默认 10s graceful timeout 比该窗口
  更长，因此 5~10s 才结束的请求允许丢失，必须计数且文档明确，不宣称全量 drain。

每次 PG 写尝试的 2 秒 timeout 覆盖 acquire、advisory lock、execute 与 commit
整个事务。只重试连接、serialization/deadlock、timeout 等白名单 transient
SQLSTATE；CHECK/编码等永久错误立即失败。commit 超时可能结果未知，依靠 request ID
幂等重试，并单列 `write_outcome_unknown`，不能直接算 confirmed drop。shutdown
deadline 是 runner 的外层硬上限，不会因连接池或网络长期挂起而无限延长。

### 5.5 writer 可观测性

`AiUsageWriterStats` 使用 Atomics：

- totals：enqueued、written、duplicate、`dropped`、write_failures、retries、
  write_outcome_unknown、DB-less evicted；`dropped` 是下列 reason counters 之和，
  `/status` 与 Prometheus 同时暴露总数和分项；
- dropped：queue_full、writer_closed、write_retries_exhausted、
  shutdown_timeout；
- gauges：queue_depth、queue_capacity。

`/status` 增 `ai_usage_writer` JSON；启用 Prometheus status endpoint 时，在现有
Lua exporter 文本后追加 `kong_ai_usage_writer_*` counter/gauge。该处只交付 writer
运行健康，不提前实现 REQ-AI-004 的 `ai_llm_*` 业务指标。

告警只含 backend、batch size、attempt、错误类别和累计丢失量，不打印事实正文；
连续失败按首次/恢复和限频策略记录，避免故障时日志风暴。

### 5.6 DB-less ring

`MemoryAiUsageStore` 持有进程启动时随机生成的 `ring_instance_id`，并使用单个
`tokio::RwLock<RingState { rows, next_seq, generation, evicted }>`：

- append 在同一写锁临界区分配 sequence、push，并在超容量时删除最旧记录、增加
  generation/evicted；
- 查询在同一读锁临界区取得 watermark/generation/ring_instance_id 并 clone 候选
  `Arc`，释放锁后筛选聚合；
- 新数据 seq 大于 watermark，不进入旧 snapshot；
- 任意淘汰使 generation 变化，旧 snapshot 统一 409。这是有意的保守语义；
- snapshot 中的 `ring_instance_id` 与当前实例不符也返回 409，防止进程重启后
  sequence/generation 从零复用而错误接受旧 token；
- 5 秒 timeout 通过 `spawn_blocking` + `tokio::time::timeout` 执行 CPU 聚合；
  因 timeout 不会取消 blocking task，入口另用小型 semaphore 限制并发，满时返回
  503 `analytics_query_unavailable`，避免恶意请求堆积后台计算；
- 重启为空，不写入声明式 `DblessStore`。

## 6. Admin API 与查询算法

### 6.1 路由与严格参数

新增 `crates/kong-admin/src/handlers/ai_usage.rs`，并同步：

- `build_admin_router` 注册 `/ai-usage`、`/ai-usage/summary`；
- `is_known_route`、`determine_allowed_methods` 和 `/endpoints` 登记；
- `AdminState` 增加前述 `AiUsageRuntime`；route 只提取 State/RawQuery，先对
  `UnsupportedHybrid` 返回 501，Supported 分支才解析严格 query，不创建伪
  Hybrid store/writer/collector。

不复用宽松 `ListParams`。新 extractor 先读取 raw query keys，拒绝未知参数和重复
scalar 参数，再解析带 `#[serde(deny_unknown_fields)]` 的 DTO。

公共参数：`start/end/snapshot` 和 analysis FR-7 的全部过滤器。规则：

- 无 snapshot 且无 start/end：解析为请求时刻最近 24h；
- 显式时间必须成对、`start < end`、最大 90d；
- start/end 解析后统一向下截断到 UTC 毫秒并写入 snapshot；规范化后不再
  `start < end` 仍返回 400。事实和 DB-less 使用同一精度，保证毫秒边界的
  `[start,end)` 与 PG 一致；
- snapshot 已内嵌规范化 start/end；后续省略时间时复用其边界，不能重新滚动 24h；
- UUID、request ID、status、enum 严格校验；
- `workspace_id` 和任何未知 key 返回 400；
- list：`size` 默认 100、范围 1~1000；offset 已内嵌 snapshot，可单独传入，若又
  显式传 snapshot 则两者必须逐字段一致；
- summary：`breakdown=hour|day|provider|actual_model|model_group|virtual_key|route|service`；
  `limit` 仅分类维度可用，默认 10、最大 100；`order_by` 仅允许
  `cost_usd|total_tokens|requests`，分类维度默认 `cost_usd`；`timezone` 仅
  hour/day 可用并通过 `chrono-tz` 校验，缺省为 `UTC`；
- hour 同时要求原始 `end-start <= 31d` 且 TimeBucketPlan 实际桶数 <=744；day
  同时要求 `end-start <= 90d` 且实际桶数 <=90。两个条件分别校验，不能因 DST
  跳时后桶数较少而放宽绝对时长；
- 未传 `breakdown` 时拒绝 `timezone/limit/order_by`；时间维度拒绝
  `limit/order_by`，分类维度拒绝 `timezone`，避免参数被静默忽略；
- 时间项始终按 `bucket_start ASC`；分类项按选定指标 DESC、`requests DESC`、
  canonical group key ASC 稳定排序。若选定指标就是 requests，则只应用一次
  requests DESC 后按 key 排序；合法 null group 用显式 `key IS NULL` 排在非 null
  group 之后；`other` 不参与 Top N 排序。

### 6.2 snapshot 与 offset

token 是 base64url-no-pad 编码的版本化 JSON，不构成权限边界：

```text
snapshot = {
  v, backend, workspace_id, start, end,
  high_watermark, eviction_generation?, ring_instance_id?,
  filter_hash
}
offset = {
  v, snapshot fields/hash,
  last_started_at, last_id
}
```

`filter_hash` 是规范化 start/end、默认 workspace 和所有公共筛选的 SHA-256；不含
page size、breakdown、timezone、limit 或 order_by，因此同一事实集合可在明细、
趋势和多个排行间复用。backend/filter/token 不一致返回
`analytics_invalid_query`。所有 SQL 值继续使用 bind，篡改 token 不会形成 SQL
注入或跨 workspace 查询。

token 作为不可信输入：解码前限制长度，解码后重新校验 version/backend、固定由
服务器覆盖的 default workspace、规范化时间与 90d 上限、非负 i64 high watermark、
且 high watermark 不大于当前已提交/当前 Store 水位、DB-less
generation/instance ID、filter hash，以及 offset 的
`last_started_at/last_id` 位于 snapshot 窗口且类型合法。当前没有稳定集群签名密钥，
首版不使用进程随机 HMAC（否则跨节点/重启失效）；安全性依赖上述全量校验、workspace
覆盖与参数 bind，而不是信任 token 内容。

PG list 条件：

```sql
WHERE workspace_id = $default_workspace
  AND ingest_seq <= $high_watermark
  AND started_at >= $start AND started_at < $end
  AND ... exact filters ...
  AND (started_at, id) < ($last_started_at, $last_id) -- 非首屏
ORDER BY started_at DESC, id DESC
LIMIT $size + 1
```

### 6.3 明细响应

```json
{
  "data": [{
    "id": "uuid",
    "request_id": "32-lowercase-hex",
    "started_at": "2026-07-26T08:00:00.123Z",
    "finished_at": "2026-07-26T08:00:01.234Z",
    "gateway": {
      "route": {"id": "uuid", "name": "chat"},
      "service": {"id": "uuid", "name": "openai"}
    },
    "ai": {
      "provider": {"id": "uuid", "name": "prod", "type": "openai"},
      "model": {
        "id": "uuid",
        "requested": "chat",
        "group": "chat",
        "actual": "gpt-5.6-sol"
      },
      "attempt_count": 1
    },
    "identity": {
      "virtual_key": {"id": "uuid", "name": "team-a", "prefix": "kr_abcd"},
      "consumer_id": null
    },
    "usage": {
      "prompt_tokens": 100,
      "completion_tokens": 20,
      "total_tokens": 120,
      "prompt_source": "provider",
      "completion_source": "provider",
      "total_source": "provider",
      "reasoning_tokens": null,
      "cache_read_input_tokens": null,
      "cache_write_input_tokens": null,
      "source": "provider",
      "unavailable_reasons": []
    },
    "pricing": {
      "status": "matched",
      "currency": "USD",
      "input": {
        "usd_per_million": "5.000000000000",
        "source": "builtin",
        "version": "2026-07-26.1",
        "snapshot_date": "2026-07-26",
        "effective_from": "2026-07-26T00:00:00Z",
        "effective_to": null
      },
      "output": {
        "usd_per_million": "30.000000000000",
        "source": "builtin",
        "version": "2026-07-26.1",
        "snapshot_date": "2026-07-26",
        "effective_from": "2026-07-26T00:00:00Z",
        "effective_to": null
      },
      "unsupported_reasons": []
    },
    "cost": {
      "usd": "0.001100000000",
      "status": "calculated",
      "unavailable_reasons": []
    },
    "result": {
      "status_code": 200,
      "upstream_status_code": 200,
      "outcome": "success",
      "e2e_ms": 1111,
      "ttft_ms": null,
      "upstream_attempted": true,
      "stream": false,
      "cache_status": "not_configured"
    }
  }],
  "offset": "opaque-or-null",
  "next": "/ai-usage?...",
  "snapshot": "opaque",
  "meta": {
    "mode": "postgres",
    "ephemeral": false,
    "node_id": null,
    "capacity": null,
    "earliest_available_at": null,
    "restart_clears": false
  }
}
```

实体已删除时仍返回事实中的 name/ID 快照；API 不 join 配置表，也不暴露任何
payload、headers、key 原文/hash 或 provider auth。

所有可缺失嵌套值采用稳定的 null 语义：route/service/provider/model/virtual_key
整体无快照时为 null，input/output price direction 不存在时为 null，不返回空
`{}`；数组与固定 status map 才使用空集合/零值键。

明细和 summary 共用完整 `AiUsageMeta` DTO，不能让首屏额外探测运行模式。PG 固定
`mode=postgres/ephemeral=false/restart_clears=false`，其 node/capacity/earliest
为 null；DB-less 固定 `mode=dbless/ephemeral=true/restart_clears=true`，返回当前
node ID、配置 capacity，空 ring 的 earliest 为 null、非空为最早事实时间。

### 6.4 summary 口径

`totals` 与每个 breakdown item 的 `metrics` 共用同一个 `AggregateMetrics` DTO：

- `requests/successful_requests/failed_requests/cache_hits` 为整数；
- `outcomes` 固定返回
  `success/gateway_rejected/gateway_error/upstream_error/client_disconnected/stream_interrupted`
  六个键，零值也不省略；
- `prompt_tokens/completion_tokens/total_tokens` 均为
  `{known_sum, known_requests, unknown_requests, coverage}`；known_sum 是任意精度
  十进制整数字符串，两个 request count 为整数，coverage 为六位小数字符串或 null；
- `cost_usd_calculable_sum` 为 12 位十进制字符串；
- `pricing_status` 固定返回
  `matched/unmatched/unsupported/not_applicable`，`cost_status` 固定返回
  `calculated/estimated/not_incurred/unavailable`，零值键不省略；
- `estimated_usage_ratio/pricing_coverage/cost_calculable_coverage` 为六位小数字符串
  或 null；`avg_e2e_ms/p95_e2e_ms/avg_ttft_ms` 为三位小数字符串或 null。

响应包含：

```json
{
  "snapshot": "opaque",
  "meta": {
    "mode": "postgres",
    "ephemeral": false,
    "node_id": null,
    "capacity": null,
    "earliest_available_at": null,
    "restart_clears": false
  },
  "totals": {
    "requests": 10,
    "successful_requests": 8,
    "failed_requests": 2,
    "outcomes": {
      "success": 8,
      "gateway_rejected": 1,
      "gateway_error": 0,
      "upstream_error": 1,
      "client_disconnected": 0,
      "stream_interrupted": 0
    },
    "prompt_tokens": {
      "known_sum": "1000",
      "known_requests": 8,
      "unknown_requests": 1,
      "coverage": "0.888889"
    },
    "completion_tokens": {
      "known_sum": "200",
      "known_requests": 8,
      "unknown_requests": 1,
      "coverage": "0.888889"
    },
    "total_tokens": {
      "known_sum": "1200",
      "known_requests": 8,
      "unknown_requests": 1,
      "coverage": "0.888889"
    },
    "cost_usd_calculable_sum": "0.123456789012",
    "pricing_status": {
      "matched": 8,
      "unmatched": 1,
      "unsupported": 0,
      "not_applicable": 1
    },
    "cost_status": {
      "calculated": 7,
      "estimated": 0,
      "not_incurred": 1,
      "unavailable": 2
    },
    "estimated_usage_ratio": "0.125000",
    "pricing_coverage": "0.888889",
    "cost_calculable_coverage": "0.777778",
    "avg_e2e_ms": "123.500",
    "p95_e2e_ms": "456.000",
    "avg_ttft_ms": null,
    "cache_hits": 0
  },
  "breakdown": {
    "type": "hour",
    "timezone": "Asia/Shanghai",
    "order_by": null,
    "limit": null,
    "items": [{
      "key": "2026-07-26T08:00:00Z",
      "label": "2026-07-26 16:00 +08:00",
      "is_other": false,
      "bucket_start": "2026-07-26T08:00:00Z",
      "bucket_end": "2026-07-26T09:00:00Z",
      "dimension": null,
      "metrics": {
        "requests": 2,
        "successful_requests": 2,
        "failed_requests": 0,
        "outcomes": {
          "success": 2,
          "gateway_rejected": 0,
          "gateway_error": 0,
          "upstream_error": 0,
          "client_disconnected": 0,
          "stream_interrupted": 0
        },
        "prompt_tokens": {
          "known_sum": "200",
          "known_requests": 2,
          "unknown_requests": 0,
          "coverage": "1.000000"
        },
        "completion_tokens": {
          "known_sum": "40",
          "known_requests": 2,
          "unknown_requests": 0,
          "coverage": "1.000000"
        },
        "total_tokens": {
          "known_sum": "240",
          "known_requests": 2,
          "unknown_requests": 0,
          "coverage": "1.000000"
        },
        "cost_usd_calculable_sum": "0.004000000000",
        "pricing_status": {
          "matched": 2,
          "unmatched": 0,
          "unsupported": 0,
          "not_applicable": 0
        },
        "cost_status": {
          "calculated": 2,
          "estimated": 0,
          "not_incurred": 0,
          "unavailable": 0
        },
        "estimated_usage_ratio": "0.000000",
        "pricing_coverage": "1.000000",
        "cost_calculable_coverage": "1.000000",
        "avg_e2e_ms": "120.000",
        "p95_e2e_ms": "135.000",
        "avg_ttft_ms": "25.000",
        "cache_hits": 0
      }
    }],
    "other": null
  }
}
```

token coverage 的 known/unknown 分母只含 `upstream_attempted=true`；上游前拒绝
不污染 usage coverage。cost subtotal 只求和 `calculated/estimated`；0 个分母的
coverage 返回 null。所有状态 count 明确返回，不能把可计算小计称为完整账单。
未请求 breakdown 时该字段为 null；请求多个排行时分别调用 API，但复用同一
snapshot。

在线 OpenAPI/schema 和实际 JSON 中，`metrics` 必须完整返回与 `totals` 相同的
`AggregateMetrics` 全部字段，不得输出稀疏对象。时间项 DTO 固定为：

```text
{
  key: UTC bucket_start string,
  label: timezone-aware label with numeric UTC offset,
  is_other: false,
  bucket_start: UTC timestamp,
  bucket_end: UTC timestamp,
  dimension: null,
  metrics: AggregateMetrics
}
```

分类项和 `other` 使用同一外形：

```text
category item = {
  key: canonical group key or null,
  label: display snapshot or null,
  is_other: false,
  bucket_start: null,
  bucket_end: null,
  dimension: {
    id: UUID or null,
    name: string or null,
    type: string or null,
    prefix: string or null
  },
  metrics: AggregateMetrics
}

other = {
  key: null,
  label: "Other",
  is_other: true,
  bucket_start: null,
  bucket_end: null,
  dimension: null,
  metrics: AggregateMetrics
}
```

分类 response 的 `breakdown.timezone=null`，并回显实际 `order_by/limit`；时间
response 的 `order_by/limit=null`。provider、virtual_key、route、service 有稳定
实体 ID 时 canonical key 为 `id:<uuid>`，缺 ID 时使用各自事实快照；actual_model
始终按 `provider_type + actual_model` 分组，model_group 始终按 group 字符串分组，
避免实体改名后把两个实际模型误合并。所有 fallback key 都用维度类型和规范化快照
字段的 SHA-256 生成 `snapshot:<lowercase-hex>`，不暴露分隔拼接细节。全空维度是
key=null、label=null 的合法“未关联”组，由 Manager 本地化显示，并与
`is_other=true` 明确区分。同一 ID 组的展示 name/type/prefix 取过滤集合中
`(started_at, id)` 最大事实的快照，因此实体改名后分组稳定且标签确定。

### 6.5 PostgreSQL summary、P95、Top/Other 与 DST

PG 查询在 primary pool 的 read-only repeatable-read transaction 内执行：

```sql
SET LOCAL statement_timeout = '5s';
WITH filtered AS MATERIALIZED (
  SELECT ...
  FROM ai_usage_logs
  WHERE workspace_id = $1
    AND ingest_seq <= $2
    AND started_at >= $3 AND started_at < $4
    ...
)
...
```

外层另加 5 秒 `tokio::time::timeout`。totals 用 `COUNT/SUM ... FILTER`；
`percentile_cont(0.95) WITHIN GROUP (ORDER BY e2e_ms)`；DB-less 按
`rank=(n-1)*0.95` 做相同线性插值，并用同 fixture 做 PG/内存 parity。

分类 breakdown：

1. 按白名单静态维度表达式聚合并稳定排序；
2. 选 Top N；
3. 回到 filtered 原始事实标记 top / `is_other` 后重新聚合。

不能把已聚合 group 的 AVG/P95 二次合并。null 维度是合法“未关联”组，
`is_other` 单独标识，不用 magic key；没有剩余事实时 `other=null`。

仓库仍支持 PostgreSQL 15+，因此不使用 PG16 才提供的 timezone-aware
`generate_series`。PG 与 DB-less 共用 `chrono-tz` 的 `TimeBucketPlan`：先按 IANA
规则扫描当地桶身份的变化，并精确到秒生成有序 UTC
`bucket_start/bucket_end` 及带 offset 的 label，再把 start 数组绑定到 PG。
PG15 的 `width_bucket(started_at, $starts::timestamptz[])` 将 filtered 事实映射到
ordinal 后聚合；Rust 按计划合并 aggregate 并补零桶。这样 PG 不自行解释重复/
不存在的当地时间，且 PG/内存共享完全相同的边界：

- hour 桶身份包含当地日期、小时和 UTC offset；秋季两个 01:00 用不同 UTC start
  表示，历史秒级 offset 也产生精确边界；
- day 桶身份是当地 civil date：重复午夜不会拆成两个同日期桶，不存在午夜时以
  当日首个有效 instant 开始；允许 23/25 小时及历史日期线回拨形成的更长桶；
- item 返回 UTC `bucket_start/bucket_end`，Manager 按请求 timezone 展示并带
  offset；
- 空桶 additive metrics 为 0，coverage/AVG/P95 为 null；
- hour 在时长检查后实际生成桶数不得超过 744，day 不得超过 90。

SQL 与 migration 在 PostgreSQL 15、16 各跑一遍兼容测试；默认受管环境继续使用
仓库当前版本，但不得把产品最低版本暗升到 16。

### 6.6 错误体

保留现有 Kong 数字 `code`，新增稳定字符串 `error_code`：

```json
{
  "message": "Analytics snapshot has expired",
  "name": "analytics error",
  "code": 5,
  "error_code": "analytics_snapshot_expired",
  "fields": {}
}
```

| HTTP | `error_code` | 场景 |
|---:|---|---|
| 400 | `analytics_invalid_query` | 参数、时间、cursor、filter/backend mismatch |
| 409 | `analytics_snapshot_expired` | DB-less ring 淘汰或 Store 实例已重启 |
| 501 | `analytics_unsupported_in_hybrid` | CP/DP Hybrid |
| 503 | `analytics_query_timeout` | 5 秒超时 |
| 503 | `analytics_query_unavailable` | Store 暂时不可用 |
| 500 | `analytics_internal_error` | 未预期错误；不回传 SQL/内部细节 |

### 6.7 Model 有效价格

`ai_models::list/get_one` 在现有响应上增只读 `effective_pricing`；先一次加载 Provider
映射，再调用共享 `PriceCatalog`，前端不复制匹配算法：

```json
{
  "effective_pricing": {
    "currency": "USD",
    "unit": "1M tokens",
    "status": "matched",
    "catalog_version": "2026-07-26.1",
    "catalog_snapshot_date": "2026-07-26",
    "input": {
      "amount": "5.000000000000",
      "source": "builtin",
      "version": "2026-07-26.1",
      "snapshot_date": "2026-07-26"
    },
    "output": {
      "amount": "30.000000000000",
      "source": "builtin",
      "version": "2026-07-26.1",
      "snapshot_date": "2026-07-26"
    },
    "conditions": [{"type": "max_prompt_tokens", "value": 272000}]
  }
}
```

这是 additive response。create/update/upsert 前专门规范化并校验两项成本：
`null`、规范十进制字符串与兼容 JSON number 可用，0 合法；负数、NaN、Infinity、
超范围或不能无损转为 12 位 Decimal 的值返回字段级 400。响应保留原有 number
类型的 `input_cost/output_cost`，并新增精确字符串
`input_cost_decimal/output_cost_decimal`；`effective_pricing.*.amount` 也是
12 位十进制字符串。缺失方向统一返回 null，不用 `{}` 表示缺失。

## 7. Kong Manager

### 7.1 路由与组件

保持 `/ai-gateway` → Endpoints 不变，新增嵌套路由：

```text
/ai-gateway/usage       ai-usage-overview
/ai-gateway/usage/logs  ai-usage-logs
```

`AiGatewayNav.vue` 新增“调用统计”。推荐文件：

```text
pages/ai-gateway/usage/
├── Usage.vue
├── UsageOverview.vue
├── UsageLogs.vue
├── useAiUsageController.ts
├── aiUsageTypes.ts
├── aiUsageFormatters.ts
├── services/aiUsageService.ts
└── components/
    ├── UsageFilterBar.vue
    ├── UsageTrendChart.vue
    ├── UsageRankingTable.vue
    └── UsageStatusBadge.vue
```

### 7.2 数据流

URL query 是已应用过滤器的唯一 source of truth：

- 保存 `range=24h|7d|30d|custom`、自定义 UTC start/end、IANA timezone 和全部
  API 对齐过滤：
  `request_id/route_id/service_id/provider_id/provider_type/requested_model/model_group/`
  `actual_model/virtual_key_id/consumer_id/status_code/outcome/stream/cache_status/`
  `usage_source/pricing_status/cost_status`；request ID 只做大小写敏感精确查询；
- snapshot 不进 URL；刷新页面自然取得最新 snapshot；
- 第一个 totals summary 返回 snapshot；
- 同一 snapshot 并行请求 hour/day 趋势、actual-model Top、Virtual-Key Top；
- 日志页按需请求 detail，保存 cursor stack 支持前后翻页；
- 过滤变化取消旧请求、清空 snapshot/cursor，再发新一代请求；
- `AbortController + generation` 防止慢响应覆盖新条件；
- 409 不自动吞掉，保留过滤并显示“数据窗口已滚动”，由用户刷新取得新 snapshot。

24h/7d 默认 hour，30d 默认 day；自定义范围不超过 7d 用 hour，否则 day。浏览器
本地时间用 `Intl.DateTimeFormat().resolvedOptions().timeZone` 转 UTC，页面始终
显示当前 IANA 时区。

Models 行的“查看用量”携带 `provider_id + model_group + actual_model`；Virtual
Keys 行携带 `virtual_key_id`。排行点击同样更新 URL 并下钻，不维护第二套隐式
筛选状态。

### 7.3 展示与状态

- 首屏 KPI 固定包含“可计算成本小计”、总请求、prompt/completion/total 三项
  “已知 Token 小计”、估算 usage 占比、三项 token usage coverage、pricing
  coverage 与 cost-calculable coverage；每个 token 小计紧邻展示自己的 unknown
  request count/coverage；
- 趋势图可在成本与 Token 间切换；Token 模式可选 prompt/completion/total，只绘制
  服务端时间桶小计和 coverage 提示，不在浏览器重聚合金额；
- `$0` 只用于真实 0；未知为 `—`，unmatched 为“未定价”，unavailable 展示原因；
- 日志主表通过可横向滚动的组合列完整显示时间、request ID、status code/outcome、
  route/service、provider/requested model/model group/actual model、Virtual Key
  非敏感 name/prefix、prompt/completion/total 与各自 source、input/output 生效
  价格 source、pricing/cost status 与 reasons、成本、e2e/TTFT、stream/cache；
- 详情抽屉补齐各实体 ID/name 快照、consumer ID、reasoning/cache usage
  breakdown、input/output rate/version/snapshot/effective period 和全部机器原因；
- 排行使用语义化 table，SVG 有文字图例、键盘 tooltip 和可访问名称；
- 窄屏让过滤器换行、日志表横向滚动，详情抽屉占满可用宽度；
- 明确区分初次 loading、时间窗无调用、过滤无匹配、501 能力不可用、普通 API
  错误和 409 snapshot 失效；
- DB-less 顶部常驻显示本节点、容量、最早记录时间和重启清空；
- Hybrid 导航仍可见，页面展示能力说明，不能用零 KPI 冒充无调用。

大量新增文案进入 `locales/en.json` / `zh-CN.json` 的 `aiUsage.*` namespace；
`useAiGatewayI18n.ts` 只补导航和“查看用量”等现有页面短文案。

### 7.4 Models 定价交互

`Models.vue` 的 `input_cost/output_cost` 改为字符串表单值，字段标签固定为：

- 中文：“自定义 Input 覆盖价（USD / 1M tokens）”与“自定义 Output 覆盖价
  （USD / 1M tokens）”；
- English：“Custom input override (USD / 1M tokens)”与
  “Custom output override (USD / 1M tokens)”。

两项 helper 明确：“留空使用当前内置价格；填写 0 表示该方向免费”。保存时空串发
null，其他值发送未转为 JavaScript number 的规范十进制字符串；客户端只做非负、
位数和格式提示，后端校验仍是权威。

Models 列表的价格单元格和编辑抽屉都分别展示 effective input/output：

- amount + `USD / 1M tokens`；source=override 显示“自定义覆盖”，source=builtin
  显示“内置价”；
- direction 为 null 或整体 unmatched 时显示“未定价”与 `—`，绝不显示 `$0`；
  显式 0 显示 `$0 / 1M tokens` 并保留“自定义覆盖”徽章；
- 内置方向显示 snapshot date/version，页面同时展示 catalog snapshot date；
  override 方向显示 Model 版本/更新时间快照；
- unsupported 显示价目及条件说明，但标记“当前条件不支持计价”，不能假装为
  可计算成本。

input/output 可来自不同 source，UI 按方向渲染，不能用一个模型级 badge 掩盖混合
来源。`conditions` 以只读 helper 列出 token 阈值等条件，前端不复制价表匹配逻辑。

## 8. 配置、装配与依赖改动

| 位置 | 改动 |
|---|---|
| `Cargo.toml` | `sqlx` 开 `chrono,rust_decimal`；新增 `rust_decimal`、`bigdecimal`、`num-bigint`、`chrono-tz` workspace 依赖 |
| `kong-core` | 通用生命周期、route/service/workspace snapshot |
| `kong-plugin-system` | lifecycle observer trait；短路/插件错误 hint |
| `kong-proxy` | 单一 request 时钟、尽早保存链、transport/stream 终态、observer begin/finalize |
| `kong-ai` | `usage/` 全模块、provider codec 扩展、AiAuthContext prefix、model 临时 ID 语义修复 |
| `kong-db` | migration 006、Model 金额 NUMERIC、PgDao Decimal、reset 表顺序 |
| `kong-config` | 五项 `ai_usage_*` 配置、默认值、解析与范围校验 |
| `kong-server` | 解析 default workspace；按 mode 构造 store/writer/collector；注册 writer BackgroundService |
| `kong-admin` | AdminState store/stats、两条 API、status/metrics、Model 价格扩展与校验 |
| `kong-manager` | 调用统计路由/页面/类型/i18n；Models/Virtual Keys 下钻 |

配置校验：容量/批次/时间必须大于 0，`batch_size <= queue_capacity`；非法配置启动
失败，不静默回退。retry 次数/退避和 5 秒 query timeout 首版为内部常量，不扩大
配置面。

## 9. 隐私、兼容与运行约束

- 新 analytics 事实/API/cursor/Manager/writer/analytics 诊断日志均不保存/返回
  prompt、响应正文、请求/响应 headers、Authorization、`x-api-key`、provider
  `auth_config`、Virtual Key 原文或 `key_hash`；既有显式 opt-in 的
  `log_payloads` 兼容日志行为不在本单改变，但其 payload 绝不流入 usage fact；
- writer 错误日志不序列化事实；provider 错误 body 不进入 termination hint；
- 配置实体删除不级联事实，查询只读快照，不 join 当前实体；
- 未包含 `ai-proxy` 的 Route 不创建草稿或事实；现有 AI API 与 Endpoint 默认入口
  不变；
- 现有 `ai.*` 兼容日志继续可用；Prometheus `ai_llm_*` 对齐仍属于 REQ-AI-004；
- DB-less 仅本节点、容量有限、重启清空；Hybrid 不上传、不聚合；
- 首版无 retention/partition/export，运维文档明确数据持续增长风险；
- 本表允许进程崩溃/队列满/数据库长期故障时丢失，不用于财务审计或硬预算执行。

## 10. 测试与验收证据

### 10.1 生命周期与一请求一事实

- 成功非流式、Chat stream、Responses pass-through/translation stream；
- 缺/错 key 401、model-not-allowed 403、Prompt Guard、rate-limit 429；
- body/JSON/config/选模错误、无/禁用 Service；
- 缺 key + 坏 JSON 仍先返回 401，rewrite 浅提取不改变 access priority；
- upstream 4xx/5xx、DNS/connect/timeout、客户端断开、流中断、空流；
- upstream 2xx 后 response transform 失败为 gateway_error；stream 配置下的
  upstream 4xx 不因缺协议终态误判为 stream_interrupted；
- response/body filter 只记 send attempt；仅 logging(error=None) 记 completed，
  Downstream error 即使已见协议终态仍为 client_disconnected；
- 非 AI Route 不记录；finalize 重入只入队/落库一次；
- nested/top-level `log_statistics` 相反值、默认值与 false 仍采事实；无 observer /
  Hybrid 仍保留兼容 `ai.*` 日志；
- 每条路径校验 request ID、route/service、attempt/outcome/e2e 和稀疏字段。

### 10.2 normalizer 与 pricing

- OpenAI Chat/Responses 非流与流式 cached/reasoning fixture；
- Anthropic message_start/cache + 最后累计 output，不重复相加；
- Gemini 多个累计 snapshot、thinking/cache、缺主字段；
- provider total 与分项不一致仍保留官方 total，partial 不清零；
- 负 token、`i64::MAX+1`、Anthropic/Gemini/total checked-add overflow：事实保留、
  usage unavailable，不拖垮 batch；
- 全部内置 ID/alias/单价；provider 隔离、禁止 latest/模糊匹配；
- 单/双方向 override、显式 0、未知模型、cache/tool unsupported；
- GPT prompt 272000/272001、Sonnet effective 切换边界；
- Decimal 极小/极大/12 位量化、公式 overflow→unavailable、历史价不随
  catalog/model 更新漂移；
- Model 规范 string 与 legacy number 输入、固定 12 位精确响应、负数/NaN/Infinity/
  超范围/不可无损量化校验；DB-less 声明式 string/number parity。

### 10.3 migration、writer 与 Store

- 005→006 真实升级、全新 bootstrap、migration 注册、KNOWN_TABLES、CHECK/index；
  旧 float 超 12 位无损范围时 fail-fast，直写 numeric NaN 被 CHECK 拒绝；
- 状态交叉 CHECK 覆盖 usage/source、price bundle、pricing/cost/reasons/currency；
- 删除 route/service/model/provider/key/consumer 后事实仍在；
- 1/256 行批写、重复 request ID、事务回滚、提交结果未知后的幂等重试；
- 两连接复现晚提交竞态，验证 advisory lock；
- size/timer flush、2 秒内可见、队列满不阻塞、故障恢复、永久/瞬态 SQLSTATE、
  commit outcome unknown；
- shutdown 恰好发生在 proxy 已接收但 collector begin 前，验证完整 5s window、
  deadline 后 writer_closed/drop 计数；
- writer `/status` 和 Prometheus counters；日志无事实正文；
- PG 关键 list/summary 用 `EXPLAIN (ANALYZE, BUFFERS)` 验证索引；
- 性能压测对比开关前后，代理 p95 回归不超过 5%、错误率不增加。

### 10.4 API、summary 与运行模式

- GET/HEAD/OPTIONS/405 与 `/endpoints` 登记；
- 全部 exact filter、request ID 大小写敏感、`[start,end)`、24h 默认、90d 边界；
- 同 started_at、并发新增、分页无重漏、坏/超长 token、filter/backend/workspace/
  watermark/last-key mismatch；
- detail 与 totals 对账；known/unknown、零分母 null、全部 status counts；
- PG/DB-less P95 parity、Top 并列稳定排序、null group、other 从原始行重聚合；
- 极值合法事实的 BigDecimal/BigInt PG/内存汇总，不发生 Decimal/i64 overflow；
- America/New_York 春跳/秋回、Australia/Lord_Howe 半小时 DST、
  Pacific/Apia 跳日、23/25h day、Asia/Kathmandu 非整点 offset；TimeBucketPlan
  starts 严格递增且 bucket end 取下一真实边界；
- 事实/query 毫秒量化边界的 PG/DB-less `[start,end)` parity；
- hour 31d/超 31d 与 744/745 桶分别校验（含跳时区），day 90d/超 90d 与
  90/91 桶分别校验；
- 明细与首个 summary 返回相同完整 AiUsageMeta；DB-less 容量/淘汰/generation
  409/重启为空/meta；旧 token 对新 Store instance 固定返回 409；
- CP 与 DP 对合法/非法 query 均先返回 501；query timeout/semaphore 满返回 503；
- 既有 arbitrary default workspace ID、route ws 缺失、非 default 排除、selector 400；
- API schema 断言不存在 payload/header/key/hash/auth 字段。

### 10.5 Manager

- 先修正 `01-AiGateway.spec.ts` 过时的 `management metadata only` 断言；
- 新增 `02-AiUsage.spec.ts`：URL 保持、snapshot 并发请求、稳定翻页、下钻；
- 全部 API 对齐过滤器与 request ID 精确查询；成本/Token 趋势切换；
- 总请求、三项已知 token/unknown/coverage、估算占比、pricing/cost coverage KPI；
- 日志组合列与抽屉覆盖身份快照、usage/price source、pricing/cost status/reasons、
  e2e/TTFT/stream/cache；
- loading、两类空态、503 重试、409、501、DB-less、未定价/未知/真实 0；
- Models 双向覆盖价标签/helper、空值回退、显式 0、混合来源、价表快照与
  conditions；Models/Virtual Keys “查看用量”；
- 双语、窄屏、键盘/SVG 可访问性；
- 真实 mock upstream 发非流/流请求，`expect.poll` 等待事实后验证趋势/排行/日志。

### 10.6 验收标准追踪

| analysis AC | 主要设计章节 | 核心证据 |
|---:|---|---|
| 1 一请求一事实 | §3、§10.1 | 生命周期矩阵、幂等、真实代理路径 |
| 2 usage 归一化 | §4.1~4.3、§10.2 | 四类 surface fixture、缺失/估算/溢出 |
| 3 价格与覆盖 | D3~D5、§4.4~4.5 | 全价表、边界、override/version、历史快照 |
| 4 成本与 coverage | §4.5、§6.4 | Decimal/BigDecimal、状态机、detail-summary 对账 |
| 5 明细与游标 | D7/D9、§6.1~6.3 | snapshot、同时间戳/并发分页、全过滤 |
| 6 summary | §6.4~6.5 | 完整 DTO、Top/Other、DST、PG/Memory parity |
| 7 Manager | §7、§10.5 | 真实 API Playwright、状态/双语/可访问性 |
| 8 隐私与历史 | §5.1、§6.3、§9 | schema/API 禁止字段断言、实体删除测试 |
| 9 writer/性能 | D6、§5.4~5.5 | 故障/drop 指标、2 秒可见、p95 对比 |
| 10 DB-less/Hybrid/migration | D8、§5.1/5.6、§6.6 | 淘汰/重启 409、先行 501、PG15/16 升级 |

编码阶段按风险逐层验证：

```bash
cargo test -p kong-ai --locked
cargo test -p kong-proxy --locked
cargo test -p kong-db --locked
cargo test -p kong-admin --test ai_usage_api --locked
cargo test -p kong-admin --test admin_api_compat --locked
cargo check -p kong-server --locked
make test-dbless
pnpm lint
pnpm build
pnpm test:e2e
make fmt-check
make lint
git diff --check
```

真实 PG migration/集成测试至少覆盖 PostgreSQL 15 与仓库受管 PostgreSQL 16；
是否执行会清卷的 `make test-pg` 按当时本地数据情况决定并明确报告。

## 11. 文档与跟踪

设计门禁完成、进入编码时先在 `docs/tasks.md` 登记未完成实现任务；编码门禁完成时
再同步：

- `docs/ai-gateway-guide.md` / `_cn.md`：价格口径、状态、API、Manager、
  DB-less/Hybrid 和增长风险；
- `docs/design.md`：事实采集边界、专用 Store、运行模式和依赖关系；
- `docs/tasks.md`：把实现任务更新为完成并补验证摘要；
- `docs/implementation-logs/`：migration、生命周期、投递语义、价格快照与验证证据。

本设计没有改变产品范围，不更新 `docs/requirements.md`。当前仅完成设计门禁，
除登记 pending 实现任务外，不在 guide、架构或 implementation log 中提前制造
实现进度。

## 12. 风险与应对

| # | 风险 | 应对 |
|---|---|---|
| R1 | Pingora 在不同断开点给出的 ErrorType/Source 有差异 | 使用结构化矩阵 + transport 集成测试；无法判明时宁可 gateway/upstream error，不用文本猜 client disconnect |
| R2 | 多实例 PG writer 的 sequence 晚提交破坏 snapshot | 批写事务 advisory lock；两连接确定性测试 |
| R3 | 大事实表与多个维度索引增加写放大 | 批写、只建高频 partial 索引、EXPLAIN 验证；retention/partition 后续立项 |
| R4 | 既有 Model 浮点价升级到 Decimal 时存在脏值或精度歧义 | 迁移前置审计并对脏值 fail-fast；合法值按 PG 文本表示转换；API/DAO 此后只用 Decimal |
| R5 | provider 流式终态/usage 事件差异导致 partial 误判 | surface 专用 fixture、显式 snapshot/partial kind、终态状态机 |
| R6 | DB-less 淘汰可能使本来无关的 snapshot 也失效 | 采用保守 409，保证不返回部分结果；Manager 保留条件并提供刷新 |
| R7 | PG15/16 聚合与内存时间边界产生细微差异 | 共用 chrono-tz TimeBucketPlan，跨 DST/非整点/跳日时区 parity fixture |
| R8 | 无 retention 导致长期增长 | guide、Manager/运维提示；90 天查询上限；生命周期治理另立需求 |

上述风险均有实现与验证路径，无阻塞编码门禁的开放产品决策。

## 13. 编码任务拆分（下一门禁执行顺序）

1. 通用生命周期 + observer + 尽早保存插件链，补早期失败/transport 分类测试；
2. usage model/normalizer/provider fixture、TTFT 与临时模型 ID 语义；
3. 静态价表、Decimal resolver/cost 状态机、Model 价格校验与只读价格视图；
4. migration 006、Pg/Memory Store、snapshot/cursor、writer/background/stats；
5. Admin 明细/summary API、PG aggregation/DST/Top-other、DB-less/Hybrid；
6. 端到端一请求一事实、writer 故障/性能与 PG/内存 parity；
7. Manager 调用统计、下钻、i18n、原生 SVG 与 Playwright；
8. guide 双语、架构、tasks、implementation log 和最终全量验证。
