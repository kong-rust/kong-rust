# Implementation Log: REQ-AI-003 — Virtual Key 配额与预算控制

**日期：** 2026-07-26

**状态：** 🧪 核心编码与主链路验证完成 / 发布档位验证中；本记录不代表需求已验收完成

**需求：** `docs/pm/REQ-AI-003/analysis.md`

**方案：** `docs/pm/REQ-AI-003/design.md`

## 当前交付结论

REQ-AI-003 已把 Virtual Key 的 RPM、TPM 和生命周期 USD 预算接入请求生命周期。
实时配额与预算账务使用两个独立的后端无关契约：

- `RateLimitStore` 承担 RPM/TPM 联合准入、修正、查询和幂等；当前 adapter 是
  本进程 `MemoryRateLimitStore`。
- `BudgetStore`、`BudgetAdminStore` 与 `BudgetAccountGovernance` 承担预算准入、
  强一致账本、治理和对账；当前权威 adapter 是 PostgreSQL。

这两个默认 adapter 是可运行基线，不是企业多副本的最终存储拓扑。Redis
分布式实时配额尚未实现，由 REQ-AI-009 交付；Elasticsearch/OpenSearch、
ClickHouse、Kafka 等外部 usage/log 存储尚未实现，由 REQ-AI-013 交付。外部
analytics 后端不参与强一致预算决策，Redis 或 ES 也不能替代预算审计账本。

## 已实现范围

### 配额 Store 与请求生命周期

- 新增异步 `RateLimitStore`，公共命令只暴露结构化 subject、`WindowSpec`、
  quota、request/operation ID 和 opaque reservation，不暴露 `DashMap`、
  `Instant` 或未来 Redis key。
- `MemoryRateLimitStore` 在单个逻辑 key 的短锁内联合检查并预扣 RPM/TPM；
  任一维超限时两项都不增加。准入和 settlement 分别按固定 ID 幂等，迟到修正
  通过 window ID/generation 变成 stale no-op，不修改新窗口。
- Memory adapter 具有 bucket、总 record、单 bucket record、live reservation
  和 recovery headroom 上限，并提供 bounded cleanup。容量耗尽返回结构化
  overloaded，不随机驱逐活动 reservation。
- `limit_by=virtual_key` 只使用认证后的稳定 Virtual Key UUID；额度来自 key
  自身的 `rpm_limit/tpm_limit`。非法 `limit_by`、缺少身份或缺少 `ai-proxy`
  的策略链 fail closed，不再静默落入 global。
- 代理层新增通用 async dispatch hook、dispatch abort compensator 和 finalizer。
  critical budget hook 在允许网络 dispatch 前提交账务状态；失败时先不可逆禁止
  上游，再补偿 quota。客户端响应确定后，finalizer 复用同一 operation ID 修正
  TPM 和结算预算。
- 当前 quota 窗口是本进程、首次命中起算的固定 60 秒窗口；多节点各自计数且
  重启清零。该状态通过 Admin/Manager 显式标记为 local/node。

### PostgreSQL 强一致预算账本

- migration 007 将预算金额迁移为 `NUMERIC(28,12)`，新增非负/范围约束、
  pending/unresolved/revision/tail 投影，以及
  `ai_budget_ledger`、`ai_budget_checkpoints`、
  `ai_budget_owner_sessions` 和 deployment runtime settings。
- migration 008 收紧 numeric overflow 未决记录的终态幂等约束。迁移为既有非零
  `budget_used` 建立 opening balance，并为既有与新账户建立 genesis checkpoint。
- 预算热路径在 provider 前创建 request intent，dispatch 前把 prepared intent
  转为 dispatching，finalize 以内部 request ID 和固定 operation ID 幂等结算。
  aggregate、pending/unresolved count、ledger 行和 revision 在同一 key-first
  事务中提交；analytics writer 成功与否不影响预算。
- 金额在 Rust、Admin API 和 PostgreSQL 中使用 Decimal/NUMERIC；Admin 的规范
  金额字段是固定 12 位字符串。`budget_used`、账务 count/revision/state、
  `key_hash/key_prefix` 均为服务端所有，普通 create/PATCH 不能覆盖。
- 已持久化 `budget_used >= budget_limit` 的下一请求在 provider 前返回 403。
  由于 completion 成本在响应后才确定，已准入请求及并发在途请求可以把 used
  推到 100% 以上；这是 settle-then-block-next，不是严格零超支 reservation。
- owner session 使用数据库时钟、heartbeat lease 和 fencing。进程内有界
  `ActiveBudgetIntentRegistry` 保留 retry fact、safe-zero 或 unresolved 恢复
  状态；guard 取消不会执行 I/O，但会把 entry 留给后台幂等恢复。
- PG adapter 已实现失联 owner 的 stale intent 恢复：prepared 自动结算
  `not_incurred=0`，dispatching 保守转 unresolved。checkpoint 命令按账户
  revision 幂等推进，聚合或约束漂移不被静默掩盖。
- verify/rebuild 使用 checkpoint + revision tail，并以短 key-lock CAS 提交；
  mismatch、numeric overflow 和不可重建 invariant 保持 fail closed。后台维护
  服务周期执行 registry recovery、失联 owner stale scan 和 soft-tail
  checkpoint，并在关闭时有界 drain。
- owner fencing 后的本地恢复不再用失效 owner 重放 `create_intent`。恢复线程通过
  owner-independent 的 `lookup_intent` 查询 request identity：未提交且从未进入
  upstream 的 create 可安全释放本地 registry；prepared 自动结算 0；
  dispatching 保守进入 unresolved；已持久化终态只做本地确认。真实 PostgreSQL
  用例覆盖 owner stop 后 lookup 与 safe-zero settlement。

### 资源隔离与背压

- Traditional PostgreSQL 启动路径分别创建有上限的 budget hot pool、
  heartbeat/owner pool 和 Admin pool。heartbeat 不与 admission、finalize 或
  长时间 Admin verify/rebuild 共用同一连接池。
- hot pool 额外受 operation semaphore 和 recovery reserved permits 约束；
  timeout、lock timeout、owner lease/heartbeat、registry 容量、recovery batch
  与 checkpoint soft/hard tail 均可配置并进行交叉校验。
- 这种隔离减少 usage/Admin 洪峰饿死预算恢复的风险，但三个 pool 当前仍可共享
  同一 PostgreSQL 实例、WAL、磁盘和 CPU，不等同于物理隔离或无限吞吐。
- 非预算 key 不进入预算数据库路径；预算 key 需要 primary inspect、intent、
  dispatch 和 settlement 事务。单个热点 key 的强一致 aggregate 会串行，是
  当前 PG adapter 必须压测和监控的容量边界。

### Admin API 与 Kong Manager

- Virtual Key create/PATCH/delete 改走预算治理事务。创建强制 zero
  aggregate/genesis checkpoint；混合 PATCH 原子更新普通字段和 budget limit；
  有 pending/unresolved 时 clear/delete 返回 409。
- Virtual Key 响应增加精确 Decimal、quota/budget capability、backend、
  node/window、coverage、pending/unresolved 和派生状态。coverage index 按
  workspace 预构建，列表投影不对每个 key 重扫所有 Route/Plugin。
- 新增账务管理端点：
  - `GET /ai-virtual-keys/{id}/budget-ledger`
  - `POST /ai-virtual-keys/{id}/budget-reconciliations`
  - `POST /ai-virtual-keys/{id}/budget-ledger/rebuild`
- reconciliation 支持带稳定 operation ID 的人工 settle 或 waive；ledger 查询
  支持状态、RFC 3339 时间范围和稳定 cursor；rebuild 默认 dry-run，显式
  `dry_run=false` 才尝试 CAS 修复并写审计行。
- Manager Virtual Keys 页面只使用服务端派生状态，显示本节点 quota、
  Endpoint coverage、精确预算进度、warning/exhausted/unresolved/unsupported，
  并提供 ledger、settle/waive 和 verify/rebuild 操作。前端金额不经过 JavaScript
  `Number` 运算。

### 运行模式边界

| 模式 | Virtual Key quota | 持久预算 |
|---|---|---|
| Traditional + PostgreSQL | 支持；本进程 Memory、60 秒、重启清零 | 支持；PostgreSQL primary + ledger |
| standalone DB-less | 支持；本进程 Memory、易失 | 不支持；命中预算策略时 fail closed |
| Hybrid CP/DP | capability 明确为 unsupported | unsupported；没有 DP→CP accounting |

DB-less 或 Hybrid 不会用内存余额伪装强一致预算。Hybrid 的 AI 实体同步本身仍未
完成，DP 可能先在认证阶段返回 401。

## 明确未实现的企业后端

- **REQ-AI-009：** Redis/Sentinel/Cluster adapter、跨节点 RPM/TPM、一致的
  reservation/settlement script、distributed/degraded 状态和切换治理。
- **REQ-AI-013：** 外部 usage/log sink 与 query backend、retention、rollover、
  bulk ACK、双写迁移和外部查询。
- **不允许的替代：** 把预算 aggregate/ledger 直接搬到 Redis 或 ES，并不能满足
  当前 key-first 原子事务、审计、reconciliation 和 rebuild 契约。未来预算扩容
  必须实现同一强一致 `BudgetStore` 语义。

## 验证矩阵（收口中）

以下矩阵用于最终回填可复核证据；在所有必需项通过前，REQ-AI-003 保持“验证中”。

| 范围 | 复核命令/证据 | 当前状态 |
|---|---|---|
| quota contract、Memory 并发/cleanup | `cargo test -p kong-ai --locked` | 🟢 全 crate 共 564 通过、0 失败、容量基准 1 项按设计 ignored |
| budget PG 事务、幂等、stale、checkpoint、rebuild | 真实 PostgreSQL budget 聚焦测试与 migration 006→007→008 | 🟢 budget 22/22；真实迁移 1/1 |
| lifecycle hook 与补偿 | `cargo test -p kong-proxy --locked` | 🟢 82 通过、0 失败 |
| schema、Admin governance/ledger/reconciliation | `cargo test -p kong-admin --locked` + 8001 CRUD/ledger/reconciliation | 🟢 77 通过、0 失败；真实 Admin API 通过 |
| 配置、插件系统与 server 装配 | `cargo test -p kong-config --locked`、`cargo test -p kong-plugin-system --locked`、`cargo check -p kong-server --locked` | 🟢 29 + 8 通过，server check 通过 |
| 工作区构建检查 | `make check` | 🟢 通过；仅有既有 dead-code warning |
| Manager 精确金额与操作流 | `pnpm lint`、`pnpm build`、Playwright AI Gateway 全文件 | 🟢 lint 0 error、build 通过、Playwright 6/6 |
| 真实 HTTP 核心路径 | 8000 的 401/403/429/500/503、headers 与 OpenAI/Anthropic envelope | 🟢 核心路径通过，详见下节 |
| 真实 HTTP backend 故障档位 | 四种 budget 503、三种 quota backend 503 的可注入真实代理矩阵 | 🟡 固定契约单元测试通过；发布档位真实 HTTP 注入矩阵待补 |
| 运行模式 | Traditional、DB-less、Hybrid capability/unsupported | 🟡 runtime/投影契约通过；DB-less/Hybrid 独立进程矩阵待补 |
| 容量与故障 | Memory quota ignored 基准、热点 key、pool 饱和、heartbeat、commit unknown、10k SSE、高基数 soak | 🟡 Memory quota 已有可重复证据，其余待发布档位验证 |
| 差异完整性 | `git diff --check`、新增 Rust 文件 `rustfmt --check` | 🟢 通过 |

### 真实 HTTP 核心链路

2026-07-26 使用运行中的 Traditional PostgreSQL 实例、8000 Proxy、8001 Admin
以及带调用计数器的本地 OpenAI-compatible mock upstream 复核。测试临时挂载
`ai-rate-limit(limit_by=virtual_key)`、创建独立 Virtual Key，并在 `finally`
中删除所有临时 key/plugin、恢复 Service、Provider、Model 价格和 `ai-proxy`
配置；结束后再次查询确认无 `req-ai003-*` 实体残留。结果：

- 缺失或无效 Bearer 均返回 OpenAI 401，mock upstream 计数为 0；
- 未知模型价格 + 已配置预算返回
  `503 / budget_pricing_unavailable`，upstream 计数为 0；
- 完整 Decimal 价格下，首个请求 200 并结算
  `budget_used_decimal=0.000020000000`，下一请求在 upstream 前返回
  `403 / budget_exhausted`，upstream 总计只收到首个请求；
- RPM 与 TPM 分别返回稳定的
  `requests_rate_limit_exceeded` / `tokens_rate_limit_exceeded` 429；六个
  `X-RateLimit-*` 头与 `Retry-After` 均存在，拒绝请求不调用 upstream；
- 暂时禁用 `ai-proxy` 后，无效策略链返回
  `500 / ai_policy_chain_invalid`，不调用 upstream；
- `client_protocol=anthropic` 时，401、预算 403 与 TPM 429 均使用
  Anthropic `type=error` envelope，包含 request ID；429 仍携带配额头和
  `Retry-After`，三项均未调用 upstream。

同一运行实例还通过浏览器复核了 Usage Summary → Logs → Summary 往返、
24h/7d 区间、token metric、model/provider 下钻、清除过滤器、日志详情以及
Virtual Key ledger verify；未再复现“从调用日志切回调用统计失败”。

### Memory quota 容量基准

可重复命令：

```bash
cargo test -p kong-ai --locked --test ratelimit_capacity_test -- --ignored --nocapture
```

2026-07-26 在本地 debug profile、8 个 Tokio worker 的一次样本通过：

- 10,000 个唯一 key 全部准入，耗时 77.466 ms、约 129,088 ops/s，
  p50/p95/p99 分别为 0.010/0.070/0.305 ms；
- 单热点 key 并发 10,000 次，在请求上限 5,000 下精确得到 5,000 次准入和
  5,000 次拒绝，无 Store 错误、无 overload、无额度突破；耗时 1,669.278 ms、
  约 5,991 ops/s，p50/p95/p99 分别为 0.301/2.204/26.246 ms；
- 终态 stats 为 10,001 buckets、20,000 idempotency records、15,000 live
  reservations；测试进程 RSS 增量粗估 49.00 MiB，按 bucket + record 计约
  1,712 bytes/entry。RSS 包含 Tokio、测试采样与 allocator 保留，只用于容量
  量级估算，不能视为 Store 单条记录的精确堆大小。

该测试默认 ignored，不拖慢常规回归。吞吐与延迟只记录机器样本，不设脆弱的
性能阈值或发布 SLA；自动断言仅覆盖容量上界内无错误、热点原子上限不突破及
Store stats 一致性。

## 剩余风险

- 当前 Memory quota 不是全局配额；N 个无粘性节点理论上可获得约 N 倍额度。
- PG budget 每个正常预算请求包含多次 primary 操作，热点 key 会在单 key 锁上
  串行；未完成压测前不承诺固定 QPS 或 p95/p99。
- budget、usage 和配置虽然使用不同 pool/写入路径，但当前可共享同一 PG 物理
  资源。usage 高增长仍需 REQ-AI-013 外置和 retention 治理。
- raw ledger 仍持续增长。checkpoint 限制 verify/rebuild 的 tail 工作量，但
  不等于明细归档或删除策略。
- provider dispatch 后进程崩溃无法证明真实成本；此类 intent 保守 unresolved，
  需要晚到 fact 或人工 reconciliation。
