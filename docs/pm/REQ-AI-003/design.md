# REQ-AI-003 方案设计 — Virtual Key 配额与预算控制

> Quota & Budget Enforcement — Solution Design
>
> - **状态：** ✅ 方案设计定稿（2026-07-26）；实现处于编码收口 / 验证中
> - **需求分析：** [analysis.md](analysis.md)（FR-1~11、16 条验收标准与
>   13 项产品决策以其为准）
> - **下一门禁：** 编码验证与验收证据收口
> - **范围：** `kong-core` / `kong-config` / `kong-plugin-system` /
>   `kong-proxy` / `kong-ai` / `kong-db` / `kong-admin` / `kong-server` /
>   `kong-manager` / 文档与测试

## 1. 方案概述

本需求把 Virtual Key 的静态限额字段接入真实请求路径，并将三类状态严格分层：

1. **配置与身份**继续由 Virtual Key 实体和 `ai-key-auth` 提供；
2. **实时 RPM/TPM**只通过异步 `RateLimitStore` 访问，首版为本进程 Memory，
   REQ-AI-009 可直接替换为 Redis；
3. **预算 aggregate 与审计账本**只通过 `BudgetAccountant/BudgetStore` 访问，
   首版 PostgreSQL primary 是强一致权威，绝不依赖 best-effort usage writer。

```text
有效插件链：ai-key-auth(774) → ai-rate-limit(771) → ai-proxy(770)
        │
        ├─ AiPolicyChainObserver（同步、无 I/O）
        │    └─ 保存有效链快照
        │
        ├─ ai-key-auth.access
        │    └─ AiAuthContext + VirtualKeyPolicySnapshot
        │
        ├─ ai-rate-limit.access
        │    ├─ BudgetAccountant.inspect(primary，预算启用/阻断时才调用)
        │    └─ RateLimitStore.admit（RPM/TPM 联合原子预扣）
        │
        ├─ ai-proxy.access（模型和 Provider 已选定）
        │    ├─ 解析并保存与 usage collector 共用的 pricing snapshot
        │    └─ BudgetAccountant.preflight
        │         └─ key 行锁 + pending/prepared intent
        │
        ├─ RequestDispatchHook（真正交给上游前）
        │    └─ pending/prepared → pending/dispatching；提交成功才允许联网
        │
        ├─ provider / downstream / stream
        │
        └─ KongProxy::logging
             ├─ AiUsageCollector::finalize（同步、无 I/O）
             │    └─ Arc<AiUsageFact> 写入 RequestCtx
             ├─ AiEnforcementFinalizer（异步、有界）
             │    ├─ RateLimitStore.settle
             │    └─ BudgetAccountant.finalize(primary)
             └─ 普通插件 log
```

这样做有四个关键结果：

- 预算和配额 finalization 不受普通插件 `log` 的优先级或提前报错影响；
- 成本结算直接复用 REQ-AI-002 同步生成的 `AiUsageFact`，analytics 队列满、写入
  PG 失败或未来切到 ES 都不影响预算；
- `prepared` intent 的请求尚未获准联网，可以在确认过期后自动结算
  `not_incurred=0`；`dispatching` 后崩溃则保守进入 unresolved；
- Memory、PostgreSQL 只是 adapter，插件、响应契约和 Manager 状态不包含
  `DashMap`、SQL、Redis key 或 ES index 等物理存储细节。

## 2. 关键设计决策

### D1 使用共享 `AiEnforcementRuntime`，不让插件自行创建 Store

`kong-server` 按运行模式构造一份共享运行时，并注入 `ai-rate-limit`、
`ai-proxy`、dispatch hook、finalizer 和 Admin：

```rust
pub struct AiEnforcementRuntime {
    pub capability: AiEnforcementCapability,
    pub quota: Arc<dyn RateLimitStore>,
    pub budget: BudgetRuntime,
    pub policy_topology: Arc<ArcSwap<PolicyTopologySnapshot>>,
}

pub enum BudgetRuntime {
    Supported {
        accountant: Arc<BudgetAccountant>,
        active_intents: Arc<ActiveBudgetIntentRegistry>,
        descriptor: BudgetBackendDescriptor,
    },
    Unsupported {
        reason: BudgetUnsupportedReason,
    },
}
```

插件只编排领域命令和协议响应，不创建 Memory Store、不持有 `PgPool`，也不直接
操作计数或账本。测试通过 fake Store 注入。

`BudgetRuntime` 不暴露 PostgreSQL 枚举分支。首版 descriptor 为
`postgres/authoritative`；未来可替换为分片强一致账本。预算 limit、aggregate、
revision、clear-vs-intent 与 delete guard 必须由同一个 budget adapter/coordinator
原子维护，不能把 enforcement 换到外部 Store，却继续让独立 PG
`ai_virtual_keys` 行作为另一份可竞争的预算真相。

### D2 认证缓存只携带不可变 policy hint，预算权威判断始终读 primary

`AiAuthContext` 增加：

```rust
pub struct VirtualKeyPolicySnapshot {
    pub rpm_limit: Option<NonZeroU64>,
    pub tpm_limit: Option<NonZeroU64>,
    pub budget_guard_required: bool,
    pub accounting_blocked: bool,
}

pub struct AiAuthContext {
    pub virtual_key_id: Uuid,
    pub key_name: String,
    pub key_prefix: String,
    pub consumer_id: Option<Uuid>,
    pub client_protocol: AiClientProtocol,
    pub policy: VirtualKeyPolicySnapshot,
}
```

`budget_guard_required` 在 limit 非空、存在 pending 或 unresolved 时为 true；
`accounting_blocked` 只表示已有 unresolved。它们只是决定“是否进入预算主库路径”
的 hint，`budget_used`、当前 limit 和最终状态不能从认证缓存判断。

`client_protocol` 不能只由 credential header 猜测。`AiPolicyChainObserver` 预先
解析有效 `ai-proxy` 的 `client_protocol/route_type`；`ai-key-auth` 的显式
`error_format` 优先，其次使用 chain protocol，最后才按 `x-api-key`/path 推断。
因此 Bearer + Anthropic 配置在认证、限流和预算错误上仍返回 Anthropic envelope。

同进程 Admin 写成功后清空 authenticator cache，下一请求从 primary 重新加载。
其他节点和带外写仍遵循分析阶段确定的短暂最终一致边界。

### D3 `RateLimitStore` 冻结联合原子与幂等语义

旧同步 `RateLimiter` 删除，改为后端无关的 async trait。RPM/TPM 在一次
`admit` 中联合检查、一次提交；`settle` 只能使用 Store 签发的 opaque
reservation，不能重新拼接当前 key/window。

同 request ID 的 `admit` 重放返回原 decision；同 operation ID 的 `settle`
重放只返回原结果。结果不确定时只能 `inspect` 或用相同 ID 重放，禁止生成新 ID
再次增减。

### D4 Memory backend 使用“每个逻辑 key 一个锁”，并显式处理清理竞态

单个 bucket 内的 RPM、TPM、window、admission record 和 settlement tombstone
由同一短临界区保护。不同 key 并行；同 key 不会发生“先扣 RPM、TPM 失败后遗留
RPM”的部分提交。

cleanup 删除 bucket 前必须在 DashMap entry guard 内取得 active-operation lease，
并再次确认 window/record 均可回收，避免请求在已经从 map 脱离的旧 cell 上扣费。
容量满且无法回收返回 `Overloaded`，不随机驱逐活动 bucket。

### D5 增加 async dispatch hook 与 finalizer，不改变同步 observer 的约束

`RequestLifecycleObserver` 保持同步、不可阻塞。新增两个通用接口：

```rust
#[async_trait]
pub trait RequestDispatchHook: Send + Sync {
    fn failure_policy(&self) -> DispatchFailurePolicy;

    async fn before_upstream_dispatch(
        &self,
        plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    ) -> Result<(), LifecycleHookError>;
}

#[async_trait]
pub trait RequestDispatchAbortHandler: Send + Sync {
    async fn compensate_before_response(
        &self,
        ctx: &mut RequestCtx,
        cause: DispatchAbortCause,
    ) -> Result<(), LifecycleHookError>;
}

#[async_trait]
pub trait RequestFinalizer: Send + Sync {
    async fn finalize(
        &self,
        plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    ) -> Result<(), LifecycleHookError>;
}
```

`KongProxy` 在 policy/model/provider 已确定、但 DNS/peer/外部网络尚未开始且
`mark_upstream_attempted()` 之前依次调用 dispatch hook；hook 可设置结构化短路
响应，代理随后走正常 short-circuit 与 header-filter 流程。

runner 对每个 hook/finalizer 单独施加配置化 timeout。dispatch hook 的
`error/timeout/panic` 不能与 finalizer 一样“记录后放行”：对
`FailClosed(protocol_error)` hook，runner 记录低基数结果、继续调用剩余 hook 做
必要清理，但最终必须设置该固定 503 并禁止 upstream。`AiBudgetDispatchHook`
永远注册为 fail closed，panic/timeout 映射
`budget_accounting_unavailable`；预期的 budget/quota 领域故障由 hook 设置更精确
的固定响应。

一旦任一 critical hook 失败，runner 先不可逆地设置 `dispatch_forbidden`，再在
short-circuit/header-filter **之前**调用独立的 `AiDispatchAbortCompensator`，不能
回调刚刚 panic 的 budget hook：

1. 同步把 budget guard 转 `NeedsSafeZero`（因为 runner 保证此后绝不联网）；
2. 在 await 前把固定 quota `{0,0}` settlement command 写入 request context；
3. 查询/重放 quota settlement，成功则把退款后的 snapshot 写入
   `response_snapshot`；timeout/unknown 时保留 command 给 finalizer，并清除
   admission snapshot，使本次 503 省略 quota headers；
4. 最后生成固定 503，并继续正常 header-filter/logging。

abort compensation 自身失败仍不允许 upstream，也不能回退为 admission snapshot。
finalizer 已发生在客户端结果确定后，才允许彼此隔离并继续，从而一个实现不会无限
挂住其他 finalizer 或普通日志插件。

在 `logging()` 中，先调用同步 observers 形成 `AiUsageFact`，然后在任何 await
之前冻结 request latency、response size 和 `log_serialize`，再调用所有 async
finalizer，最后执行普通插件 log。这样 PG settlement 等待不会被计入已经完成的
客户端请求时延，也不会改变 usage fact 的 finished time。

### D6 价格只解析一次，成本公式只保留一份

抽取 REQ-AI-002 已有价格解析和 Decimal 成本计算为共享只读组件。预算启用时，
`ai-proxy` 在选定实际 Model/Provider 后生成 `ResolvedPricing`，写入
`AiUsageContext` 并传给 budget preflight；collector finalize 复用同一 snapshot。
没有预算时 collector 可保持 finalize 时解析的现有路径。

当前 `PriceCatalog::resolve(..., upstream_attempted=false)` 会返回
`NotApplicable`，不能直接用于 provider 前预检。编码时拆成与 attempt 无关的
`resolve_snapshot(...)` 和 finalize 才调用的 `calculate_cost(...)`；是否发生
上游调用只影响最终 cost status，不影响价目本身是否可解析。

预算 preflight 只判断本次调用能否按当前计费模式安全形成成本。价格 unmatched、
unsupported 或必要方向缺失时在上游前返回 `budget_pricing_unavailable`；显式
零价合法。若价目含 `max_prompt_tokens` 等条件，preflight 可用估算 prompt 做早期
拒绝，但 finalize 必须用标准化实际 prompt 再验证冻结 snapshot；实际值越过条件
时 cost 转 unavailable、intent 转 unresolved，不能按错误档位盲算。最终金额仍由
`AiUsageFact` 的 `CostStatus/cost_usd` 决定。

### D7 PostgreSQL 账本与 aggregate 同事务，analytics 完全解耦

每个预算请求在 provider 前创建独立 intent；finalize 在同一事务内：

1. 锁 Virtual Key；
2. 锁 request intent；
3. 首次结算时更新 `budget_used`；
4. 把 intent 置为 terminal。

`ai_usage_logs` 只提供可空 `usage_fact_id` 关联，不建外键。外部 usage/log sink
即使迁到 Elasticsearch、OpenSearch、ClickHouse 或消息管道，也不参与预算准入、
结算、重建或 reconciliation。

“解耦”同时包括资源隔离：`PgBudgetStore` 使用有上限的专用 primary connection
pool/连接配额、短 acquire/lock/statement timeout 和独立 metrics；usage writer
继续使用 analytics pool，不能占光 budget 连接。两者首版仍可能共享同一 PG
实例、WAL、磁盘和 CPU，因此不是物理隔离；重负载下必须监控 budget latency/WAL
lag 并 fail closed，REQ-AI-013 外置 usage sink 后才能消除主要写入争用。未来还可
把 budget adapter 部署到独立事务存储，而不改变上层契约。长期重建不依赖永久
全表扫描：同一 budget adapter 还维护经 revision/CAS 校验的 account checkpoint，
rebuild 只聚合 checkpoint 后的 tail；checkpoint 不能由 analytics/ES 生成。

### D8 account 写固定 key-first；ledger-only 事务不得反向取 key 锁

所有会改变 aggregate/count 或 accounting ledger 投影的路径统一：

```text
BEGIN(primary)
  → SELECT ai_virtual_keys ... FOR UPDATE
  → SELECT/INSERT ai_budget_ledger
  → 更新 aggregate/count/revision/tail-events
  → 更新或插入 ledger
COMMIT
```

governance/periodic checkpoint 等 key-only/CAS 路径可以省略 ledger step，但仍先锁
key；不得为了统一模板创建虚假账本行。

`prepared → dispatching` 只改变单条 intent，不改 aggregate，因此使用独立
ledger-only 短事务；这类事务一旦取得 ledger 行锁，**本事务内禁止再获取 key
锁**。其他写路径禁止 ledger-first、read replica、advisory/global lock。不同 key
可以并行；单个热点 key 的 preflight/finalize 因强一致 aggregate 必然串行，这是
首版 PostgreSQL adapter 的明确容量边界。

### D9 owner-session heartbeat 区分可自动恢复与必须人工对账

固定 intent timeout 会误伤长 SSE，逐 intent 续租又会产生
`active_intents / heartbeat` 的持续写放大。因此每个进程启动生成唯一
`budget_owner_session_id`，只续租一行 owner-session heartbeat；intent 保存该
session ID 与最早 stale 时间。只有 owner heartbeat 已过期且 intent 已超过最早
stale 时间，scanner 才自动判定进程已失联。intent 生命周期：

```text
pending/prepared
  ├─ dispatch 提交成功 → pending/dispatching
  ├─ 上游前结束       → settled/not_incurred/0
  └─ 过期              → settled/not_incurred/0

pending/dispatching
  ├─ fact 可结算        → settled
  ├─ cost unavailable   → unresolved
  └─ 过期/进程崩溃      → unresolved

unresolved
  ├─ 尚未人工处理且晚到 fact 可确定 → settled
  └─ Admin settle/waive             → resolved + reconciliation entry
```

在 dispatch commit 成功与 socket 实际写出之间崩溃仍无法证明 provider 未收到
请求，因此只能 unresolved；除非未来 provider 提供端到端幂等/事务协议，这个
分布式边界不能伪装成自动恢复。

owner 仍存活但 intent 超过最大预期时长时只告警，不自动处理；避免误伤合法长流。
但“owner 活着”不能成为永久悬挂的理由：每个进程还维护有界
`ActiveBudgetIntentRegistry`。注册表 permit 必须在 preflight 建立 pending intent
**之前**取得，并返回请求任务持有的 cancellation-safe `BudgetIntentGuard`。entry
显式经历
`Preparing → ActivePrepared → DispatchCommitPending → ActiveDispatching →
RetryWithFact | NeedsSafeZero | NeedsUnresolved`，且记录 guard 是否仍存活；数据库中
属于当前 session 的 pending intent 必须能在注册表找到。finalize 成功后才 ack 并
释放 permit。

guard 的同步 `Drop` 不做 I/O，而按最后状态原地转 recovery：preflight outcome
未知时要求查询 intent；已确认 prepared 转 `NeedsSafeZero`；dispatch commit pending/
confirmed 但没有 fact 时保守转 `NeedsUnresolved`。dispatch hook 在发起事务前先写
`DispatchCommitPending`，确认回滚才退回 prepared，避免 COMMIT ACK/任务取消窗口被
误判为零成本。这样 request task 取消、finalizer panic、重试耗尽或通知队列满时，
恢复器仍能用保留的强类型 fact 重试，或把 intent 转 unresolved，不会因 owner
heartbeat 持续存活而永远漏账。

人工 reconciliation 只禁止处理“仍为 pending 且 owner session 有效”的 intent；
`unresolved` 已是可人工处理、也可由晚到 fact 修正的账务状态，不受 owner 是否
存活影响。进程活着但 current session 的 pending intent 不在注册表时，说明请求级
状态已丢失，恢复器立即将其转
unresolved 并告警；不要求重启进程，也不能用 UI 确认框直接 waive 一个仍在
`Active` 的请求。

owner session 同时是 fencing token，不只是心跳时间戳。所有时间判断使用 PostgreSQL
`clock_timestamp()`；heartbeat 只允许续租尚未 stopped 且**当前仍未过期**的行，
0-row 表示该 session 永久失去所有权，禁止用同一 UUID upsert/复活。runtime 立即
停止该 session 的新 preflight/dispatch；数据库恢复后只能生成新 session，并先让
旧 registry entry 进入 recovery。create intent、mark dispatching、scanner 与
pending reconciliation 都在各自写事务内重验 live/dead predicate，因此迟到
heartbeat 与 recovery 不能形成双主。

### D10 金额内部只用 Decimal，Admin 用精确字符串作为规范字段

`AiVirtualKey.budget_limit/budget_used` 改为 `Decimal`，
PostgreSQL 为 `NUMERIC(28,12)`。API：

- `budget_limit_decimal` / `budget_used_decimal`：固定 12 位字符串，规范字段；
- legacy `budget_limit` / `budget_used`：兼容 number，无法安全投影时为 null；
- Manager 只读写 decimal 字符串，不用 JavaScript Number 计算金额；
- `budget_percentage_decimal` 不是金额列：极小 limit 下比例可接近 `1e30`，
  超出 `rust_decimal` 96-bit 范围；状态比较使用 `used >= limit × 0.8` 的 checked
  交叉比较，比例字符串使用 scaled integer + `BigInt`（或等价任意精度定点）
  计算并按 12 位小数 half-up 舍入，禁止 Decimal 除法溢出后返回 null/panic；
- `budget_used(_decimal)`、accounting count/state、`key_hash/key_prefix` 均为
  server-owned，普通调用方写入返回字段级 400。

### D11 capability 是显式状态，不用“能否碰巧跑通”推断

| 模式 | quota runtime | budget runtime |
|---|---|---|
| traditional + PostgreSQL | `local/memory` | `postgres/authoritative` |
| standalone DB-less | `local/memory/ephemeral` | `unsupported` |
| Hybrid CP/DP | `unsupported` | `unsupported` |

Hybrid 即使未来某个 DP 偶然拿到 Virtual Key 配置，也不能标记为受支持的 local
quota；配置预算后必须返回固定 unsupported 503。

### D12 Admin 与 runtime 共用有效插件解析和 coverage index

从 `PluginExecutor` 抽取不依赖 handler 的纯 `EffectivePluginResolver`，runtime
chain 与 Admin coverage 都按相同 global/service/route 覆盖规则解析。resolver
显式接收 `workspace_id`，所有实体关联都验证同 workspace，禁止 route/service/
plugin 跨 workspace 串链。历史 `ws_id IS NULL` 在 snapshot 边界统一归一为启动时
解析出的 `default_workspace_id`，不得让 `None` 与默认 workspace 形成两套 key。

PostgreSQL 模式新增专用 topology loader：在一个 bounded、read-only
`REPEATABLE READ` 事务/snapshot 中，通过 cursor 读完 routes/services/plugins 的
**全部分页**，不能用 `size=10000` 截断或让三类实体来自不同代；DB-less 则从一次
不可变 declarative config snapshot 构建。然后一次生成不可变
`PolicyTopologySnapshot`，其中同时包含 routes、services、plugins、effective
chains 与按 workspace 的 `AiPolicyCoverageIndex`：

- 有效 `ai-key-auth` Route 数；
- 同时具有 `ai-key-auth + ai-proxy + ai-rate-limit(virtual_key)` 的 Route 数；
- 无效 policy chain/config 数。

Virtual Key list/get 只做 O(1) coverage lookup，不按每个 key 全量扫描插件，避免
Manager 列表形成 N×Route×Plugin 成本。构建完成后用一次 `ArcSwap` 发布整个
snapshot，proxy routing 与 Admin coverage 始终读取同一 generation，不允许分别
更新多个锁。每次本机 Admin CUD commit 后递增 local `required_refresh_epoch`；
loader 只有在该 commit **之后新开的** DB snapshot 才可标记满足该 epoch。若 CUD
发生在分页中途，旧 load 即使完成也不能满足新 epoch，必须再刷新；因此 Admin
等待的是 `snapshot.satisfied_refresh_epoch >= target`，而不是一个可能来自旧 DB
snapshot 的本地 generation。refresh 失败保留旧 snapshot，暴露
stale/error/generation/unsatisfied epoch；超时则明确返回“已持久化但 refresh
未完成”的状态，不能谎报即时生效。

### D13 Manager 只消费服务端状态

Manager 不扫描 plugins，也不因 limit 非空就显示“已生效”。它只消费
`quota_enforcement`、`budget_status`、capability 和 endpoint counts；所有比例用
服务端 Decimal 字符串格式化。

### D14 首版明确是可运行基线，不冒充企业全局档位

本需求交付的部署演进边界如下：

| 状态类别 | 首版默认 | 企业规模目标 | 稳定抽象 |
|---|---|---|---|
| RPM/TPM 热状态 | 本进程 Memory | REQ-AI-009 Redis Cluster/Sentinel | `RateLimitStore` |
| usage/log analytics | 当前组合式 `AiUsageStore`：PG / DB-less ring | REQ-AI-013 外部分析存储并拆分读写 | 计划中的 `AiUsageSink` / `AiUsageQueryBackend` |
| 预算 aggregate/账本 | PostgreSQL primary | 可分片强一致事务账本 | `BudgetStore` |

Redis 不能代替预算审计账本，ES 也不能成为预算真相；未来扩容只能替换对应 adapter，
不能跨层混用。

### D15 API 和 Kong 兼容边界保持显式

`virtual_key`、生命周期 USD 预算、403 和 `X-RateLimit-*` 是 Kong-Rust 扩展。
旧合法 `global/route/consumer`、`error_code/error_message` 与 deprecated
`header_name` 保持兼容；非法 `limit_by` 不再静默回退。不得把本实现描述为 Kong
AI Rate Limiting Advanced 的 `policies`、多窗口或官方 header 完整兼容。

## 3. 模块与代码归属

```text
crates/kong-ai/src/
├── enforcement/
│   ├── mod.rs                    # AiEnforcementRuntime/capability
│   ├── policy.rs                 # chain snapshot/coverage/status
│   ├── dispatch.rs               # AiBudgetDispatchHook/AiDispatchAbortCompensator
│   ├── finalizer.rs              # AiEnforcementFinalizer
│   └── response.rs               # OpenAI/Anthropic error + headers
├── ratelimit/
│   ├── mod.rs
│   ├── types.rs                  # key/window/command/result
│   ├── store.rs                  # async RateLimitStore
│   ├── memory.rs                 # MemoryRateLimitStore
│   ├── clock.rs                  # System/Manual clock
│   ├── context.rs                # request reservation state
│   └── metrics.rs
├── budget/
│   ├── mod.rs
│   ├── model.rs                  # intent/ledger/Decimal DTO
│   ├── store.rs                  # async BudgetStore
│   ├── postgres.rs               # PgBudgetStore
│   ├── service.rs                # BudgetAccountant
│   ├── recovery.rs               # lease/retry/stale/checkpoint runner
│   └── admin.rs                  # reconciliation/rebuild service
└── virtual_keys/
    └── governance.rs             # 专用 CUD 安全边界
```

其他 crate：

| crate | 改动 |
|---|---|
| `kong-core` | 不新增 AI 类型；复用 `RequestLifecycle.request_id` |
| `kong-plugin-system` | dispatch/abort/finalizer interfaces、纯有效插件 resolver |
| `kong-proxy` | hook/abort/finalizer builder 与调用顺序；上游 attempt 标记移到 dispatch 成功后 |
| `kong-config` | bundled 注册及 bounded memory/budget recovery 参数 |
| `kong-db` | migration 007、Decimal schema、server-owned column 支持 |
| `kong-admin` | Virtual Key 专用 CUD、ledger/reconcile/rebuild、schema 校验、状态投影 |
| `kong-server` | 按模式构造 Store、hooks、runner、coverage index |
| `kong-manager` | VirtualKeys 精确预算、状态、表单、ledger reconciliation UI |

## 4. Policy chain 与 Virtual Key 快照

### 4.1 `AiPolicyChainObserver`

在 `on_plugins_resolved` 中解析有效插件：

```rust
pub struct AiPolicyChainSnapshot {
    pub has_ai_key_auth: bool,
    pub has_ai_proxy: bool,
    pub rate_limit_mode: Option<AiRateLimitMode>,
    pub client_protocol: Option<AiClientProtocol>,
    pub config_error: Option<AiPolicyConfigError>,
}
```

它只解析内存中的配置，不做 I/O。`limit_by=virtual_key` 时：

- 缺 `ai-proxy`：Store 零调用，返回 500 `ai_policy_chain_invalid`；
- 缺 `AiAuthContext`：Store 零调用，返回 401 `virtual_key_required`；
- 非法 runtime 配置：fail closed，不回退 global。

### 4.2 结构化 quota key

```rust
pub struct RateLimitKey {
    pub schema_version: u16,             // v1
    pub deployment_namespace: Arc<str>,  // adapter 配置，不含 node ID
    pub subject: RateLimitSubject,
}

pub enum RateLimitSubject {
    VirtualKey(Uuid),
    Global,
    Route(Uuid),
    Consumer(Option<Uuid>),
}
```

Virtual Key key 等价性只包含 deployment namespace 和 key UUID。同一 key 命中
多个 Route/Service/Plugin 时仍共享 bucket；route、service、endpoint、plugin、
name、prefix、hash、原始 key 均不得加盐。

`Consumer(None)` 只为保留旧 `consumer:` 空桶行为，不对它做新的安全承诺。

## 5. `RateLimitStore` 契约

### 5.1 Window、limit 与 snapshot

```rust
pub enum WindowAlgorithm {
    FixedFirstHit,
}

pub struct WindowSpec {
    pub algorithm: WindowAlgorithm,
    pub duration: Duration, // 本需求固定 60s
}

pub struct QuotaLimits {
    pub requests: Option<NonZeroU64>,
    pub tokens: Option<NonZeroU64>,
}

pub struct QuotaCharge {
    pub requests: u64,
    pub tokens: u64,
}

pub struct WindowIdentity {
    pub id: WindowId,
    pub generation: u64,
}

pub struct DimensionSnapshot {
    pub limit: u64,
    pub used: u64,
    pub remaining: u64,
}

pub struct RateLimitSnapshot {
    pub window: WindowSnapshot,
    pub requests: Option<DimensionSnapshot>,
    pub tokens: Option<DimensionSnapshot>,
}
```

backend 使用自己的权威时钟计算 `started_at/reset_at/reset_after`。header 直接使用
原子结果中的 `reset_after`，节点不能再次按本地时钟推导。

`inspect(Current)` 在无活动 window 时返回 prospective zero snapshot，不创建
bucket、不启动窗口。活动窗口收到不同 spec 时继续使用原 spec，新 spec 仅在下一
generation 生效。

### 5.2 Opaque reservation

```rust
pub struct ReservationToken(Arc<ReservationEnvelope>);

// crate-private
struct ReservationEnvelope {
    schema_version: u16,
    backend_instance_id: BackendInstanceId,
    reservation_id: Uuid,
    request_id: Arc<str>,
    key: RateLimitKey,
    window: WindowIdentity,
    limits_at_admission: QuotaLimits,
    reserved: QuotaCharge,
}
```

token 不公开序列化、不输出敏感 Debug，并绑定签发 backend。未来 backend 切换时，
router 必须把 token 路由回原 backend，不能拿 Redis token 调 Memory 或反向调用。

### 5.3 命令与结果

```rust
pub struct AdmitCommand {
    pub request_id: Arc<str>,
    pub key: RateLimitKey,
    pub window: WindowSpec,
    pub limits: QuotaLimits,
    pub reserve: QuotaCharge,
}

pub enum AdmissionDecision {
    Allowed {
        reservation: ReservationToken,
        snapshot: RateLimitSnapshot,
        replayed: bool,
    },
    Rejected {
        reason: ExceededDimension,
        snapshot: RateLimitSnapshot,
        replayed: bool,
    },
}

pub struct SettleCommand {
    pub operation_id: Arc<str>,
    pub reservation: ReservationToken,
    pub final_charge: QuotaCharge,
}

pub enum SettlementDisposition {
    Applied,
    StaleWindowNoop,
    Replayed,
}

pub struct SettlementResult {
    pub disposition: SettlementDisposition,
    // Applied/Replayed 且原 window 仍活动时返回；stale 可以为 null
    pub snapshot: Option<RateLimitSnapshot>,
}

pub enum InspectQuery {
    Current { key: RateLimitKey, window: WindowSpec, limits: QuotaLimits },
    Admission { key: RateLimitKey, request_id: Arc<str> },
    Settlement { reservation: ReservationToken, operation_id: Arc<str> },
}

pub enum InspectResult {
    NotFound,
    Current(RateLimitSnapshot),
    Admission(AdmissionDecision),
    Settlement(SettlementResult),
}
```

固定 operation ID 为 `quota-settle:v1:<request_id>`。预算提前全退与最终
finalizer 必须共用这一 ID；一旦提交 `{0,0}`，后续不能改用新 ID 再收取。

### 5.4 Async trait 与错误

```rust
#[async_trait]
pub trait RateLimitStore: Send + Sync {
    fn descriptor(&self) -> RateLimitBackendDescriptor;
    async fn admit(&self, command: AdmitCommand)
        -> Result<AdmissionDecision, RateLimitStoreError>;
    async fn settle(&self, command: SettleCommand)
        -> Result<SettlementResult, RateLimitStoreError>;
    async fn inspect(&self, query: InspectQuery)
        -> Result<InspectResult, RateLimitStoreError>;
    fn stats(&self) -> RateLimitStoreStatsSnapshot;
}

pub enum RateLimitStoreErrorKind {
    Unavailable,
    Timeout,
    OutcomeUnknown,
    Overloaded,
    Corrupt,
    Unsupported,
}
```

Store 不返回 HTTP/Kong error。插件层映射：

| Store error | HTTP / code |
|---|---|
| unavailable/timeout/overloaded/重查后未知 | 503 `quota_backend_unavailable` |
| corrupt/幂等载荷冲突/token backend 不匹配 | 503 `quota_backend_state_invalid` |
| unsupported | 503 `quota_backend_unsupported` |

无权威 snapshot 时省略所有 quota headers。REQ-AI-003 只实现 fail closed；
REQ-AI-009 的 fail-open 必须在 Store 外显式配置，并标记 degraded。

## 6. Memory backend 算法

### 6.1 数据结构

```rust
pub struct MemoryRateLimitStore {
    instance_id: BackendInstanceId,
    buckets: DashMap<RateLimitKey, Arc<BucketCell>>,
    clock: Arc<dyn RateLimitClock>,
    config: MemoryRateLimitConfig,
    stats: Arc<RateLimitStoreStats>,
}

struct BucketCell {
    active_ops: AtomicUsize,
    state: Mutex<BucketState>,
}

struct BucketState {
    active_window: Option<WindowState>,
    admissions: HashMap<Arc<str>, AdmissionRecord>,
    last_touched_mono: MonoTime,
}
```

mutex 只覆盖纯内存计算，不跨 `.await`。`ManualRateLimitClock` 用于确定性窗口和
跨窗口测试。

### 6.2 `admit`

在同一 bucket lock 内：

1. 清理可过期的 replay/tombstone；
2. 若 request ID 已存在，fingerprint 一致返回原 decision，否则 `Corrupt`；
3. 必要时创建新 window；活动 window 保留创建时 spec；
4. 在局部变量分别 `checked_add` RPM/TPM 候选值；
5. 任一维度超过 limit：两个 count 都不写，保存 rejected 幂等结果；
6. 两者通过：一次提交两个 count，签发 reservation，保存 allowed 结果。

limit 在活动窗口内调高/调低会立即对现有 count 生效。维度设 null 后不再增加，
但原 count 保留；同窗口恢复时继续使用。settle 可把实际 token 修正到超过 limit，
它不重新执行 admission，下一请求才会被拒绝。

### 6.3 `settle`

在同一 bucket lock 内：

1. 校验 token schema/backend/request/window；
2. 找到原 allowed admission；
3. 已有相同 operation/payload 返回 `Replayed`；不同 payload 为 `Corrupt`；
4. window ID/generation 已变化或已过 reset：记录 terminal tombstone，返回
   `StaleWindowNoop`，不修改当前新窗口；
5. 计算 `current - reserved + final_charge`，所有运算 checked；
6. 一次提交两个维度并保存 terminal result。

最终 charge：

| 场景 | requests | tokens |
|---|---:|---:|
| budget preflight/dispatch 403 或 503 | 0 | 0 |
| 已准入、上游未尝试的其他本地失败 | 1 | 0 |
| 上游已尝试且 total 可得 | 1 | 标准化 actual total |
| total 不完整 | 1 | 至少保留准入 prompt 预扣 |
| cache 在 limiter 前命中 | 无 reservation，不 settle |

### 6.4 保留、清理与容量

```rust
pub struct MemoryRateLimitConfig {
    pub max_buckets: usize,
    pub max_idempotency_records: usize,
    pub max_records_per_bucket: usize,
    pub max_live_reservations: usize,
    pub recovery_record_headroom: usize,
    pub max_request_lifetime: Duration,
    pub settlement_retry_grace: Duration,
    pub cleanup_interval: Duration,
    pub cleanup_scan_batch: usize,
}
```

最低幂等保留期：

```text
window duration + max_request_lifetime + settlement retry grace
```

默认候选值在编码阶段以配置校验落地：

- `max_buckets=100_000`
- `max_idempotency_records=2_000_000`
- `max_records_per_bucket=100_000`
- `max_live_reservations=200_000`
- `recovery_record_headroom=50_000`
- `max_request_lifetime=15m`
- `settlement_retry_grace=5m`
- `cleanup_interval=30s`
- `cleanup_scan_batch=4096`

按 1000 QPS 和 21 分钟最低保留期估算约 126 万条 record，200 万上限留出重放、
拒绝和抖动余量；实际默认值仍必须用实测对象大小和吞吐校准，不是 SLA。单 key
若需要高于 `max_records_per_bucket / retention_seconds` 的持续吞吐，必须显式
调高 per-key 上限并重新做内存容量验证，不能让一个热点或恶意 key 默认占满全局。

容量统计区分 live reservation、普通 replay/rejected record 与 recovery record。
新唯一 admit 只能使用扣除 `recovery_record_headroom` 后的 admission 区；既有
request replay、settle 和 recovery 不受 admission 阈值阻断，并优先原地转换已有
record。`max_live_reservations` 单独限制未完成请求，不能由大量 rejected ID 挤占。
未 terminal 的 live reservation 不因普通 TTL 清除；window counter 可过期，但
tombstone 独立保留。bucket 只有在 window 过期、records 为空且 `active_ops=0`
时删除。

达到全局或 per-bucket 容量先执行一次 bounded cleanup，仍满时仅对该次**新唯一**
request 返回 `Overloaded`，且不再保存一个新的 overload/rejected replay record；
同 key 的既有 request replay 与 settlement 仍必须成功。per-bucket cap 只隔离该
key，全局 cap 仍保护进程；多 key 认证洪峰最终只产生受控 503 和告警，不能 OOM
或随机驱逐活动 key。删除 Virtual Key 不主动 O(N) 扫描；UUID 不复用，旧状态按
上述规则自然回收。

## 7. 请求生命周期与补偿

### 7.1 请求级 quota context

```rust
pub struct AiRateLimitRequestContext {
    pub key: RateLimitKey,
    pub admission_snapshot: Option<RateLimitSnapshot>,
    pub response_snapshot: Option<RateLimitSnapshot>,
    pub rejection: Option<ExceededDimension>,
    pub reservation: Option<ReservationToken>,
    pub settlement: QuotaSettlementState,
    pub headers_emitted: bool,
}
```

跨 `.await` 前先 clone command/token 并释放 AnyMap borrow，完成后再回写 context。

### 7.2 固定执行顺序

| 阶段 | 操作 | 失败补偿 |
|---|---|---|
| chain/auth | 校验链、身份、policy hint | 无 Store/ledger 变更 |
| budget inspect | primary 当前 limit/used/state | 403/503；quota `inspect(Current)` 仅取头 |
| quota admit | RPM/TPM 联合预扣 | 429 不建 intent |
| model selection | 选实际 Model/Provider | 普通解析/选择错误：RPM 保留、TPM 终态退还 |
| budget pricing | 解析预算所需 pricing/capability | 503：quota `{0,0}` 全退，不建 intent |
| budget preflight | key lock + pending/prepared intent | 403/503：quota `{0,0}` 全退 |
| dispatch hook | prepared→dispatching | 503：先禁止联网并由独立 abort compensator 全退 quota、safe-zero intent |
| upstream/finalize | usage fact + quota/budget settle | 有界重试；不能改已发送响应 |

RPM/TPM 都为 null 时只跳过 `RateLimitStore`，不能跳过 budget inspect、pricing
preflight、intent、dispatch 或 budget finalize；因此“没有实时限额”与“没有预算”
是两个独立判断。

预算初次 inspect 的 403 需要 quota snapshot 时调用 `inspect(Current)`；若 quota
Store 自身不可用，则 quota backend 503 优先，不能用伪造 headers 返回 403。

### 7.3 结果不确定

- quota `admit` ACK 丢失：`inspect(Admission)`，找不到再用相同 request ID 重放；
- quota `settle` ACK 丢失：`inspect(Settlement)` 或相同 operation ID 重放；
- budget preflight/dispatch commit ACK 丢失：查询同 request ID/operation；未确认
  前不调用 provider；
- 未联网的 commit-unknown intent 在预留 registry entry 中标记为高优先级
  safe-zero recovery；
- quota refund 结果未知时保存原 `{0,0}` command，finalizer 只能重放相同命令。

`AiEnforcementFinalizer` 分别执行 quota 与 budget settlement；两者没有跨 Store
事务，也不能因一方失败跳过另一方。实现可在各自 timeout 内并发执行，完成后独立
回写结果和 registry state。若预算 intent 已进入 dispatching 但 `AiUsageFact`
意外缺失，不能猜成 0：entry 转 `NeedsUnresolved`，runner 尽快收口；只有生命周期
可以证明未获准 dispatch 的 prepared 请求才可自动结算 `not_incurred=0`。

任何本应在响应前完成的 quota 补偿若 unavailable/timeout/unknown，最终对外错误
提升为 `quota_backend_unavailable`；corrupt 则为
`quota_backend_state_invalid`。原 budget/dispatch 原因仅内部记录，且无 snapshot
时省略 headers，不能返回看似已成功退款的 403/预算 503。

### 7.4 Header 注入

`ai-rate-limit.header_filter` 从 `response_snapshot` 统一 upsert headers，因此正常
代理与 short-circuit 都覆盖：

```text
X-RateLimit-Limit-Requests
X-RateLimit-Remaining-Requests
X-RateLimit-Reset-Requests
X-RateLimit-Limit-Tokens
X-RateLimit-Remaining-Tokens
X-RateLimit-Reset-Tokens
```

reset 毫秒向上取整为秒，最小 1。429 的 `Retry-After` 取超限维度 reset；两维都
超限取较晚者，并稳定优先 requests code。正常响应使用 admission snapshot；
budget preflight 全退后的 403/503 使用 settlement snapshot；log 时的最终 token
修正不会追溯改写已发送的 header。critical dispatch abort 的 refund 若未知，必须
清掉 admission/response snapshot 并省略 headers，不能把退款前 remaining 冒充
最终值。

## 8. `BudgetStore` 与 `BudgetAccountant`

### 8.1 稳定领域契约

```rust
#[async_trait]
pub trait BudgetStore: Send + Sync {
    fn descriptor(&self) -> BudgetBackendDescriptor;
    async fn inspect(&self, command: BudgetInspectCommand)
        -> Result<BudgetInspection, BudgetStoreError>;
    async fn create_intent(&self, command: CreateBudgetIntent)
        -> Result<BudgetIntent, BudgetStoreError>;
    async fn mark_dispatching(&self, command: MarkBudgetDispatching)
        -> Result<BudgetIntent, BudgetStoreError>;
    async fn settle(&self, command: SettleBudgetIntent)
        -> Result<BudgetSettlement, BudgetStoreError>;
    async fn register_owner(&self, command: RegisterBudgetOwner)
        -> Result<BudgetOwnerLease, BudgetStoreError>;
    async fn heartbeat_owner(&self, command: HeartbeatBudgetOwner)
        -> Result<BudgetOwnerLease, BudgetStoreError>;
    async fn stop_owner(&self, command: StopBudgetOwner)
        -> Result<(), BudgetStoreError>;
    async fn recover_stale(&self, command: RecoverStaleBudgetIntents)
        -> Result<BudgetRecoveryBatch, BudgetStoreError>;
    async fn checkpoint_account(&self, command: CheckpointBudgetAccount)
        -> Result<BudgetCheckpoint, BudgetStoreError>;
}

#[async_trait]
pub trait BudgetAdminStore: Send + Sync {
    async fn list_ledger(&self, query: BudgetLedgerQuery)
        -> Result<BudgetLedgerPage, BudgetStoreError>;
    async fn reconcile(&self, command: ReconcileBudgetIntent)
        -> Result<BudgetReconciliation, BudgetStoreError>;
    async fn verify_or_rebuild(&self, command: RebuildBudgetAccount)
        -> Result<BudgetRebuildResult, BudgetStoreError>;
}

#[async_trait]
pub trait BudgetAccountGovernance: Send + Sync {
    async fn create_account(&self, command: CreateBudgetAccount)
        -> Result<BudgetAccountSnapshot, BudgetStoreError>;
    async fn update_limit(&self, command: UpdateBudgetLimit)
        -> Result<BudgetAccountSnapshot, BudgetStoreError>;
    async fn delete_account(&self, command: DeleteBudgetAccount)
        -> Result<DeletedBudgetAccount, BudgetStoreError>;
}
```

公共命令只含 UUID、Decimal、时间、强类型 pricing/cost snapshot 和幂等 ID，
不暴露 `sqlx::Transaction`、SQLSTATE 或 PG row。

`BudgetStore` 是代理热路径最小契约；分页、人工 reconciliation 与 rebuild 放在
独立 `BudgetAdminStore`，未来高吞吐 adapter 不必把管理查询能力耦合进 admission
接口。`BudgetAccountGovernance` 冻结 limit/clear/delete 与 intent 使用同一
revision/并发控制域的要求。首版 PostgreSQL adapter 可由同一对象实现三个 trait；
`VirtualKeyGovernanceStore` 在同一事务 coordinator 内组合非预算字段更新；delete
必须在持有 key lock 的同一事务内完成，不能先返回 guard/permit 再由 generic DAO
删除。未来
外部账本必须同时提供强一致 account projection，或用 revision/CAS + outbox 让
PG 配置与外部预算账户形成单一可恢复提交，不能双写后各自成功。

`BudgetAccountant` 负责：

- primary inspect/preflight 的业务顺序；
- timeout、同 ID 重查和错误分类；
- owner-session heartbeat 与 stale recovery；
- 把 `AiUsageFact` 转为 settlement；
- cancellation-safe guard、safe-zero 与 registry recovery；
- 把 Store 错误映射为固定领域错误。

### 8.2 错误分类

```rust
pub enum BudgetErrorKind {
    Exhausted,
    AccountingUnavailable,
    AccountingUnresolved,
    Unsupported,
    PricingUnavailable,
    OutcomeUnknown,
    Corrupt,
    NumericOverflow,
}
```

`OutcomeUnknown` 在 Accountant 内先按相同 ID 有界查询/重放；仍未知才向请求映射
`budget_accounting_unavailable`。`Corrupt/NumericOverflow` 必须使账户进入
unresolved，不能按普通暂时故障放行。

## 9. Migration 007 与数据库 schema

### 9.1 注册与 fail-fast 审计

新增：

```text
crates/kong-db/migrations/core/007_ai_virtual_key_budget_accounting.sql
```

注册到 `CORE_MIGRATIONS`；`KNOWN_TABLES` 的 reset 删除顺序明确为
`ai_budget_ledger → ai_budget_checkpoints → ai_budget_owner_sessions →
ai_budget_runtime_settings → ai_virtual_keys`，先删运行时/子表再删 settings 与
Virtual Key。

迁移前逐 key 审计 `budget_limit/budget_used`。以下任一情况抛 SQLSTATE 22003，
异常包含 key ID、列名和原值，整项 migration 回滚：

```sql
value::text IN ('NaN', 'Infinity', '-Infinity')
OR value < 0
OR (value::text)::numeric >= 10000000000000000::numeric
OR (value::text)::numeric <> round((value::text)::numeric, 12)
```

`tpm_limit/rpm_limit IS NOT NULL AND <= 0` 同样 fail-fast。之后转换：

```sql
ALTER TABLE ai_virtual_keys
  ALTER COLUMN budget_limit TYPE NUMERIC(28,12)
    USING ((budget_limit::text)::numeric(28,12)),
  ALTER COLUMN budget_used TYPE NUMERIC(28,12)
    USING ((budget_used::text)::numeric(28,12)),
  ALTER COLUMN budget_used SET DEFAULT 0.000000000000;
```

新增非负金额、正整数 quota CHECK；不增加 `budget_used <= budget_limit`。

007 是 forward-only migration：升级前必须备份并先修复审计出的脏数据；迁移事务
只能保证本次升级失败时整体回滚，升级成功后不支持旧二进制直接 downgrade。若需
回退应用，必须先走单独评审的数据兼容/恢复方案，不能假设 `db down` 可无损还原
金额类型和新增账本。

### 9.2 Virtual Key 账务列

```sql
ALTER TABLE ai_virtual_keys
  ADD COLUMN budget_pending_count BIGINT NOT NULL DEFAULT 0
    CHECK (budget_pending_count >= 0),
  ADD COLUMN budget_unresolved_count BIGINT NOT NULL DEFAULT 0
    CHECK (budget_unresolved_count >= 0),
  ADD COLUMN budget_accounting_revision BIGINT NOT NULL DEFAULT 0
    CHECK (budget_accounting_revision >= 0),
  ADD COLUMN budget_checkpoint_tail_events BIGINT NOT NULL DEFAULT 0
    CHECK (budget_checkpoint_tail_events >= 0),
  ADD COLUMN budget_state_updated_at TIMESTAMPTZ(3)
    NOT NULL DEFAULT clock_timestamp(),
  ADD COLUMN budget_accounting_state TEXT
    GENERATED ALWAYS AS (
      CASE
        WHEN budget_unresolved_count > 0 THEN 'unresolved'
        WHEN budget_pending_count > 0 THEN 'pending'
        ELSE 'clean'
      END
    ) STORED;
```

pending 不阻止新请求，但阻止清空 limit 和删除；unresolved 阻止所有后续预算
请求。每次**影响 aggregate/count/settled amount 或追加幂等账务审计**的 ledger
insert/update 按受影响 row 数 checked 增加 `budget_checkpoint_tail_events`，
checkpoint CAS 成功才归零；它为 checkpoint 后扫描工作量提供 O(1) 保守上界。仅改变
`dispatch_state` 的 ledger-only transition 不推进 key revision/tail。count/revision/
tail/state 都是 server-owned。

### 9.3 `ai_budget_ledger`

hard tail 是跨节点正确性门禁，不能让各进程各用一个配置值。PostgreSQL adapter
先维护 deployment-scoped 权威设置：

```sql
CREATE TABLE ai_budget_runtime_settings (
  deployment_namespace          TEXT PRIMARY KEY,
  checkpoint_hard_tail_events   BIGINT NOT NULL
    CHECK (checkpoint_hard_tail_events > 0),
  config_fingerprint            CHAR(64) NOT NULL,
  updated_at                    TIMESTAMPTZ(3) NOT NULL
);
```

第一个 owner 注册时原子初始化；后续节点本地 hard 值不一致则 budget readiness
失败，不能监听后各自执行。变更 hard 值必须先 drain 该 deployment 的全部 owner
session，再以维护事务更新；soft trigger 可因节点不同而只影响效率。外部
`BudgetStore` 也必须在 descriptor/coordinator 内提供同等权威 hard guard。

进程会话表每个运行实例只有一行：

```sql
CREATE TABLE ai_budget_owner_sessions (
  session_id               UUID PRIMARY KEY,
  deployment_namespace     TEXT NOT NULL
    REFERENCES ai_budget_runtime_settings(deployment_namespace)
    ON DELETE RESTRICT,
  node_id                  UUID NOT NULL,
  started_at               TIMESTAMPTZ(3) NOT NULL,
  last_heartbeat_at        TIMESTAMPTZ(3) NOT NULL,
  expires_at               TIMESTAMPTZ(3) NOT NULL,
  stopped_at               TIMESTAMPTZ(3)
);

CREATE INDEX ai_budget_owner_sessions_expiry_idx
  ON ai_budget_owner_sessions(deployment_namespace, expires_at)
  WHERE stopped_at IS NULL;
```

heartbeat 只更新这一行；正常关闭写 `stopped_at`，崩溃则由 `expires_at` 判定。
owner 注册事务先锁对应 settings row、核对 hard config，再插入同 namespace
session；setting 变更只检查该 namespace 的 live owners。scanner 也必须按当前
deployment namespace 过滤，不能恢复共享数据库中另一 deployment 的 intent。
注册与续租都使用数据库时钟；续租 SQL 必须等价于：

```sql
UPDATE ai_budget_owner_sessions
   SET last_heartbeat_at = clock_timestamp(),
       expires_at = clock_timestamp() + $lease
 WHERE session_id = $id
   AND deployment_namespace = $namespace
   AND stopped_at IS NULL
   AND expires_at > clock_timestamp()
RETURNING expires_at;
```

返回 0 行即永久 fence；同一 session ID 不允许 `INSERT ... ON CONFLICT` 复活。

```sql
CREATE TABLE ai_budget_ledger (
  id                       UUID PRIMARY KEY,
  virtual_key_id           UUID NOT NULL,
  virtual_key_name         TEXT NOT NULL,
  virtual_key_prefix       TEXT NOT NULL,
  workspace_id             UUID,

  kind                     TEXT NOT NULL,
  status                   TEXT NOT NULL,
  request_id               VARCHAR(32) UNIQUE,
  operation_id             VARCHAR(128) NOT NULL UNIQUE,
  command_fingerprint      CHAR(64),
  dispatch_operation_id    VARCHAR(128) UNIQUE,
  terminal_operation_id    VARCHAR(128) UNIQUE,
  terminal_command_fingerprint CHAR(64),
  last_account_revision    BIGINT NOT NULL,
  parent_intent_id         UUID REFERENCES ai_budget_ledger(id) ON DELETE RESTRICT,
  usage_fact_id            UUID,
  attempt_no               SMALLINT NOT NULL DEFAULT 0,

  observed_cost_usd        NUMERIC(28,12),
  accounted_cost_usd       NUMERIC(28,12),
  cost_status              TEXT,
  cost_reasons             TEXT[] NOT NULL DEFAULT '{}',
  pricing_fingerprint      CHAR(64),
  pricing_snapshot         JSONB,

  dispatch_state           TEXT,
  node_id                  UUID,
  owner_session_id         UUID,
  stale_not_before         TIMESTAMPTZ(3),

  resolution_reason        TEXT,
  resolution_actor         TEXT,
  resolution_entry_id      UUID
    REFERENCES ai_budget_ledger(id) ON DELETE RESTRICT,

  created_at               TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
  updated_at               TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
  settled_at               TIMESTAMPTZ(3),
  resolved_at              TIMESTAMPTZ(3)
);
```

约束：

- `kind`:
  `request/opening_balance/reconciliation/reconciliation_attempt/account_issue/rebuild_audit`；
- `status`: `pending/unresolved/settled/resolved/waived`；
- request 必须有 32 位小写 hex request ID，其他 kind 必须为 null；
- request intent 必须保存 versioned canonical `command_fingerprint`；
  dispatch transition 保存固定 `dispatch_operation_id=budget-dispatch:v1:<request>`，
  结果不确定时据此核对，hash 输入只含白名单领域字段和 pricing fingerprint；
- `dispatch_state` 只能为 `prepared/dispatching`；request 的 pending 行必须有
  `owner_session_id`、`stale_not_before` 和 dispatch state，非 request 不得携带
  dispatch/owner/stale 字段；
- pending 的两个金额都为 null；
- settled 必须有 `accounted_cost_usd`；
- unresolved 可把已观察到但未入 aggregate 的合法金额保存在
  `observed_cost_usd`；
- waived 的 accounted 金额为 0；
- kind/status 组合由数据库 CHECK 固定：opening balance 只能 settled；
  reconciliation 只能 settled/waived 且必须有 parent；account issue 只能
  unresolved/resolved；失败的 reconciliation attempt 只能 resolved、必须有 parent/
  command fingerprint/outcome reason 且不参与 aggregate；rebuild audit 只能
  resolved；request 才能经历 pending、unresolved、settled、resolved；
- request 的 pending/unresolved 不占用
  `terminal_operation_id/terminal_command_fingerprint`；request 真正
  settled/resolved 时两个字段必须同时存在，非 request 行保持 null 并使用自身
  `operation_id/command_fingerprint`。terminal fingerprint 使用 versioned
  canonical settlement 字段（key/request、cost status、observed/accounted amount、
  规范化 reasons、pricing fingerprint），排除时间戳和 analytics-only
  `usage_fact_id`；同 operation 同 fingerprint 才是 replay，不同则 corrupt。晚到
  fact 因而仍可使用固定 settle operation，scanner 对 pending→unresolved 的幂等
  依靠行锁下状态条件更新，不发明第二个 operation；
- resolved 的 request parent 必须有 `resolution_entry_id` 指向本次
  reconciliation；resolved account issue 必须指向修复它的 rebuild audit。reason
  长度与 actor 来源满足 §10.6；自外键只保证存在，事务还必须校验 target 与 source
  的 key/workspace 一致及目标 kind 正确；
- 每次影响 aggregate/count/settled amount 或追加幂等审计的同 key 账务变更先
  递增 `budget_accounting_revision`，所有本次被插入或更新的 ledger rows 保存
  相同 `last_account_revision`；dispatch-only 更新保留原 revision，不使用跨 key
  全局 sequence；
- reconciliation 必须有 parent、reason、actor；
- reconciliation/reconciliation_attempt/rebuild_audit 与可重放的 account_issue
  都保存 versioned canonical `command_fingerprint`；同 operation ID 的 Admin/
  recovery 重放必须先比较 fingerprint；
- `attempt_no=0` 是本需求约束，字段为 REQ-AI-005 多 attempt 留位；
- `last_account_revision >= 0`；所有 key 与 ledger 金额列均以 CHECK 排除
  `'NaN'::numeric`、要求 `>= 0` 且 `< 10000000000000000`，不能只依赖 Rust
  validator；
- pricing snapshot 由白名单强类型 DTO 构造，禁止透传请求 JSON，规范 JSON
  最大 4 KiB，并有
  `pricing_snapshot IS NULL OR octet_length(pricing_snapshot::text) <= 4096`
  CHECK；重复字段优先使用 `pricing_fingerprint`/版本引用，避免把任意 provider
  配置复制进每行。

表故意不对 Virtual Key 或 usage fact 建外键。key 删除或 analytics 归档后，历史
账本仍保留 key ID/name/prefix/workspace snapshot；不保存原始 key、key hash、
prompt、response body 或 provider credential。`usage_fact_id` 只能在账务 transition
时随同步 `AiUsageFact` 一次写入，analytics writer 不得事后回填/修改 terminal row；
settled/waived/resolved row 保持不可变。

索引：

```sql
CREATE INDEX ai_budget_ledger_key_time_idx
  ON ai_budget_ledger(virtual_key_id, created_at DESC, id DESC);
CREATE INDEX ai_budget_ledger_open_idx
  ON ai_budget_ledger(virtual_key_id, status, created_at)
  WHERE status IN ('pending', 'unresolved');
CREATE INDEX ai_budget_ledger_owner_open_idx
  ON ai_budget_ledger(owner_session_id, status, stale_not_before)
  WHERE status = 'pending';
CREATE INDEX ai_budget_ledger_parent_idx
  ON ai_budget_ledger(parent_intent_id);
CREATE INDEX ai_budget_ledger_revision_idx
  ON ai_budget_ledger(virtual_key_id, last_account_revision)
  WHERE status = 'settled';
```

### 9.4 `ai_budget_checkpoints`

为避免多年 raw ledger 让 verify/rebuild 退化为全生命周期扫描，新增同一强一致
adapter 管理的 checkpoint：

```sql
CREATE TABLE ai_budget_checkpoints (
  virtual_key_id             UUID NOT NULL,
  checkpoint_revision        BIGINT NOT NULL CHECK (checkpoint_revision >= 0),
  accounted_cost_usd         NUMERIC(28,12) NOT NULL,
  operation_id               VARCHAR(128) NOT NULL UNIQUE,
  created_at                 TIMESTAMPTZ(3) NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (virtual_key_id, checkpoint_revision),
  CHECK (
    accounted_cost_usd <> 'NaN'::numeric
    AND accounted_cost_usd >= 0
    AND accounted_cost_usd < 10000000000000000::numeric
  )
);
```

checkpoint 表示“截至该 key revision 已验证的 settled 累计金额”，不是可独立修改
的第二份余额。新 key 在 revision 0 建立 zero checkpoint；migration 为旧 key 在
opening balance 完成后建立等值 genesis checkpoint。后续 checkpoint 只能从前一个
checkpoint 加上 revision tail 计算，并在短 key lock 下验证当前 revision 与
`budget_used` 完全相等后追加并把 tail-event counter 归零；成功 rebuild 也在同一
CAS 事务追加新 checkpoint/归零。
若 derived amount 与 stored used 不同，普通 checkpoint 不得掩盖漂移或归零 tail；
它在短 key-first 事务创建/复用 `account_issue/unresolved` 并要求 verify/rebuild。
任何会改变 ledger 金额/状态的旧 row 都必须写新的 `last_account_revision`，而
settled/waived/rebuild audit 一旦 terminal 不再修改，因此 checkpoint 前缀可安全
冻结。

soft tail-event 阈值触发后台 checkpoint；超过配置化 hard 阈值仍无法推进时，只
拒绝新的 preflight，finalize/recovery 继续使用保留资源收口，不能制造一个已无法在
timeout 内验证的无限 tail。raw ledger 首版仍保留用于审计；未来归档只可移动已被
checkpoint 覆盖且不可变的 terminal rows，并必须同时提供 Admin 历史查询与权威
request/operation 幂等 tombstone，不能删除 open intent、允许旧 request 重复入账，
或把 checkpoint 当明细。

### 9.5 Opening balance

每个旧 `budget_used > 0` 的 key 插入不可变 `opening_balance/settled`：

- `accounted_cost_usd = 旧 budget_used`；
- `operation_id = opening-balance:v1:<key_id>`；
- `last_account_revision=0`，`settled_at` 使用 migration 的数据库时钟；
- snapshot 使用迁移时的 key name/prefix/workspace；
- UUID 由 operation 文本的确定性 MD5 格式化生成，不依赖数据库 extension。

旧 used=0 不建 row。每个旧 key 在同一 migration 中保持 key revision/tail 为
`0/0`，并建立 `checkpoint_revision=0, accounted_cost_usd=旧 budget_used` 的
genesis checkpoint；新 key create 同样建立 zero checkpoint。首次运行时账务变更
从 revision 1 开始。迁移重放由 operation/checkpoint unique 保证幂等。

## 10. PostgreSQL 事务

### 10.1 初次 inspect

只对 `budget_guard_required=true` 的请求执行 primary 查询：

```text
读取 limit/used/pending/unresolved
  unresolved_count > 0 → 503 unresolved
  tail_events >= authoritative hard threshold → 503 accounting unavailable
  limit = null 且 pending > 0 → 非法账务组合，503 unresolved
  limit = null → paused，继续且不建 intent
  used >= limit → 403 exhausted
  存在符合 §11.1 双条件的 stale pending → 触发单 key promote，再返回当前结果
  其他 → eligible
```

普通非预算 key 不增加 DB I/O。

### 10.2 Preflight

模型与 Provider 已选、pricing 可用后，调用方先取得
`ActiveBudgetIntentRegistry` permit/guard；容量不足直接 503 并全退 quota，不开启
数据库事务。guard 进入 `Preparing` 后：

```sql
BEGIN;
SELECT budget_limit, budget_used, budget_pending_count,
       budget_unresolved_count, budget_checkpoint_tail_events,
       name, key_prefix, ws_id
  FROM ai_virtual_keys
 WHERE id = $1
 FOR UPDATE;

-- 再检查 paused/unresolved/exhausted/hard-tail
-- 用 DB clock 验证 owner session 仍 live
INSERT INTO ai_budget_ledger (...)
VALUES (..., 'request', 'pending', $request_id,
        'intent:v1:' || $request_id, 'prepared',
        $current_revision + 1, ...)
ON CONFLICT (request_id) DO NOTHING
RETURNING id;

UPDATE ai_virtual_keys
   SET budget_pending_count = budget_pending_count + 1,
       budget_accounting_revision = budget_accounting_revision + 1,
       budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1,
       budget_state_updated_at = clock_timestamp()
 WHERE id = $1;
COMMIT;
```

request ID 已存在时核对 key、operation 和 command fingerprint；完全一致返回
replay，不同载荷视为 corrupt/fail closed。

hard tail threshold 从 `BudgetStore` 的 deployment-authoritative setting 读取，
inspect 只负责早拒绝；key lock 内必须再次检查
`budget_checkpoint_tail_events < hard`，达到阈值返回
`budget_accounting_unavailable` 且不建 intent。owner live check 也在同一事务；
session 已 fenced 时不建 intent、runtime 停止该 session 后续准入。

上面的 aggregate UPDATE **只在 INSERT RETURNING 确实返回新行时执行**；编码时用
应用层分支或单条 CTE 绑定，冲突重放不得再次增加 `budget_pending_count`。

如果 initial inspect 后管理员已先取得同一 key 锁并把 limit 清为 null，preflight
返回 `Paused`、不创建 intent，并继续发送 upstream；这不是 403/503，既有 quota
reservation 也不退款，最终只结算 quota。非 null limit 调整则按锁内最新值重新
判断 exhausted。任何未创建 intent 的确定结果都立即释放 guard；commit outcome
unknown 则保留 guard 由 runner 按 request ID 查询。

### 10.3 Dispatch

`RequestDispatchHook` 在真正放行 upstream 前：

```text
registry: ActivePrepared → DispatchCommitPending
intent FOR UPDATE（ledger-only）
  → 用 DB clock 重验 intent.owner_session 仍 live
  → prepared 改 dispatching、写 dispatch operation
  → COMMIT 成功
  → registry: ActiveDispatching
  → 允许 Pingora 继续解析 DNS/peer
  → DNS 成功、即将交给 Pingora 建连时 lifecycle.mark_upstream_attempted()
```

commit outcome unknown 时按 operation 查询/重放；仍未知则 503、quota 全退且不
联网。

session 已 expired/stopped 时 transition 不发生，prepared intent 走 safe-zero，
返回 503 且不联网；finalize/recovery 不要求 owner 仍 live，避免 fencing 后无法
收口。

dispatching 是预算的保守“已获准尝试”状态，不等同通用 lifecycle 已实际进入
网络。移除当前 `request_filter` 过早和 `upstream_peer` 重复的 attempt 标记，只在
peer 已成功解析、即将连接时标记；这样进程内 DNS/peer 配置失败可由 collector
安全形成 `not_incurred=0`。若恰在 dispatch commit 后、DNS 前崩溃，恢复仍按
dispatching→unresolved 保守处理。

### 10.4 Finalize

在第一次 await 数据库之前，finalizer 先把已冻结的强类型 settlement command
原地写入当前 request 已持有的 registry entry：

```text
ActiveDispatching → RetryWithFact(command) → 首次 settle 尝试
                                         ├─ 成功：ack 并释放 registry permit
                                         └─ timeout/cancel/unknown：entry 保留，由 runner 查询/重放
```

entry 在 preflight 前已经预留，因此这一步不依赖另一个可能已满的队列。若无法构造
可靠 fact，则写 `NeedsUnresolved(reason)`；通知 channel 满只丢唤醒、不丢 entry，
runner 仍会周期扫描。terminal operation 固定为
`budget-settle:v1:<request_id>`；其 versioned terminal command fingerprint 在
第一次真正 settled/not_incurred 时一起占用：

```text
key FOR UPDATE
  → intent FOR UPDATE
  → terminal operation + command fingerprint 重放检查
  → pending 来源要求 pending_count>0；late-fact unresolved 来源要求 unresolved_count>0
  → calculated/estimated：checked 累加
  → not_incurred：accounted 0
  → unavailable：pending--、unresolved++、aggregate 不变且不写 terminal 字段
  → settled 才写 terminal op/fingerprint；unresolved 保留晚到 fact 修正能力
  → COMMIT
```

锁 key 后按父原状态验证并精确选择 count：正常 finalize 从 pending 结算时
pending--；尚未人工处理的 late fact 从 unresolved 结算时 unresolved--。其他状态
走 replay/`AlreadyReconciled`，不能固定只检查 pending。随后在 Rust Decimal 与
数据库上界两层执行 checked addition。下面是 pending 分支的 SQL guard；
unresolved 分支用同样 guard 改为检查/递减 `budget_unresolved_count`：

```sql
UPDATE ai_virtual_keys
   SET budget_used = budget_used + $amount,
       budget_pending_count = budget_pending_count - 1,
       budget_accounting_revision = budget_accounting_revision + 1,
       budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1,
       budget_state_updated_at = clock_timestamp()
 WHERE id = $key
   AND budget_pending_count > 0
   AND budget_used <= 9999999999999999.999999999999 - $amount
 RETURNING budget_used;
```

若预先的 checked addition 已判 overflow：aggregate 不变；通过另一条受保护
UPDATE 保留 `observed_cost_usd` 与 reason=`budget_numeric_overflow`；父原为
pending 时转 unresolved 并 pending--/unresolved++，原已 unresolved 时 count
不变。若 arithmetic 合法但
上面的 UPDATE 无返回，则是 key 缺失、pending count/revision 不一致等 corrupt
状态，必须建立 `account_issue` 并报警，不能误标 numeric overflow。不得用
`GREATEST(count-1,0)` 掩盖计数损坏。

late fact 只有在父 intent 尚未人工 resolved 时才可把 stale unresolved 自动转
settled。settle 重放按父状态分支：`settled` 必须严格匹配 settle
operation/fingerprint；`resolved` 表示人工 reconciliation 已完成，晚到自动 settle
返回 `AlreadyReconciled` 与既有 resolution，不拿 reconcile terminal fingerprint
和 `budget-settle:v1:*` 比较，也不二次计费。

### 10.5 Limit update、clear 与 delete

所有 Virtual Key PostgreSQL CUD 改走专用 `VirtualKeyGovernanceStore`，不再让
通用 DAO 写 server-owned 字段。

- 非 null limit 调高/调低：key 行锁下更新，保留 used；
- clear：key 行锁后要求 pending=0 且 unresolved=0，否则 409
  `budget_reconciliation_required`；
- delete：同样要求两个 count 都为 0；ledger 不级联删除；
- rename/rotate/enable/disable：保留 used、window 与 intent；在途 intent 保留
  创建时 snapshot；
- create：强制 used/count/revision 为 0。

混合 PATCH（例如同时改 name 与 clear limit）在一个专用事务完成，避免 generic
DAO 与 budget SQL 各成功一半。create 之外的 governance mutation 在 key lock 下推进
`budget_accounting_revision`（不增加 tail-events），让并发 verify/rebuild CAS
失效并重读；尤其不能让 clear 在锁外扫描后悄悄形成 `limit=null + rebuilt
pending`。delete 使 CAS 找不到 key，自然失败。

删除后仍允许对已 terminal request 做只读幂等 fast-path：按 request ID 读取
ledger；`settled` 按 terminal operation/fingerprint 返回 replay 或冲突，
`resolved` 返回 `AlreadyReconciled`/resolution。该分支不写也不加锁。任何
non-terminal 修改仍必须先锁 key，找不到 key 视为 corrupt。这样 COMMIT ACK
丢失后的合法重放不会因 key 已删除而退化为 404/503，也不破坏 D8 锁序。

### 10.6 Reconciliation

```http
POST /ai-virtual-keys/:id/budget-reconciliations
```

请求：

```json
{
  "intent_id": "uuid",
  "operation_id": "uuid",
  "cost_usd_decimal": "0.123000000000",
  "waive": false,
  "reason": "provider invoice reviewed"
}
```

`cost_usd_decimal` 与 `waive=true` 二选一。reason 经 trim/NFC 后 UTF-8 长度必须
为 1..=1024 bytes；`resolution_actor` 由服务端 operation context 生成，绝不接受
请求覆盖。
Manager 每次操作生成稳定 operation ID，网络重试复用。

事务用当前 Admin workspace、URL 中的 `virtual_key_id` 与 `intent_id` 联合匹配后
按 key→parent intent 加锁，不能只凭全局 intent UUID 给另一个 key/租户入账。
ledger snapshot 中的 null workspace 按 D12 归一为 default workspace；跨 workspace
UUID 统一按现有安全策略返回 404/forbidden，不泄露存在性。仅当父 intent 仍为
pending 且事务内用 DB clock 重验 owner session 尚活时返回 409
`budget_intent_active`；unresolved 已是
可人工处理状态，必须允许立即 reconciliation。

settle/waive 在同一个 key-first 事务中只递增一次 revision：

```text
settle:
  checked budget_used += cost
  + 插入 reconciliation/settled 子账
waive:
  aggregate 不变
  + 插入 reconciliation/waived、accounted=0 子账
两者共同:
  父 request → resolved + resolution_entry_id
  父写 budget-reconcile terminal operation/canonical fingerprint
  按父原状态精确 pending-- 或 unresolved--
  父、子、key 写相同 last_account_revision
```

金额累加使用与 §10.4 相同的 Rust/SQL 双重 overflow guard。合法 cost 但累加
overflow 时不写 reconciliation 子账、不改变 aggregate：父若为 pending 则原子
转 unresolved（pending--/unresolved++），父若已 unresolved 则保持 count，并在父
上保存 `observed_cost_usd` 与 `budget_numeric_overflow`；同事务还插入不参与
aggregate/count 的 `reconciliation_attempt/resolved`，保存 operation ID、
canonical command fingerprint、父 ID、输入金额、operator reason/actor 与
`numeric_overflow` outcome。它与父变更使用同 revision，tail-events 按两条 row
计数。COMMIT ACK 丢失后，同 operation/同 fingerprint 返回原 overflow，不同
fingerprint 409；管理员改金额或改 waive 必须生成新 operation。

只有 arithmetic 合法却发现既有 aggregate/count/revision 损坏时才另建
`account_issue`，不能拿 issue 掩盖这笔合法 cost。成功的 reconciliation 子账是
rebuild SUM 的唯一新增成本来源，父行不会重复计费。

## 11. Lease、重试、stale 与重建

### 11.1 `BudgetRecoveryRunner`

一个 Pingora background service 负责：

1. 每 15~30 秒只更新当前进程的 owner-session heartbeat 行，owner lease 至少为
   heartbeat 三倍；
2. 扫描当前进程的 `ActiveBudgetIntentRegistry`：`RetryWithFact` 查询/重放
   settlement，`NeedsSafeZero` 结算 not_incurred，`NeedsUnresolved` 在 key-first
   短事务中转 unresolved；guard 已丢失的 `Preparing` 先查询 create outcome，
   guard 已丢失的 active/dispatch-pending 状态按 D9 转 safe-zero/unresolved；
3. 反查数据库中属于**当前 session** 的 pending：只有 guard 仍存活的
   `Preparing/Active*` 才保持不动；recovery state 执行前一步，不在注册表中的
   orphan 立即转 unresolved 并高优先级告警；
4. 查找 `stale_not_before <= now` **且** owner session 已 stopped/expired 的其他
   候选，再按 key 分组、逐 key 使用 key-first 锁序，并在持有 key→intent 锁后
   用 DB clock 重验 owner 仍 dead 才处理；
5. 失联 owner 的 `prepared` intent 自动 settled/not_incurred/0；
6. 失联 owner 的 `dispatching` intent 转 unresolved；
7. 其他仍存活 owner 的超时 intent 只告警，不自动改变状态；
8. 对超过 soft tail-event 阈值的 key 低优先级推进 checkpoint；hard 阈值前失败持续告警，
   达到 hard 阈值后该账户停止新 preflight；
9. 暴露 registry、heartbeat lag、stale、retry exhausted、checkpoint tail 指标。

候选扫描不能先锁 ledger 再锁 key。多节点 scanner 使用条件更新和幂等 operation
竞争；pending→unresolved 依靠行锁内状态条件保证幂等，不写
`terminal_operation_id`。失败或重复不会产生双结算。

heartbeat 写入成本约为 `gateway_processes / heartbeat_interval`，与活跃 intent
数量无关；这项成本、scanner 和 retry 流量仍计入总容量模型。已 stopped 或 expired
的 session 仅在结束/过期至少 7 天、且不存在引用它的 pending ledger 后按批清理；
terminal ledger 可保留已删除的 session UUID，因此不建 owner-session 外键。

registry 与通知 queue 都有界，只保存强类型金额/ID/snapshot，不保存 payload。
permit 在 preflight 前取得，取得失败直接返回 budget 503、quota 全额退款且不建
intent；entry 的状态转换不再申请容量，所以 queue 满或 finalizer 被取消都不会丢
job。budget DB 并发许可分 admission 与 recovery/finalize 两档，后者保留最低份额，
Admin/准入洪峰不得饿死 heartbeat 和 terminal settlement。

### 11.2 Check/rebuild

```http
GET  /ai-virtual-keys/:id/budget-ledger
POST /ai-virtual-keys/:id/budget-ledger/rebuild
```

普通 GET 支持 status、时间和稳定 cursor，只返回：

- ledger page；
- stored used/pending/unresolved；
- state、capability 与当前 `budget_accounting_revision`。

它不做全生命周期 SUM，Manager 翻页不会随永久账本增长而读放大。精确校验必须显式
传 `verify=true`，并受 Admin 权限、并发、statement timeout 与速率限制保护。
GET/verify/rebuild 都把当前 Admin workspace 纳入查询与授权；key 已删除时只能依赖
ledger 自带的归一化 workspace snapshot，不能因失去 `ai_virtual_keys` 行而退化为
跨租户 UUID 查询。

rebuild 请求包含 `operation_id`、`dry_run` 与 1..=1024-byte reason，采用
checkpoint + 版本水位两阶段算法：

1. primary 读取 key 的 stored aggregate/count、
   `budget_accounting_revision=R`，以及 `checkpoint_revision=C <= R` 的最新有效
   checkpoint/base amount；checkpoint 缺失或不合法立即 fail closed，不从零猜；
2. 在 key 锁外用一个一致性 SQL snapshot 聚合 revision tail，并通过 open partial
   index 统计当前未决行：

   ```sql
   used = COALESCE(
     checkpoint.accounted_cost_usd
     + SUM(accounted_cost_usd) FILTER (
         WHERE status = 'settled'
           AND last_account_revision > C
           AND last_account_revision <= R),
     checkpoint.accounted_cost_usd)
   pending = COUNT(*) FILTER (
     WHERE status = 'pending' AND last_account_revision <= R)
   unresolved_requests =
     COUNT(*) FILTER (
       WHERE kind = 'request' AND status = 'unresolved'
         AND last_account_revision <= R)
   open_account_issues =
     COUNT(*) FILTER (
       WHERE kind = 'account_issue' AND status = 'unresolved'
         AND last_account_revision <= R)
   ```

   verify 展示
   `unresolved = unresolved_requests + open_account_issues`，每个 account issue 只
   计一次；
   `reconciliation_attempt/rebuild_audit` 不参与任何 aggregate；
3. verify/dry-run 返回 snapshot revision、stored/recomputed/difference，并短暂重读
   revision 标记结果是否仍 current，不修改数据；
4. 真正 rebuild 开启短事务锁 key，仅当当前 revision 仍等于 R 且扫描未发现无法
   修复的 ledger invariant 时，才更新 aggregate/count、递增 revision，写同
   revision 的 `rebuild_audit/resolved`，并把本次由完整重建修复的 open
   `account_issue` 转 resolved、指向该 audit；修复后的 unresolved count 只保留
   `unresolved_requests`，并追加覆盖新 revision/used 的 checkpoint。operation
   ID/reason/actor 可审计且幂等；checkpoint 提交时 tail-event counter 归零；
5. revision 已变化则释放锁并从第 1 步有界重试；热点 key 超过重试次数返回 409
   `budget_account_busy`，由操作员稍后重试或在维护窗口执行，绝不持 key 锁重扫；
6. 聚合结果超出 NUMERIC 上界或仍有不可重建的 invariant 时保留旧 aggregate，在
   短 key-first 事务创建/复用 `account_issue/unresolved` 并报警；该 issue 保持
   fail closed，直到后续成功 rebuild 明确关闭，不能成为永久无出口状态。

所有影响 aggregate 的 ledger 状态变更都必须先推进 key revision，并把同一 revision
写入受影响 rows；因此锁外扫描若与并发写交错，最终 CAS 必然失败而不会提交混合
快照。普通 verify/rebuild 成本由 checkpoint tail + open intent 基数决定，不随全部
历史线性增长；独立离线审计才允许按权限扫描 raw/archived 全历史验证 checkpoint。
删除后的 key 历史仍可按 UUID 查询，但不能 rebuild 已不存在的 aggregate。

## 12. Admin API 与 plugin schema

### 12.1 Decimal 输入/输出

新增 `VirtualKeyMutationInput` 与 `AiVirtualKeyApiView`，所有 POST/PATCH 先经过
同一个 typed DTO/validator，再调用 governance store。PATCH 使用
`Missing | Null | Value<T>` 区分未提供与显式清空：

- create 的 `name` 必填且 trim 后非空；PATCH 提供 name 时同样校验，其他长度与
  字符规则继续复用现有通用 entity name validator；
- `rpm_limit/tpm_limit` 只接受 JSON integer 或 null，合法范围
  `1..=2^31-1`；字符串、浮点/小数、0、负数和溢出均返回对应字段的 400，PATCH
  null 表示清空；
- create/patch、JSON/form body 走同一规范化结果，不能让 generic DAO 绕过；
- `ws_id` 由服务端 Admin context 注入，首版调用方不能跨 default workspace 指定；
- server-owned 字段只允许 create/rotate 内部 command 设置。

Decimal 模式与 REQ-AI-002 `AiModel` 投影一致：

```json
{
  "budget_limit": 100.5,
  "budget_limit_decimal": "100.500000000000",
  "budget_used": 83.25,
  "budget_used_decimal": "83.250000000000"
}
```

输入规范化：

- 首选 `budget_limit_decimal` string/null；
- legacy number 仅在可无损量化为 12 位且范围合法时接受；
- 两字段同传必须 Decimal 完全相等；
- 科学计数法可解析但仍需通过精度/范围校验；
- used、hash/prefix、count/state/revision 写入全部 400；
- 兼容 number 超出安全投影时响应为 null，decimal 字段仍完整。

### 12.2 Virtual Key 派生状态

响应增加：

```json
{
  "quota_enforcement": "configured_local_partial",
  "quota_backend": "memory",
  "quota_scope": "node",
  "quota_window_seconds": 60,
  "budget_status": "warning",
  "budget_backend": "postgres",
  "budget_percentage_decimal": "82.835820895522",
  "auth_endpoint_count": 4,
  "enforced_endpoint_count": 2,
  "policy_error_count": 0,
  "pending_intent_count": 1,
  "unresolved_intent_count": 0
}
```

quota 推导：

```text
Hybrid/能力不支持                  → unsupported
rpm/tpm 都为 null                  → unconfigured
enforced=0                         → awaiting_plugin
0 < enforced < auth               → configured_local_partial
enforced>0 且 enforced=auth        → configured_local
```

budget 推导优先级：

```text
能力不支持                         → unsupported
unresolved>0                       → unresolved
limit=null, used=0                 → unconfigured
limit=null, used>0                 → paused
limit!=null, enforced=0            → awaiting_plugin
used>=limit（含 limit=0）          → exhausted
used/limit>=80%                    → warning
其他                               → active
```

比例由后端任意精度定点 formatter 计算，允许超过 100%；limit=null 时为 null，
limit=0 时固定 `"100.000000000000"`，其余才做除法。金额仍只使用 Decimal。

### 12.3 `ai-rate-limit` schema

在 `kong-config::BUNDLED_PLUGINS` 与 Admin bundled list 注册
`ai-rate-limit`。Rust native schema：

```text
limit_by: global | route | consumer | virtual_key（default consumer）
tpm_limit: null | integer[1, 2^31-1]
rpm_limit: null | integer[1, 2^31-1]
header_name: string（deprecated，runtime 不使用）
error_code: integer（仅 legacy mode）
error_message: string（仅 legacy mode）
```

共享 `validate_ai_rate_limit_config()` 被以下入口调用：

- plugin POST；
- PATCH；
- PUT/upsert；
- `/schemas/plugins/validate`；
- runtime 声明式解析。

entity checks：

- `virtual_key`：plugin-level TPM/RPM 必须均为 null；
- 其他 mode：至少一个 limit 非 null；
- limit 必须为 `1..=2^31-1` 的 JSON integer；
- 非法 enum 一律 400/runtime fail closed。

PATCH 必须先把 patch 深合并到 existing config 并应用 schema default，再对完整
配置调用 `validate_ai_rate_limit_config()`；不能对局部 patch 提前做 entity check。
`header_name` 非空时按一次配置发 deprecated warning：数据库实体用
`(plugin_id, updated_at 或 canonical config fingerprint)` 去重，声明式配置用
canonical fingerprint 去重；有界 cache 只控制“本进程每个配置版本一次”，更新
配置后可再次提示。日志和无 ID label 的计数器保持低基数，fingerprint 不作为
metric label，runtime 仍完全忽略该字段。

REQ-AI-006 后续专用表单和向导直接消费这份 schema，不重复定义规则。

### 12.4 Admin trust/workspace 边界

当前 `kong-admin` 尚无可信 end-user principal/RBAC middleware，不能假装请求里已经
存在人类身份。预算 ledger/reconcile/rebuild 路由新增服务端注入的稳定契约：

```rust
pub struct AdminBudgetOperationContext {
    pub workspace_id: Uuid,
    pub actor: AuditActor,
    pub permissions: BudgetAdminPermissions,
}
```

首版 middleware 只从 `AdminState.default_workspace_id` 与本机 node identity 构造
context，actor 固定为可审计的 `system/node:<node_id>`；请求 header/query/body
不能选择 workspace、actor 或提升权限。当前受信 Admin 网络边界授予
read/reconcile/rebuild 权限，但非默认 workspace 的预算管理 API 明确不可用并按
404/forbidden 处理。未来接入 Admin 认证/RBAC 后只替换 context producer，才允许
可信 principal 和多 workspace 操作；账本 domain/store 仍强制消费同一 context。

## 13. Kong Manager

### 13.1 Virtual Keys 列表

预算单元展示：

- `used / limit USD`，明确“生命周期累计”；
- 进度条视觉宽度 clamp 100%，文字保留真实 >100%；
- limit=0 直接 exhausted/100%，不除零；
- limit=null 且 used=0 显示未配置；
- limit=null 且 used>0 显示已暂停，并继续展示历史 used；
- warning、exhausted、unresolved、unsupported 使用徽章和文字，不只靠颜色。

quota 单元展示：

- `N RPM / M TPM` 或未配置；
- “本节点 · 首次命中起算 · 60 秒”；
- awaiting、partial、configured local、unsupported；
- `enforced/auth Endpoint` 计数；
- 跳转通用 Plugins 页面创建/编辑 `ai-rate-limit`。

### 13.2 编辑与 reconciliation

- budget input 用 string，label 为 `USD / 生命周期累计`；
- 只发送 `budget_limit_decimal`，清空发送 null，不把空串转 0；
- `budget_used` 只读；
- clear 409 后展示待处理 intent，并提供 ledger 抽屉；
- reconciliation 明确选择金额结算或 waive，reason 必填；
- operation ID 在点击提交时生成，失败重试保留；
- loading、空态、API failure、unsupported 独立展示。
- 同进程 Admin 更新在目标 snapshot generation 发布后显示已生效；首次在其他
  节点启用预算/配额时提示可能延迟至“认证缓存 TTL + read-replica lag”，不能
  宣称跨节点瞬时一致。

中英文 i18n、键盘焦点、窄屏布局和状态文字必须同步。Overview/VirtualKeys 旧的
“仅保存、不执行”文案改为准确的 local quota 与模式边界。

## 14. 运行时装配与配置

### 14.1 Server 启动

对外监听前的顺序：

1. 解析 node/deployment namespace；
2. 构造共享 `PriceCatalog`；
3. 按模式构造 `RateLimitStore` 与 `BudgetRuntime`；
4. PG runtime 先原子初始化/比对 deployment-authoritative hard-tail setting，再
   生成全新 owner session ID、同步插入 session 并完成首次 heartbeat；成功后才
   允许 `create_intent` 引用该 ID。setting 不一致或注册失败则 runtime 保持
   `AccountingUnavailable`、所有预算请求固定 503，后台重注册成功前不得建 intent；
5. 构造固定容量 `ActiveBudgetIntentRegistry` 与 `AiEnforcementRuntime`；
6. 启动 `BudgetRecoveryRunner` 和 heartbeat，确认其拥有独立的高优先级 DB permit；
7. 注入 `AiRateLimitPlugin`、`AiProxyPlugin`；
8. 注册 `AiPolicyChainObserver`；
9. traditional 模式注册 `AiUsageCollector`；
10. 成对注册 fail-closed `AiBudgetDispatchHook` 与独立
    `AiDispatchAbortCompensator`，再注册 `AiEnforcementFinalizer`；builder 若发现
    critical hook 没有 abort handler 则启动失败，预算路径禁止用 no-op compensator；
11. 所有模式启动 Memory cleanup runner；
12. AdminState 获得 capability、governance、ledger 与 coverage service；
13. 上述状态确定后才开放 readiness/accept loop。

优雅关闭先停止接收新请求，再等待活动请求与 finalizer drain；runner 把剩余 registry
entry 结算或转 unresolved，registry 为空后才写 owner `stopped_at` 并停止 heartbeat。
若 shutdown deadline 内数据库不可用，不得提前写 stopped；停止 heartbeat 后让 lease
自然过期，使其他节点按保守规则恢复。heartbeat、finalize/recovery 的连接与 permit
优先级高于 admission，更高于 Admin verify/rebuild。

### 14.2 配置边界

REQ-AI-003 只开放 Memory 和 PG 所需的有界参数；Redis endpoint/TLS/ACL/pool/fail
policy 归 REQ-AI-009。建议新增：

```text
ai_quota_memory_max_buckets
ai_quota_memory_max_records
ai_quota_memory_max_records_per_bucket
ai_quota_memory_max_live_reservations
ai_quota_memory_recovery_headroom
ai_quota_max_request_lifetime_ms
ai_quota_settlement_retry_grace_ms
ai_quota_cleanup_interval_ms
ai_quota_cleanup_scan_batch

ai_budget_max_concurrent_ops
ai_budget_pg_pool_size
ai_budget_heartbeat_pg_pool_size
ai_budget_admin_pg_pool_size
ai_budget_operation_timeout_ms
ai_budget_lock_timeout_ms
ai_budget_owner_lease_seconds
ai_budget_owner_heartbeat_ms
ai_budget_intent_stale_grace_seconds
ai_budget_active_intent_capacity
ai_budget_recovery_queue_capacity
ai_budget_recovery_reserved_ops
ai_budget_recovery_scan_batch
ai_budget_owner_session_retention_days
ai_budget_checkpoint_interval_seconds
ai_budget_checkpoint_soft_tail_events
ai_budget_checkpoint_hard_tail_events
```

配置必须有上下界、交叉校验和启动日志。
`owner_lease > 3 × owner_heartbeat`。budget 使用独立、有上限的 primary pool；
其 pool size 与数据库允许的总连接数联合校验，不能在原 pool 之外无上限扩张。
heartbeat 使用独立且固定小容量（首版 1）的 primary pool/连接，不排在
admission/finalize FIFO 后；其 acquire timeout + operation timeout + 最大重连
backoff 必须小于 `owner_lease - owner_heartbeat`，否则配置拒绝启动。heartbeat
接近安全余量仍未成功时，runtime 先停止新 budget admission 并固定 503，不能等
lease 过期后继续建立旧 session intent。

热路径 pool 中 `ai_budget_max_concurrent_ops <= pg_pool_size`，
`recovery_reserved_ops >= 1` 且小于 pool size，
`admission_max_ops <= pg_pool_size - recovery_reserved_ops`；finalize/recovery
可占预留档，admission 不可。Admin verify/rebuild 使用独立小 pool 与并发限制。
active intent capacity 必须覆盖允许的活动请求与 retry state，通知 queue 即使更小
也不能丢 registry entry。checkpoint soft < hard，hard tail-event 档位与
statement timeout
必须按 aged-table 压测结果校验。Admin 长查询、rebuild 或 admission 洪峰不能饿死
heartbeat/terminal settlement。

## 15. 响应与错误协议

共享 `AiErrorResponseBuilder` 的协议来源顺序固定为：显式
`ai-key-auth.error_format` → `AiAuthContext.client_protocol`（认证成功时）→
`AiPolicyChainSnapshot.client_protocol` → path/header 推断 → OpenAI 默认。缺失或
错误 credential 尚未形成 `AiAuthContext`，也必须能从 chain snapshot 得到
Anthropic envelope。

固定映射：

| 条件 | HTTP | OpenAI type / code | 固定 message |
|---|---:|---|---|
| Virtual Key identity missing | 401 | `invalid_request_error` / `virtual_key_required` | `A virtual key is required.` |
| RPM exceeded | 429 | `rate_limit_error` / `requests_rate_limit_exceeded` | `Virtual key request rate limit exceeded.` |
| TPM exceeded | 429 | `rate_limit_error` / `tokens_rate_limit_exceeded` | `Virtual key token rate limit exceeded.` |
| budget exhausted | 403 | `insufficient_quota` / `budget_exhausted` | `The virtual key budget has been exhausted.` |
| budget DB unavailable | 503 | `server_error` / `budget_accounting_unavailable` | `Budget accounting is temporarily unavailable.` |
| budget unresolved | 503 | `server_error` / `budget_accounting_unresolved` | `Budget accounting requires reconciliation.` |
| budget unsupported | 503 | `server_error` / `budget_accounting_unsupported` | `Budget accounting is not supported in this deployment mode.` |
| pricing unavailable | 503 | `server_error` / `budget_pricing_unavailable` | `Budget pricing is unavailable for this request.` |
| quota unavailable/timeout/overloaded | 503 | `server_error` / `quota_backend_unavailable` | `Quota enforcement is temporarily unavailable.` |
| quota corrupt | 503 | `server_error` / `quota_backend_state_invalid` | `Quota enforcement state is invalid.` |
| quota unsupported | 503 | `server_error` / `quota_backend_unsupported` | `Quota enforcement is not supported in this deployment mode.` |
| invalid policy chain | 500 | `server_error` / `ai_policy_chain_invalid` | `AI policy chain is invalid.` |

Anthropic 使用对应 `type=error` envelope。Virtual Key 模式不能被 plugin
`error_code/error_message` 覆盖。

Admin 冲突固定使用 Kong schema/error envelope：清空或删除仍有 open intent 的
key 返回 409 `budget_reconciliation_required`；尝试人工处理仍在有效 lease 内的
intent 返回 409 `budget_intent_active`；同一 operation ID 携带不同载荷返回 409
`idempotency_conflict`。

## 16. 可观测性、容量与高并发基线

### 16.1 低基数指标

```text
kong_ai_quota_admit_total{backend,result,reason}
kong_ai_quota_settle_total{backend,result}
kong_ai_quota_store_operation_seconds{backend,operation}
kong_ai_quota_store_errors_total{backend,operation,kind}
kong_ai_quota_active_buckets{backend}
kong_ai_quota_active_windows{backend}
kong_ai_quota_idempotency_records{backend}
kong_ai_quota_capacity_rejected_total{backend}
kong_ai_quota_bucket_capacity_rejected_total{backend}
kong_ai_quota_live_reservations{backend}

kong_ai_budget_operation_total{backend,operation,result}
kong_ai_budget_operation_seconds{backend,operation}
kong_ai_budget_db_acquire_seconds{operation}
kong_ai_budget_lock_wait_seconds{operation}
kong_ai_budget_pending_intents
kong_ai_budget_unresolved_intents
kong_ai_budget_owner_heartbeat_lag_seconds
kong_ai_budget_recovery_queue_depth
kong_ai_budget_active_registry_entries{state}
kong_ai_budget_active_registry_rejected_total
kong_ai_budget_checkpoint_total{result}
kong_ai_budget_checkpoint_tail_events
kong_ai_budget_reconciliation_total{action,result}
```

禁止以 key/request/route/intent ID 作 label。结构化日志只输出 backend、operation、
error kind、key prefix 或不可逆短 hash，不输出原始 key/hash/payload。

### 16.2 每请求成本

| 请求类型 | quota Store | PG primary |
|---|---:|---:|
| 非 Virtual Key policy | 0 | 0 |
| Virtual Key，无 budget | admit + settle | 0 |
| budget 在初检已拒绝 | inspect | 1 read/短事务 |
| budget 正常请求 | admit + settle | inspect + preflight + dispatch + finalize |

预算路径约为 4 次 primary 操作，其中 preflight/dispatch/finalize 是写事务；
稳定态 `inspect read QPS ≈ budget QPS`、`write tx/s ≈ 3 × budget QPS`，另加
commit-unknown retry、recovery/reconciliation 和
`gateway_processes / heartbeat_interval` heartbeat writes。dispatch 只锁 intent，
preflight/finalize 才竞争热点 key。该成本是安全区分 prepared/dispatching 和
强一致审计的选择，不能隐藏在“异步日志”名义下。

### 16.3 压测档位

本需求的交付门禁不是未经测量的产品 SLA，而是必须记录以下矩阵：

- quota：1000 QPS 持续一个完整 retention、10k 并发、100k key；
- budget：100/500/1000 QPS；
- key 分布：10/100/10k key，uniform 与单热点 key；
- node：1/4/16（Memory 只验证各节点独立；不得汇总成全局额度）；
- 短响应与长 SSE；
- PG pool 默认 10 与调优档位；
- DB timeout、lock timeout、commit ACK 丢失、registry/通知 queue 满；
- 采集 p50/p95/p99、吞吐、pool wait、lock wait、内存、cleanup、deadlock、
  retry、stale/unresolved。

容量公式：

```text
quota allowed/live records ≈ allowed_qps × request_lifetime
quota replay/rejected records ≈ (allowed_qps + rejected_qps) × retention
quota memory ≈ bucket_count × bucket_bytes
             + live_records × live_record_bytes
             + replay_records × replay_record_bytes
             + configured recovery headroom
budget inspect reads/s ≈ budget_qps
budget write tx/s ≈ budget_qps × 3 + retry + recovery + reconciliation
budget heartbeat writes/s ≈ gateway_processes / heartbeat_interval_seconds
budget connection demand ≈ operation_qps × p95_transaction_seconds × headroom
                            （headroom >= 1.5，另保留 recovery permits）
registry memory ceiling ≈ active_intent_capacity × measured_worst_entry_bytes
notification queue memory ≈ queue_capacity × measured_id_entry_bytes
ledger rows/day ≈ budget_qps × 86400 + reconciliation/account_issue/audit rows
```

registry entry 只允许有界 ID、Decimal、枚举 reason 与最大 4 KiB pricing snapshot；
编码时测量 inline/heap worst-case bytes，并对可变字段设硬上限，不能让“固定 entry
数量”掩盖单 entry 无界增长。通知 queue 只保存 entry ID，不复制完整 fact。

仅 request intent 的理论增长：

| budget QPS | request ledger rows/day |
|---:|---:|
| 100 | 8,640,000 |
| 500 | 43,200,000 |
| 1000 | 86,400,000 |

即使 100 QPS 也约为 3,153,600,000 request rows/year。

压测必须实测单 row、各索引与 WAL 的 bytes/row，给出 bytes/day、autovacuum/
checkpoint 与备份影响。以上 100/500/1000 是测试档位，不是支持声明；发布时必须
随硬件、PG 参数、pool/timeout、key 分布和 retention/表大小一起公布实测的可持续
QPS、最大 key 基数与数据库增长档位。

go/no-go 门禁：

- 30 分钟 warm-up 后稳态 2 小时；高基数场景覆盖完整 retention 与至少 2 次 cleanup，
  release candidate 另做 24 小时 soak；
- 零重复/丢失 settlement、零部分扣减、零 deadlock/负 count/静默 fail-open；
- retention 后内存回落到配置上界内，registry/queue/pool 全程有界；
- 10k 长 SSE 与配置化 registry-capacity 档位记录 entry bytes、queue bytes 与 RSS，
  取消/重试后回落，不以默认 `max_live_reservations` 猜内存；
- heartbeat 与 recovery/finalize 不被 admission/Admin 饿死；
- overload 只返回受控 503，且无权威 snapshot 时不伪造 quota header；
- 除故障注入外无 unresolved；注入产生的 unresolved 都能由 late fact 或人工流程
  恢复；
- 记录 p95/p99，但基线完成前不承诺固定延迟 SLA。

除 fresh 24h soak 外，必须预灌至计划发布档位的 30/90/365-day 等效 raw row/index
规模，验证 checkpoint tail rebuild、深分页、vacuum 与写放大；未实测的年龄/规模
不列为支持档位。raw ledger 首版因审计保留，但 aggregate rebuild 只依赖 checkpoint
与 bounded tail。若实测预算吞吐或保留量超过单 PG 档位，使用相同 `BudgetStore`
契约接入可分片的强一致事务账本；不能把 ledger 简单搬到 ES 后继续宣称具备原子
预算。

## 17. 测试与验收映射

### 17.1 需求追踪

| 需求 | 主要设计落点 |
|---|---|
| FR-1 激活与身份隔离 | §4、§7、§12.2 |
| FR-2 配置与校验 | §5、§6、§12.1、§12.3 |
| FR-3 窗口与计数 | §5~§7 |
| FR-4 响应与错误 | §7.4、§15 |
| FR-5 预算生命周期与准入 | §7、§8、§10 |
| FR-6 成本结算口径 | D6、D7、§10.4 |
| FR-7 独立可恢复账本 | §8~§11 |
| FR-8 精度与 Admin | D10、§9、§12 |
| FR-9 运行模式 | D11、§14 |
| FR-10 Manager | §12.2、§13 |
| FR-11 文档与可观测性 | D14、§16、§18、§19 |

16 条验收标准的证据位置：

| AC | 主要设计/测试落点 |
|---:|---|
| 1 | §5、§6、§17.2、§17.5 |
| 2 | §6.2、§6.4、§12.1、§17.2、§17.3 |
| 3 | §4、§12.3、§17.5 |
| 4 | §7.4、§15、§17.5 |
| 5 | §6.3、§7.2、§17.2、§17.5 |
| 6 | D6、D7、§10.4、§17.4 |
| 7 | §10.5、§12.2、§13、§17.6 |
| 8 | §9~§11、§17.4、§17.6 |
| 9 | D6、§10.4、§17.5 |
| 10 | D10、§9、§12.1、§17.4、§17.6 |
| 11 | D11、§14、§17.5 |
| 12 | §12.2、§13、§17.6 |
| 13 | D14、D15、§18、§19 |
| 14 | §7、§10、§11、§17.5 |
| 15 | §11.2、§15、§17.5、§17.6 |
| 16 | §5、§6、§16.3、§17.2、§17.3 |

### 17.2 Store contract suite

同一 suite 运行于 Memory 和 deterministic remote fake：

- RPM only、TPM only、联合限制与任一维失败无部分扣减；
- exact limit、高并发同 key、不同 key 隔离；
- 同 key 跨 Endpoint；
- allowed/rejected admit replay；
- settle full refund、TPM-only refund、actual 上/下调；
- settle replay、载荷冲突、跨窗口 stale no-op；
- limit 调高/调低/null/恢复与 spec 下一 generation；
- admit/settle outcome unknown 后 inspect/重放；
- checked overflow、prospective inspect；
- retention 覆盖迟到 settlement。

### 17.3 Memory 并发/cleanup

- cleanup 与首次 acquire 并发无 detached-cell lost update；
- active operation 不可删除；
- window 过期但 reservation live 时保留 tombstone；
- 完整 retention + 两轮 cleanup 的高基数 soak 后内存回落；
- per-key cap 只让攻击 key 的新 ID Overloaded，不影响其他 key；该 key 的既有
  replay/settle 仍成功且 overload 不新增 record；
- live/replay/rejected/recovery 各容量区互不挤占，global cap 满受控 503；
- RPM/TPM 同为 null 时 `RateLimitStore` 零调用但 budget 正常执行；
- 不随机驱逐活动 key。

### 17.4 Migration/PG

- 合法边界、null、0、12 位转换；
- NaN/Infinity/负数/超范围/超精度、quota<=0 fail-fast；
- opening balance 与 deterministic operation；
- new/migrated key genesis checkpoint，open intent 跨 checkpoint 后结算仍只计一次；
- schema CHECK、reset 顺序；
- 同 key 100 并发 preflight/finalize，无丢失、负 count 或重复 request row；
- 不同 key 并行；
- clear/preflight、delete/finalize、reconcile/late fact 两种锁先后无死锁；
- 每个 COMMIT 后断连接再重放；
- terminal settlement 同 operation/同 fingerprint replay，不同 fingerprint
  conflict；unresolved 晚到 fact 可首次占用 terminal 字段，已 reconciled 父的
  晚到 settle 返回 `AlreadyReconciled`；
- owner register/首次 heartbeat 发生在 readiness 前，关闭先 drain 再 stopped；
- admission/Admin/finalize 饱和时独占 heartbeat 仍在安全余量内续租；续租失败则
  在 lease 前停止新 admission；
- expired/stopped session 的迟到 heartbeat 不能复活；heartbeat、scanner、
  reconciliation 与 dispatch 竞态中只有未 fenced owner 能继续 dispatch；
- 同库两个 deployment namespace 的 owner/settings/scanner 互不注册、续租或恢复；
- 长 SSE 依赖 owner-session heartbeat 不被误判 stale；stopped/expired + stale 双
  条件下 prepared 自动 0、dispatching unresolved；
- current-session orphan、`NeedsUnresolved`、finalizer 在 COMMIT ACK 前取消均可
  收口，registry 满在 preflight 前 503 且不建 intent；
- request task 在 create commit 后、dispatch commit 中与 fact 注册前三个取消点
  均由 guard drop 收口，活 owner 下不遗留永久 pending；
- numeric overflow aggregate 不变且 unresolved；
- rebuild 锁外 revision snapshot + 短 CAS；并发热点有界失败、不长持 key 锁；
  drift 修复、rebuild audit、account issue 关闭与 overflow issue 均幂等；
- soft/hard tail-event checkpoint、aged table bounded-tail rebuild；hard cap 只拒绝
  新 preflight，既有 finalize/recovery 仍可推进；
- 多节点 hard-tail setting 不一致时 budget readiness 失败；inspect 早拒绝与
  preflight 锁内重检一致；
- reconciliation settle/waive 的 key aggregate、父/子、count/revision 同事务，金额
  overflow 不产生部分金额修改；失败 attempt 持久化 operation/fingerprint，
  ACK 丢失可 replay、不同载荷 conflict。

### 17.5 Plugin/HTTP

- 缺身份/坏凭据的 chain protocol envelope、非法链 500、非法 enum runtime fail
  closed，Store 零误调用；
- plugin POST/PATCH/PUT/schema validate/声明式配置统一校验，PATCH 深合并后校验；
  `header_name` 每个配置版本只告警一次且不参与身份；
- RPM/TPM/双维 429、Retry-After、六个 headers；
- 四种 budget 503、三种 quota 503、403 与 Anthropic/OpenAI envelope；
- 无 snapshot 的 503 不伪造 headers；
- 初次 budget 403 不扣 quota；第二次 preflight/dispatch 失败全退；
- critical dispatch hook error/timeout/panic 均不联网；独立 compensator 在
  header-filter 前全退，退款未知时无伪造 header 且 finalizer 重放同 command；
- server wiring 缺失 critical hook 对应 abort compensator 时启动失败；
- ai-proxy 解析失败保留 RPM、退 TPM；
- official/estimated/unknown total settlement；
- upstream error、client disconnect、stream interruption、跨窗口长流；
- collector/writer drop 不影响 budget/quota finalize；
- finalizer 重入和普通 log 先失败都不重复/漏结算。

### 17.6 Admin/Manager E2E

- Virtual Key POST/PATCH 的 name required/non-empty；RPM/TPM 对 null、1、
  `2^31-1`、0、负数、小数、字符串与溢出给出准确字段级结果，PATCH null 清空；
- exact decimal 与 legacy number 往返、冲突字段、只读字段；
- 极小 limit/极大 used 的任意精度百分比不 overflow，`100.5/83.25` 示例显示
  `82.835820895522`；
- limit clear 409、ledger 定位、settle/waive 幂等；
- unconfigured/paused/awaiting/partial/local/warning/exhausted/unresolved/unsupported；
- >100%、limit=0、历史 used、rotate/rename/disable 保留；
- cursor 拉取超过 10k routes/plugins，两个 workspace 与 legacy `ws_id=null`
  归一后无跨 workspace 计数；分页中途 CUD 不发布混代 topology，目标 refresh
  epoch 只由 commit 后新开的 consistent snapshot 满足；refresh 失败保留同
  generation 的 routing/coverage；
- ledger GET/verify/rebuild/reconcile 即使 key 已删除也按 workspace snapshot 授权，
  跨 workspace UUID 不泄露、不入账；
- 首版 default-workspace/system-node Admin context 不信任伪造 actor/workspace
  header/query/body；非默认 workspace 明确拒绝；
- DB-less local/restart 清零、budget unsupported；
- Hybrid 两项 unsupported；
- 双语、窄屏、键盘与错误/空态。

### 17.7 验证命令

```bash
cargo check -p kong-ai -p kong-plugin-system -p kong-proxy -p kong-admin -p kong-server
cargo test -p kong-ai --locked
cargo test -p kong-db --locked
cargo test -p kong-admin --locked
make manager-install
cd kong-manager && pnpm lint && pnpm build && pnpm test:e2e
git diff --check
```

数据库事务/migration 另跑受管 PostgreSQL 聚焦测试；真实 HTTP 覆盖 8000/8001。
大范围实现完成后再扩大到 `make check`、`make lint` 和相关 `make test*`。

## 18. 实施顺序

1. 冻结 `RateLimitStore` contract suite、Memory clock/atomic/cleanup；
2. migration 007、Decimal model/schema、opening balance；
3. `BudgetStore/PgBudgetStore` 事务、幂等、lease、rebuild 测试；
4. policy observer、dispatch hook、async finalizer 与 server 装配；
5. ai-rate-limit schema/校验、Virtual Key 专用 governance、Admin 状态/API；
6. Manager 精确金额、状态与 reconciliation；
7. 中英文 guide、`docs/design.md`、`docs/tasks.md`、implementation log；
8. 真实 PG/HTTP/browser 与容量/故障压测。

编码期间按以上顺序保持每一步可测试；不得先接 UI 再补账本正确性，也不得把
Redis/ES 的实现混入本需求。

## 19. 风险与后续边界

| 风险 | 本需求处理 | 后续 |
|---|---|---|
| 多节点 Memory 可获得 N 倍额度 | API/UI/文档明确 local | REQ-AI-009 Redis |
| usage PG 持续增长 | 与 budget 解耦、暴露增长风险 | REQ-AI-013 外部存储 |
| 单热点 key 锁串行 | 单 key 短事务、指标与压测 | 分片 `BudgetStore` adapter |
| dispatch 后崩溃不知真实成本 | unresolved + 人工 reconciliation | provider 幂等/attempt 账务 |
| completion 导致超预算 | settle 后累计，下一请求阻断 | 严格最大成本 reservation 不在本单 |
| raw ledger 长期增长 | 强一致 checkpoint + bounded tail、aged-table 容量门禁 | 覆盖 checkpoint 前缀的审计归档需独立设计 |
| Hybrid 无 key/accounting 同步 | capability unsupported | 独立 Hybrid 项目 |

本方案已作为编码基线；当前实现仍在验证收口。REQ-AI-003 的完成定义仍以
[analysis.md](analysis.md) 的 16 条验收标准全部有测试或可复核证据为准。
