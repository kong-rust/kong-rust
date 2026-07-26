# REQ-AI-003 需求分析 — Virtual Key 配额与预算控制

> Quota & Budget Enforcement — Requirement Analysis
>
> - **优先级 / Effort：** P0 / L（分析后由 M 调整；涉及请求热路径、独立预算账本、
>   PostgreSQL 迁移、Admin 校验与 Manager 闭环）
> - **需求分析定稿：** 2026-07-26
> - **依赖：** REQ-AI-001（Virtual Key 身份，已完成）、REQ-AI-002（token、价格
>   与成本口径，已完成）
> - **需求单索引：** [../backlog.md](../backlog.md)

## 背景与价值

`ai_virtual_keys` 已保存 `tpm_limit`、`rpm_limit`、`budget_limit` 和
`budget_used`，但当前请求链完全不读取或更新这些字段。现有
`ai-rate-limit` 也存在直接影响隔离性的缺陷：

- `limit_by=virtual_key` 没有实现，会与任意非法值一起静默降级到 `global`；
- 限额只来自插件配置，无法按已认证 Virtual Key 使用不同额度；
- TPM/RPM 是两个独立的单进程固定窗口计数，响应没有剩余额度或重置时间；
- 超限错误是扁平 `{message, current, limit}`，不是 AI 客户端可识别的错误契约；
- `budget_used` 可被 Admin API 调用方直接创建或 PATCH，且金额仍使用
  `DOUBLE PRECISION/f64`；
- Manager 只显示静态文本，并继续提示配额与预算尚未执行。

REQ-AI-001 已把稳定的 `virtual_key_id` 注入 `AiAuthContext`；REQ-AI-002 已在请求
finalize 阶段同步形成带 Decimal 成本的请求事实，再由 best-effort writer 异步
保存 analytics。本需求在这两个基础上交付 Virtual Key 治理闭环：按 key 隔离的
本地 TPM/RPM、持久预算截止线、明确的失败语义，以及可操作的 Manager 状态。

本地窗口与 PostgreSQL 预算是首个可运行的默认部署档位，不是企业规模的唯一
架构。当前需求必须同时冻结后端无关的实时配额与预算账务契约，使 REQ-AI-009
可以增加 Redis 多节点配额，而无需重写插件生命周期；海量 usage/log 的外部存储
由 REQ-AI-013 交付，且始终与强一致预算账本解耦。

本需求的限流概念参考 Kong AI Rate Limiting Advanced，但不是它的 schema 或
wire-level 兼容实现。Kong 官方 AI 插件使用 `identifier`、多窗口与
`tokens_count_strategy`，官方 Virtual Key 持久总账也没有直接对应物：

- [AI Rate Limiting Advanced](https://developer.konghq.com/plugins/ai-rate-limiting-advanced/)
- [AI Rate Limiting Advanced 配置参考](https://developer.konghq.com/plugins/ai-rate-limiting-advanced/reference/)
- [Kong Entitlement enforcement 限制](https://developer.konghq.com/metering-and-billing/entitlements/#entitlement-enforcement)

因此 `limit_by=virtual_key`、生命周期 USD 预算、403、协议化错误体都是
Kong-Rust 产品扩展；不得宣传为 Kong AI Rate Limiting Advanced 完整兼容。

## 目标与成功定义

1. 在启用 `ai-key-auth` 与 `ai-rate-limit(limit_by=virtual_key)` 的 AI Endpoint
   上，以已认证 Virtual Key ID 为唯一隔离键，执行 key 自身配置的 TPM/RPM。
2. 以 PostgreSQL 中的 Decimal、幂等预算账本为权威，持续累加已发生的标准 USD
   成本；analytics writer 成功与否不得影响预算执行。
3. 已持久化 `budget_used >= budget_limit` 时，在调用 provider 前拒绝后续请求；
   提高预算后立即恢复，不需要重启或等待认证缓存。
4. 429、403 与预算后端异常具有固定的机器可读错误和配额响应头，客户端无需解析
   自由文本。
5. Admin API 不能篡改 `budget_used`，非法限额和非法 `limit_by` 在保存时被拒绝；
   DB-less/Hybrid 的能力边界显式 fail closed 或标记不支持。
6. Manager 能准确区分未配置、待挂载、节点本地生效、预算预警、已耗尽与不支持，
   不再把静态字段误报为全局生效。
7. 插件和领域服务只依赖可注入的异步配额 Store 与预算账务 Store；首版
   Memory/PostgreSQL 后端不能把本机容器、时钟或 SQL 私有语义泄漏到公共契约。

## 用户故事

- 作为平台管理员，我可以给不同 Virtual Key 设置不同 RPM/TPM，同一分钟内一个
  key 超限不会影响另一个 key。
- 作为预算负责人，我可以设置一个以 USD 表示的累计预算，查看已用比例；已耗尽
  key 的后续调用在到达 provider 前被拒绝。
- 作为运维人员，我可以通过响应头判断请求和 token 的剩余额度及多久恢复，并用
  机器可读错误区分 RPM、TPM、预算耗尽和预算账务不可用。
- 作为管理员，我可以调高预算让已耗尽 key 立即恢复，同时保留既有累计用量；
  rename、rotate、disable/enable 不会暗中清零。
- 作为 DB-less/Hybrid 用户，我会看到明确的不支持状态，而不是把易失或未同步的
  `budget_used` 当作可靠硬预算。

## 术语与执行口径

| 术语 | 本需求定义 |
|------|------------|
| Virtual Key 配额策略 | 同一有效插件链同时包含 `ai-key-auth`、`ai-proxy` 和启用的 `ai-rate-limit`，且后者 `limit_by=virtual_key` |
| RPM | 当前 60 秒本地固定窗口中准入的请求数；限额来自已认证 key 的 `rpm_limit` |
| TPM | 当前 60 秒本地固定窗口中请求消耗的 prompt + completion 总 token；准入时预扣 prompt 估值，结束后按 REQ-AI-002 的标准化 usage 修正 |
| 未配置限额 | 对应字段为 `null`；该维度不计数、不返回对应响应头 |
| 生命周期预算 | key 在预算启用期间累计的标准 USD 成本，没有自动日/月重置周期 |
| 预算启用 | key 的 `budget_limit` 非 `null`，且请求命中 Virtual Key 配额策略 |
| 预算耗尽 | 权威存储中的 `budget_used >= budget_limit` |
| 预算结算 | 对一次已发生调用按网关内部 request ID 幂等记账，并原子增加 `budget_used`；fact ID 只作可空关联 |
| 持久消费截止线 | 预算依据已持久化成本拒绝后续请求；不是对尚未知晓的 completion 成本做严格预留 |
| 本地窗口 | 计数仅存在当前 Kong-Rust 进程；多节点不共享，重启清零 |
| 生效状态 | Manager 对“是否存在启用的 Virtual Key 配额插件”和当前运行模式的可验证描述，不表示所有 Endpoint 自动挂载 |

### 预算超支边界

LLM 的实际 completion token 和成本通常要到响应后才能确定。本需求选择与现有
Kong AI 限流限制相近的 **settle-then-block-next** 语义：

1. 请求准入时以主库当前 `budget_used` 判断是否已经耗尽；
2. 已准入请求完成后结算实际或可安全估算的成本；
3. 单个请求或多个并发在途请求可以在结算时把 `budget_used` 推到
   `budget_limit` 之上；下一次请求开始拒绝；
4. UI 和文档必须允许显示超过 100%，不得宣称“严格零超支”或供应商账单级硬余额。

严格的最大成本预留要求每种 provider 都有可信的 output 上限、价格与失败恢复
协议，会显著改变请求契约，不在本需求范围。这里的“超预算拒绝”专指**准入时已经
耗尽**的请求在到达 provider 前被拒绝。

## 现状事实与约束（2026-07-26 基于代码核实）

- `ai-key-auth` priority 774，早于 `ai-rate-limit` 771；认证成功会注入
  `AiAuthContext`，但上下文目前只有 key ID/name/prefix/consumer，不含配额快照。
- `VirtualKeyAuthenticator` 返回完整 key 并有 1 秒缓存；Admin CUD/rotate 会清
  缓存。静态 TPM/RPM 可以由认证结果传递，但预算准入不能信任缓存中的
  `budget_used`，必须查询主库权威状态。
- `AiRateLimitConfig.limit_by` 是任意字符串。运行时只显式处理
  `global/route/consumer`，其余全部落到 `global`；`header_name` 从未被读取。
- 现有限流器是按 counter 第一次命中起算的 60 秒固定窗口：
  `DashMap<String, {Instant, AtomicU64}>` 只保证当前进程内单 counter 的 CAS 原子
  递增，不是自然分钟、滑动窗口或分布式窗口。
- 当前先扣 RPM，再预扣 TPM；TPM 失败不会回滚 RPM。log 修正缺少 window ID，
  长请求跨窗口时可能错误增减新窗口；provider 缺 `total_tokens` 时还会把预扣
  错误退成 0。
- `AiVirtualKey` 使用 `Option<i32>` 配额和 `f64` 预算；数据库为
  `INTEGER/DOUBLE PRECISION` 且没有非负约束，通用 DAO 不支持幂等原子增量。
- Virtual Key create 接受调用方提供的 `budget_used`，PATCH 也能覆盖它；
  `key_hash/key_prefix` 同样没有限制在 rotate 专用路径。
- `ai-rate-limit` 虽在 server runtime 注册，但不在
  `kong_config::BUNDLED_PLUGINS` 中；Admin 创建插件会拒绝。它的 Rust native
  schema 是空字段，schema validate/create/update/upsert 没有共享校验。
- REQ-AI-002 collector 在插件 log 阶段前同步生成 `Arc<AiUsageFact>`，其中包含
  Decimal 成本和成本状态；异步 writer 只负责 analytics 持久化。预算可以复用
  同一成本计算结果，但必须有独立事务与失败恢复路径。
- Endpoint 发布向导目前只自动挂载 `ai-key-auth`，不挂载 `ai-rate-limit`。
  完整的策略插件向导仍属于 REQ-AI-006。
- DB-less DAO 禁止写，无法保证预算跨重启；Hybrid 目前也不向 DP 同步 AI
  Virtual Key 实体或回传预算扣费。

## 功能需求

### FR-1 激活条件与身份隔离

1. `limit_by` 严格支持 `global`、`route`、`consumer`、`virtual_key` 四个值。
   任何其他值在 Admin create、PATCH、PUT/upsert 与
   `/schemas/plugins/validate` 均返回 400；绕过 Admin 的声明式配置在 runtime
   解析时也必须失败，绝不回退 `global`。
2. `limit_by=virtual_key` 只使用 `AiAuthContext.virtual_key_id` 生成计数键，不读
   原始 Authorization、`x-api-key` 或自定义 header，不以 key name/prefix/hash
   作为身份。
3. 缺少 `AiAuthContext` 时 fail closed：返回 401 和
   `virtual_key_required`。不得把未认证请求合并到 global 或空字符串桶。
4. Virtual Key 自身的 `rpm_limit/tpm_limit` 是唯一权威值；插件配置中的同名限额
   不作为 fallback。key 字段为 `null` 时该维度无限制。
5. rename 和 rotate 保持 key ID 不变，因此不得重置配额窗口或预算；删除后重新
   创建同名 key 是新 ID、新隔离桶。
6. 本需求不新增 `service/ip/header/path/credential/consumer-group` 等维度，也不
   改写已有 `global/route/consumer` 的产品定位。`consumer` 缺身份时合并到旧
   `consumer:` 空桶是明确保留的遗留风险，不得把它宣传为可靠匿名隔离。
7. `limit_by=virtual_key` 的有效链必须同时包含 `ai-proxy`。缺少它时返回 500
   `ai_policy_chain_invalid`，且不创建预算 intent、不消耗 RPM/TPM；不能让普通
   HTTP Route 进入一个永远无法形成 AI 成本的 accounting 流程。
8. 在同一 deployment/cluster namespace 内，Virtual Key bucket 只按稳定 key ID
   归属；同一 key 命中多个已挂载 Virtual Key 配额策略的 Endpoint 时共享同一
   RPM/TPM 窗口。plugin、Route、Service 或 Endpoint ID 不得给该 bucket 加盐，
   避免挂载 N 个 Endpoint 获得 N 倍额度。`global/route/consumer` 使用各自既有的
   policy namespace，不套用这条 Virtual Key 规则。

### FR-2 配额配置与校验

1. Virtual Key `rpm_limit/tpm_limit` 接受 `null` 或 `1..=2^31-1` 的整数；
   `0`、负数、小数、字符串伪整数和溢出值返回字段级 400。清空限制必须显式传
   `null`。
2. `limit_by=virtual_key` 不要求插件层再填写 TPM/RPM；若传入，仅作为无效组合
   在新建/修改时拒绝，避免管理员误以为存在 fallback 或全局上限。
3. `global/route/consumer` 保留插件层限额语义，并要求至少配置 TPM 或 RPM
   之一；非法数值在 Admin 和 runtime 使用同一规则拒绝。
4. `ai-rate-limit` 登记为 Kong-Rust bundled 插件，提供满足本需求字段、默认值、
   类型、枚举和 entity check 的 Rust native schema。专用 Manager 配置体验、
   Prompt Guard/Cache schema 与发布向导仍由 REQ-AI-006 交付。
5. 同进程 Admin 写成功后，下一请求必须看到新的静态配额；实现不能在清缓存后又
   从可能延迟的 read replica 读回旧值，应采用 write-through 或等价主库路径。
   带外写、其他节点和 read replica 只保证最终一致，延迟至少包含现有 1 秒 TTL
   与复制延迟；不得声称跨节点瞬时一致。
6. 旧 `header_name` 字段保持可读取/保存以兼容已有配置，但标记 deprecated 且在
   runtime 无效，并产生一次配置告警；Virtual Key 身份始终来自 `ai-key-auth`。
   后续大版本才可移除，不能在本需求中突然拒绝旧配置。
7. 活动窗口内调高/调低 TPM/RPM 不新建窗口：立即以新 limit 对现有 count 判定；
   设为 `null` 时暂停该维度，原 count 保留到原窗口过期；同一窗口内恢复后继续
   使用原 count，过期后才从 0 开始。

### FR-3 RPM/TPM 窗口与计数

1. 首版继续使用按隔离键首次命中起算的 60 秒固定窗口。policy 层向 Store 传入
   backend-neutral `WindowSpec { algorithm=fixed_first_hit, duration=60s }`；
   backend 负责用权威时钟实现 start/reset、opaque window ID/generation 和 TTL，
   不能自行改变算法或时长。窗口长度配置、自然分钟对齐、sliding、多窗口、Redis
   与跨节点共享属于 REQ-AI-009；duration/algorithm 变化只能从下一 generation
   生效，不能重置活动窗口。本需求必须把现有同步、单计数器 `RateLimiter` 重构为
   可注入的异步 `RateLimitStore`，Memory 只是首个 backend，插件不得直接持有或
   操作 `DashMap`。
2. 对同一个 Virtual Key，RPM 与 TPM 必须在一次准入判定中避免部分扣减：
   任一维度在预扣后超限时本次请求 429，两个计数均不增加。
3. RPM 每个准入请求增加 1；TPM 准入时预扣 prompt token 估值。被 429 拒绝的
   请求不计入 RPM/TPM。
4. 上游未尝试的后续网关拒绝或 ai-proxy 解析失败，退还 TPM 预扣；RPM 保留为
   一次已准入请求。缓存命中发生在限流插件之前时不消耗 RPM/TPM。
5. 已尝试上游时，TPM 使用 REQ-AI-002 的标准化 total token；没有官方 total 但
   prompt/completion 可安全估算时使用估算结果。不可完整确定时至少保留准入时
   prompt 预扣，不得修正为 0。
6. 修正必须绑定 Store 签发的 opaque reservation token，其中包含准入时的
   window ID/generation 和原始 RPM/TPM 预扣；调用方不得根据当前配置重新拼接。
   请求结束时原窗口已过期，则不得增减当前新窗口；准入头中的 token remaining
   是 prompt 预扣后的同一原子快照，不能宣称为响应结束后的最终值。
7. 单进程内同 key 并发检查必须原子，不得因 check-then-increment 竞态突破当前
   准入上限；不同 key 的计数完全隔离。
8. Virtual Key 窗口条目在过期且无在途修正后必须可回收，删除 key 后不能永久
   占用内存。本需求只保证新 Virtual Key 维度不引入无界增长；所有维度的统一
   清理策略、容量治理和可配置后端仍由 REQ-AI-009 交付。
9. 准入顺序固定如下，内存配额与 PostgreSQL 不要求形成跨存储事务，但所有失败
   必须有补偿：

   1. 校验有效插件链、身份和 policy snapshot，不修改任何计数或账本；
   2. snapshot 显示预算已启用，或携带服务端账务阻断标记时，先查 primary 的
      当前 limit、耗尽、stale/unresolved 与 capability 状态；403/503 在此返回，
      不消耗配额；
   3. 原子联合预扣 RPM/TPM；429 不创建 accounting intent；
   4. `ai-proxy` 完成 model selection 后调用 budget preflight：在同一 key 行锁
      下再次检查主库状态、校验价格并持久化 intent，再允许构造/发送 upstream；
   5. 第 4 步若因并发耗尽、价格、capability，或可确认事务未提交的 DB 失败返回
      403/503，必须使用带 window ID 的 reservation token 精确退还本请求的
      RPM/TPM，且不创建 intent。若数据库 commit 结果不确定，同样退还配额、
      不调用 upstream 并返回 503，但允许留下以 request ID 唯一标识的可恢复
      pending intent；数据库恢复后必须查询该 request ID 并幂等结算为
      `not_incurred=0`，无法确认时继续按 stale/reconciliation 路径 fail closed；
   6. 已创建 intent 后的后续本地失败由 finalize 结算为 `not_incurred=0`；已尝试
      upstream 则按 FR-6/FR-7 结算。
10. `RateLimitStore` 至少提供 backend-neutral 的 `admit/settle/inspect` 语义：
    `admit` 在一次原子操作中联合检查并预扣 RPM/TPM，以内部 request ID 幂等；
    `settle` 以 reservation token 和 operation ID 幂等提交最终 RPM/TPM charge，
    同时覆盖全退、仅退 TPM、实际 token 修正和跨窗口 no-op。Store 返回结构化
    allowed/rejected snapshot，以及
    unavailable/timeout/outcome-unknown/overloaded/corrupt/unsupported 错误，
    不能把远端故障伪装成超限。逻辑 key 使用版本化的结构化 namespace 与稳定
    subject ID，不包含原始 key/name/prefix；`WindowSpec` 是命令输入，backend
    只决定权威时钟和 window/TTL 的物理实现。准入幂等结果、reservation 与
    settlement operation 的保留期必须不短于
    `window duration + 最大请求生命周期 + settlement 重试宽限期`，使 counter
    过期后的迟到修正仍可幂等返回 stale no-op。Redis 的具体实现、连接与可配置
    fail-open 策略仍归 REQ-AI-009。

### FR-4 配额响应与错误契约

1. 已认证且命中 Virtual Key 配额策略的响应，在对应维度已配置时返回：

   - `X-RateLimit-Limit-Requests`
   - `X-RateLimit-Remaining-Requests`
   - `X-RateLimit-Reset-Requests`
   - `X-RateLimit-Limit-Tokens`
   - `X-RateLimit-Remaining-Tokens`
   - `X-RateLimit-Reset-Tokens`

   未配置的维度省略对应三项。`Remaining` 最小为 0；`Reset` 是向上取整的剩余
   秒数。成功、403 与 429 均返回当次可获得的配额快照。
2. RPM/TPM 超限固定返回 HTTP 429，并带 `Retry-After`；同时超限时取较晚的
   reset 秒数，并稳定优先返回 `requests_rate_limit_exceeded`；仅 TPM 超限时
   返回 `tokens_rate_limit_exceeded`。
3. OpenAI/Responses 客户端返回：

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

   TPM 使用对应 message/code。已知 Anthropic 客户端协议时复用 REQ-AI-001 的
   Anthropic error envelope；协议未知时默认 OpenAI envelope。
4. `error_message/error_code` 对已有非 Virtual Key 模式保持兼容；Virtual Key 模式
   的 429/403 状态和机器可读 code 不允许被任意配置改写。
5. 预算基础设施失败统一返回 HTTP 503、OpenAI `type=server_error`（Anthropic
   使用等价 envelope），并固定以下 code/message：

   | code | message |
   |------|---------|
   | `budget_accounting_unavailable` | `Budget accounting is temporarily unavailable.` |
   | `budget_accounting_unresolved` | `Budget accounting requires reconciliation.` |
   | `budget_accounting_unsupported` | `Budget accounting is not supported in this deployment mode.` |
   | `budget_pricing_unavailable` | `Budget pricing is unavailable for this request.` |

   无 `ai-proxy` 的非法链返回 HTTP 500、`type=server_error`、
   `code=ai_policy_chain_invalid`。这些错误不伪装为 403 budget exhausted。
6. 配额 Store 默认 fail closed。`unavailable/timeout/overloaded`，以及以相同
   request/operation ID 有界重查后仍不能确定的 `outcome-unknown`，返回 HTTP
   503、`type=server_error`、`code=quota_backend_unavailable`、
   message=`Quota enforcement is temporarily unavailable.`；`corrupt` 使用
   `quota_backend_state_invalid` /
   `Quota enforcement state is invalid.`；`unsupported` 使用
   `quota_backend_unsupported` /
   `Quota enforcement is not supported in this deployment mode.`。这些错误
   不能伪装成 429 或预算错误；无权威 snapshot 时省略配额头，不得伪造
   remaining/reset。内存容量耗尽也使用同一结构化故障契约。
7. REQ-AI-009 只能通过显式配置增加 fail-open，默认仍为 fail-closed。fail-open
   时不返回伪造的配额头，Admin/Manager/Status 标记 degraded；不能静默切换到
   local 后继续标为 distributed。settlement 结果未知时以相同 operation ID
   有界重试并进入可恢复状态，不能重新执行非幂等增减。
8. 这些头是 Kong-Rust 的 OpenAI 风格配额契约，不宣称等同 Kong 官方
   `X-AI-RateLimit-*` 多窗口头。

### FR-5 预算生命周期与准入

1. `budget_limit` 是可空、非负的 USD Decimal；`null` 表示暂停预算累计与执行，
   `0` 表示立即耗尽。`budget_used` 是非负 Decimal、服务端只读。
2. 预算没有自动日/月重置。首次设置、调高、调低、清空再恢复、disable/enable、
   rename 或 rotate 都保留 `budget_used`；只有未来专用重置/账期功能才可清零。
3. 仅当请求命中 Virtual Key 配额策略且准入时 `budget_limit` 非 `null`，才进入
   预算 accounting。未配置预算期间发生的历史请求不追溯累计。
4. 准入必须从 PostgreSQL primary 读取同一 key 的当前 limit、used 与账务状态，
   不使用认证缓存或读副本。`used >= limit` 时在调用 provider 前返回 403：

   ```json
   {
     "error": {
       "message": "The virtual key budget has been exhausted.",
       "type": "insufficient_quota",
       "param": null,
       "code": "budget_exhausted"
     }
   }
   ```

   已知 Anthropic 协议时返回等价 Anthropic envelope。
5. 对已经启用预算的 key，调高 limit 到大于当前 used 后下一请求从主库恢复，
   调低至不大于 used 后下一请求立即耗尽，不等待认证缓存。`null → 非 null`
   的首次启用依赖 policy snapshot：同进程 Admin 必须 write-through 并让下一
   请求生效；其他节点或带外写允许延迟 `认证 TTL + read-replica lag`，该窗口
   必须在文档和 Manager 中说明。
6. 预算耗尽请求不消耗 RPM/TPM。已经准入但最终结算越过 limit 的请求仍正常完成，
   下一请求开始返回 403，符合“持久消费截止线”定义。
7. accounting intent 创建后采用准入时的 key ID 和预算启用快照；请求在途时发生
   disable、修改非 null limit、rename 或 rotate 都不取消该 intent，仍按实际结果
   结算。新请求按最新配置判断，删除则遵循 FR-7 的 pending 409 规则。
8. 清空 `budget_limit` 与 preflight 创建 intent 必须使用同一 key 行锁串行化。
   仅在该 key 不存在 pending/stale/unresolved intent 时允许清空；否则 Admin
   返回 HTTP 409、机器码 `budget_reconciliation_required`，要求先完成结算或
   reconciliation。若清空先提交，尚未创建 intent 的在途请求按最新暂停状态继续、
   不创建账务；若 intent 先提交，清空失败。disable 不清除账务阻断状态，重新
   enable 后仍需按 stale/unresolved 规则 fail closed。由此，正常 API 路径不能
   产生 `limit=null` 且存在未决账的绕过状态。

### FR-6 成本结算口径

1. 预算成本复用 REQ-AI-002 的同一标准化 usage、PriceCatalog、Model 覆盖价、
   Decimal 精度与 `CostStatus` 口径；不得再实现一套可能漂移的 token/价格公式。
2. `cost_status=calculated` 或 `estimated` 均按 `cost_usd` 结算；UI 与账本保留
   来源，预算不是供应商发票。
3. `not_incurred`（上游前拒绝或未来网关缓存命中）结算 0；上游错误只要已经发生
   且成本可计算，仍需结算。
4. 已调用 provider 但 `cost_status=unavailable` 时不得按 0 静默放行：
   记录 `unresolved` accounting 状态，后续预算请求返回 503
   `budget_accounting_unresolved`，直到运维明确处理。可在 model 选择后、调用
   provider 前确定价格/计费模式不支持时，应提前 503
   `budget_pricing_unavailable`，避免制造新的未决账目。为此本需求明确允许在
   `ai-proxy` 完成 model selection 后、构造/发送 upstream 前调用共享 budget
   preflight hook；不能受当前插件 priority 771/770 的单一 access 顺序限制。
5. 每个内部 request ID 只结算一次。插件 log 重入、请求结束回调重复、DB
   重试或进程恢复不得重复增加 `budget_used`。
6. 未来 REQ-AI-005 的 provider 重试启用前，预算设计必须预留 request/attempt
   关系；届时应按每次实际 provider attempt 结算，而不是只按最终响应覆盖前次
   已发生成本。

### FR-7 独立、原子、可恢复的预算账本

1. PostgreSQL 新增独立预算 accounting ledger。请求产生的 entry 以网关生成的
   内部 `RequestLifecycle.request_id` 为非空唯一幂等键，另存可空 `fact_id` 供
   analytics 关联；migration opening balance 和 Admin reconciliation 使用各自
   唯一 operation ID，允许 request ID 为空。ledger 至少还持久化 ID、Virtual
   Key ID、entry kind、状态、Decimal 金额、cost source/status、不可计算原因与
   时间。它不是 `ai_usage_logs` 的视图或异步 writer 队列。
2. 对预算启用且通过准入的请求，在调用 provider 前先持久化幂等 accounting
   intent；无法写入时返回 503 `budget_accounting_unavailable`，不得 fail open。
3. 请求 finalize 后，在同一数据库事务中把 intent 结算为最终状态，并仅在首次
   结算时执行 `budget_used = budget_used + cost`。唯一约束和受影响行数共同保证
   重试幂等、并发无丢失更新。
4. 已提交的 ledger 与 aggregate 在重启后保持一致；提供校验/重建能力，用 ledger
   合计发现并修复 `budget_used` 漂移。迁移时每个 `budget_used > 0` 的旧 key
   必须生成不可变的 `opening_balance` settled entry，避免新 ledger 为空时重建
   丢失既有余额。客户端不能通过 generic DAO 绕过账本。
5. 进程在 intent 与 finalize 之间退出时，pending intent 不得丢弃或猜成 0。
   方案设计需提供不会误伤仍在运行请求的 stale 判定和显式 reconciliation 路径；
   主库维护服务端权威的账务状态，stale/unresolved key 默认 fail closed 并产生
   可观测告警。该状态不能由客户端 PATCH；清空、删除和 runtime 准入都必须服从
   它，不能只根据可空 limit 猜测。
6. 提供受审计的 Admin reconciliation 操作：管理员针对指定 pending/unresolved
   intent 提交非负 Decimal 结算金额或明确 waive，并填写原因；该操作幂等、写入
   ledger，不能通过 generic PATCH 直接修改 `budget_used`。

   - `GET /ai-virtual-keys/:id/budget-ledger` 至少支持按
     `pending/unresolved` 状态定位 intent；
   - `POST /ai-virtual-keys/:id/budget-reconciliations` 必须指定 intent ID，并在
     `cost_usd_decimal` 与 `waive=true` 中二选一，同时提供非空 reason；
   - crash 发生在 provider 响应后、成本写入 ledger 前时，系统无法自动恢复未知
     的真实金额，只能保持 unresolved 并由管理员结算或 waive，文档不得把它描述
     为自动补算。
7. DB 暂时失败时，预算准入 fail closed；finalize 采用有界重试，仍失败则保留
   pending/unresolved 状态供恢复。preflight commit ACK 丢失等结果不确定场景
   不能假定事务回滚：未调用 upstream 的 intent 在恢复后按 request ID 自动幂等
   结算为 `not_incurred=0`；恢复前保持 pending 并阻断清空，超过 stale 阈值后
   可由 reconciliation 收口。不能转投 REQ-AI-002 的 best-effort usage writer，
   也不能因 analytics 队列满而漏扣已结算成本。
8. budget ledger 不保存 prompt、响应正文、Authorization、原始 Virtual Key 或
   provider 凭据。
9. 删除存在 pending/unresolved intent 的 key 返回 409，必须先 reconciliation。
   所有 intent 已结算后允许删除 key，但 ledger 使用 key ID/name/prefix 快照保留
   历史且不级联删除；快照不得包含原始 key 或 key hash。
10. 代理、插件和预算领域逻辑只依赖 `BudgetAccountant/Store` 契约，不能直接依赖
    `PgDao`、`PgPool` 或 SQL 类型。首版 PostgreSQL backend 实现上述事务语义，
    但锁粒度必须限定为单 key、不得引入跨 key 全局串行；未来替换为分片事务账本
    或独立 admission 热状态时，不得改变 request ID 幂等、审计、reconciliation
    与 fail-closed 契约。analytics PG/ES/Kafka 等 sink 不能作为预算权威来源。

### FR-8 数据精度与 Admin API

1. forward-only migration 将 `budget_limit/budget_used` 从 double 转为
   `NUMERIC(28,12)`（或不低于 REQ-AI-002 的等价精度），迁移现有有限非负值并加
   数据库约束。TPM/RPM 增加正整数约束；因为允许结算超过 100%，不得增加
   `budget_used <= budget_limit` 约束。
2. 迁移前按 key ID、字段和值执行 fail-fast 审计：budget 的负数、
   NaN/Infinity、绝对值达到 `10000000000000000`、超过 12 位小数，以及非 null
   的 TPM/RPM `<=0` 都中止迁移并给出可修复错误；不得静默改 0、改 null 或舍入。
3. Rust 内部金额统一使用 `rust_decimal::Decimal`，不得在校验、累加、比较或
   Manager 计算中经过 JavaScript/Rust 二进制浮点。
4. 为兼容现有 AI Virtual Key API，`budget_limit/budget_used` 保留 JSON number
   兼容投影，同时新增固定 12 位字符串
   `budget_limit_decimal/budget_used_decimal`；新代码和 Manager 只使用 Decimal
   字段。超出 JSON number 安全表示范围时兼容字段允许为 `null`，精确字段仍完整。
   规范写入字段是 `budget_limit_decimal` 字符串/null；legacy `budget_limit`
   number 仅做可无损转换的兼容输入。两者同时出现必须解析为完全相同的
   Decimal/null，否则返回字段级 400。Manager 只发送精确字段，
   `budget_limit_decimal=null` 表示清空。输入最多 12 位小数且必须小于
   `10000000000000000`；超精度、超范围或需要舍入的值一律拒绝，不静默截断。
5. create 强制 `budget_used=0`，调用方传入 `budget_used` 或
   `budget_used_decimal` 返回 400；PATCH 同样只读。`key_hash/key_prefix`
   只能由 create/rotate 内部路径修改。
6. Admin 对 name、TPM/RPM、budget limit 的类型、范围、精度和有限性执行字段级
   校验；错误结构与现有 Kong schema violation 保持一致。
7. Virtual Key list/get 返回派生 `quota_enforcement` 和 `budget_status` 元数据，
   quota 至少区分
   `unconfigured/awaiting_plugin/configured_local_partial/configured_local/unsupported`，
   budget 至少区分
   `unconfigured/paused/awaiting_plugin/active/warning/exhausted/unresolved/unsupported`。
   同时返回
   `auth_endpoint_count` 和 `enforced_endpoint_count`（或等价摘要），让部分
   Endpoint 挂载不会被误报为全局生效。这是 Manager 展示状态的唯一 source of
   truth；派生逻辑必须按 global/service/route 的有效插件覆盖规则计算，不能只
   检查任意一条 plugin 记录，也不得把本地单节点状态标成全局额度。
8. `budget_status` 与实际执行交叉计算：

   - limit 为 null 且 used 为 0：`unconfigured`；
   - limit 为 null 且保留了非零 used：`paused`，仍返回并展示历史 used；
   - limit 非 null 但 `enforced_endpoint_count=0`：`awaiting_plugin`；
   - 至少一个 Endpoint 生效后，才按比例返回 `active/warning/exhausted`；
   - unresolved 或运行模式不支持分别优先返回 `unresolved/unsupported`。

   `unresolved` 优先级不受 limit 是否为 null 影响；FR-5 的行锁与 409 规则保证
   正常 API 不会制造这种组合。迁移、带外写或数据损坏发现 `limit=null` 且账务
   阻断时，Admin 仍显示 `unresolved`，runtime 必须 fail closed，不能按 paused
   放行。
9. settlement 前执行 checked Decimal addition。若单条 cost 合法但
   `budget_used + cost`、reconciliation 或 ledger rebuild 超出
   `NUMERIC(28,12)`/Rust Decimal 范围，不得部分提交或截断：intent 转为
   `unresolved`，原因固定为 `budget_numeric_overflow`，保留该笔合法 cost，
   aggregate 不变并对后续请求 fail closed，直到人工 reconciliation。

### FR-9 运行模式与故障语义

1. 运行模式能力矩阵固定如下：

   | 模式 | Virtual Key 身份 | TPM/RPM | 持久预算 |
   |------|------------------|---------|----------|
   | PostgreSQL traditional | 支持 | 支持，本进程 local | 支持，primary + ledger |
   | standalone DB-less | 声明式配置内存在时支持 | 支持，本进程 local、重启清零 | 不支持；配置后 503 |
   | Hybrid CP/DP | AI key 尚未完整同步，产品能力不支持 | `quota_enforcement=unsupported`，不把 DP 偶然存在的配置宣传为支持 | `budget_status=unsupported`，无 DP→CP accounting |

2. standalone DB-less 的 `budget_limit` 非 `null` 时，命中策略的请求返回 503
   `budget_accounting_unsupported`，绝不使用易失内存伪装成持久预算。
3. Hybrid Admin/Manager 的 quota 与 budget capability 均明确为 unsupported。
   DP 因 key 未同步而认证失败时保持 401；若 DP 通过手工/未来路径实际取得 key 并
   命中预算策略，预算仍返回 503 而不是静默放行。这种偶然本地配置不构成受支持的
   Hybrid TPM/RPM。AI 实体同步与 DP→CP accounting 另行立项。
4. RPM/TPM 的 local 限制必须在 API、Manager、guide 和错误排查文档中显著说明：
   N 个无粘性负载均衡节点理论上可使用约 N 倍额度；全局限流由 REQ-AI-009 交付。
5. 非预算 key、没有 Virtual Key 配额策略的 Endpoint，以及普通非 AI 请求不新增
   数据库热路径或行为变化。为保持该性能边界，其他节点/带外执行的
   `null → 非 null` 首次启用采用 FR-5 定义的最终一致语义。

### FR-10 Kong Manager

1. Virtual Keys 列表以精确 Decimal 显示“预算启用期间累计”的生命周期 USD
   budget：
   `used / limit`、百分比与进度条；进度大于 100% 时数值保留，视觉宽度 clamp 到
   100%。limit 为 0 时直接显示已耗尽和 100%，不得执行除零；`null` 按历史 used
   显示“未配置”或“已暂停”，并继续展示保留的 used，不得写成 `$0` 或隐藏历史。
2. 预算状态至少提供：正常、达到 80% 的预警、已耗尽、账务未决、当前运行模式不
   支持；已耗尽行显示醒目徽章。
3. TPM/RPM 显示“未配置”或具体额度，并明确“本节点 / 60 秒窗口”。页面只消费
   Admin API 派生的 `quota_enforcement`，区分“已配置但待挂载”和“已有 Endpoint
   策略”，并以 `enforced/auth endpoint count` 明示部分挂载；不能自行扫描插件
   后另算一套状态，也不能仅因 key 字段非空就写“已生效”。
4. 编辑表单使用字符串承载 Decimal、标注 `USD / 生命周期累计`，前端与后端均
   拒绝非法数字；`budget_used` 只读。清空 budget limit 使用 `null`，不能误传
   `0`。
5. 更新 Virtual Keys 信息 banner、不可达旧 Overview 文案及中英文 i18n，移除
   “字段仅存储、未执行”的过期描述，改为说明激活条件、本地窗口和预算运行模式。
6. 本需求完整交付 `ai-rate-limit` 的 schema，因此现有通用 Plugins 页面可以
   schema-driven 配置它，并从 Virtual Keys 提供跳转；专用表单体验和发布向导
   自动创建、更新、删除、回滚策略仍由 REQ-AI-006 交付。
7. loading、空态、API 错误和 unsupported 状态可区分；窄屏、键盘访问、颜色之外
   的状态文字和 English/简体中文均可用。

### FR-11 文档、可观测性与兼容声明

1. 更新中英文 AI Gateway guide：启用步骤、字段语义、60 秒 local 窗口、响应头、
   错误示例、生命周期预算、下一请求生效的超支边界、Decimal 显示、
   DB-less/Hybrid 限制与恢复操作。
2. 更新 `docs/design.md`、`docs/tasks.md` 和 implementation log，记录预算账本、
   请求生命周期接点、失败语义和 REQ-AI-009/006 的边界。
3. 暴露或记录不含高基数原始 key ID 的计数：429（按 RPM/TPM 原因）、403、
   accounting intent/settled/idempotent-replay/unresolved、DB failure 和
   reconciliation；日志只允许 key prefix/哈希化 ID 等非敏感定位信息。
4. 文档明确：
   - `virtual_key`、403 和生命周期 ledger 是 Kong-Rust 扩展；
   - 当前不是 Kong 官方 policies schema、sliding/multi-window 或
     `X-AI-RateLimit-*` 的完整实现；
   - 成本是标准列表价估算，不是供应商账单；
   - 持久预算不是严格零超支 reservation。

## 非功能需求

- **正确性：** 单进程配额并发准入无部分扣减；预算 settlement 对同一 request
  幂等，主库并发更新不丢失。
- **持久性：** 已提交账本在进程重启后可恢复；pending/unresolved 不被静默删除。
- **精度：** 金额全链路 Decimal，固定 12 位传输/存储精度；比较与累加不经过
  `f64`。
- **故障安全：** 预算后端不可用、账务未决或运行模式不支持时 fail closed；
  非预算请求不受预算 DB 故障影响。
- **性能：** 非预算 key 不新增数据库 I/O；预算 key 允许为准入和结算承担强一致
  DB 成本，但只能按 key 加锁，不得使用跨 key 全局锁。代表性测试需记录额外
  p50/p95/p99、吞吐、连接池等待和热点 key 影响，不设未经基线证明的固定百分比
  承诺。
- **可扩展性：** `RateLimitStore` 与 `BudgetAccountant/Store` 必须可注入、
  async-ready 且不泄漏 Memory/PG 私有类型。设计必须给出单节点与多节点容量档位、
  QPS/并发、节点数、Virtual Key 基数、每请求远程往返次数、超时/背压/降级、
  状态容量和高基数 soak 基线；local 模式不得被宣传为全局配额。
- **隔离：** Virtual Key ID 是唯一配额与预算归属；workspace、route、consumer
  或 key name 相同均不得串账。
- **隐私：** 配额计数、账本、API、日志和 Manager 均不保存或展示原始 key、
  key hash、prompt、响应正文或 provider 凭据。
- **兼容：** `global/route/consumer` 现有合法配置保持运行；旧非法配置不再静默
  工作，升级前须由文档/启动告警指出。

## 非目标（本单不做）

- 严格零超支的最大成本 reservation、预付余额、退款、自动充值、日/月账期、
  定时重置、多币种、税费、组织折扣或供应商发票对账。
- Redis/cluster 限流 backend、跨节点共享窗口、自然分钟/sliding/multi-window、
  所有维度的通用内存容量治理和窗口长度配置（REQ-AI-009）；本需求交付可替换
  Store 契约和自身引入的过期 Virtual Key 条目回收，不交付 Redis 连接。
- Kong AI Rate Limiting Advanced 的 `policies`、`identifier`、
  `tokens_count_strategy`、provider/model 分区或 `X-AI-RateLimit-*` 完整兼容。
- 发布向导自动挂载/回滚 ai-rate-limit，以及 ai-cache/ai-prompt-guard 的完整
  Manager schema（REQ-AI-006）。
- AI Virtual Key 在 Hybrid 的 CP→DP 同步、DP→CP accounting 上报和全局预算。
- 将 REQ-AI-002 的 best-effort usage facts 变成审计计费表；它仍只用于分析，
  预算只复用同步生成的成本口径。
- Elasticsearch/OpenSearch、ClickHouse、Kafka 等外部 usage/log sink、查询后端、
  retention、双写和迁移（REQ-AI-013）。
- Consumer Group、credential、IP、header、path、service 等新限流维度。
- 自动为已有 key 的历史 usage 回填 `budget_used`。

## 需求分析决策记录（2026-07-26）

1. **激活方式：** 只有 `ai-key-auth` + `ai-proxy` +
   `ai-rate-limit(limit_by=virtual_key)` 的有效插件链执行 key 配额/预算；缺少
   ai-proxy 是 fail-closed 配置错误，本需求不自动给所有 AI Endpoint 挂策略。
2. **身份来源：** 只信任 `AiAuthContext.virtual_key_id`，不二次解析原始 header；
   缺身份 fail closed，非法枚举在保存与 runtime 双重拒绝。
3. **限额来源：** Virtual Key 自身 TPM/RPM 权威；`null` 不限制，
   TPM/RPM 的 0 非法。配额仍是首次命中起算的本地 60 秒固定窗口。
4. **计数语义：** 预算状态预检 → RPM/TPM 联合预扣 → model-selection budget
   preflight/intent → upstream；403/503 会用 reservation token 补偿配额，
   429 不建 intent。TPM 按实际/安全估算 total 修正，跨窗口不修改新窗口。
5. **预算定位：** 单 key、USD、生命周期累计的持久消费截止线；已耗尽请求前置
   403，已准入和并发请求允许结算后超过 100%，严格 reservation 不在本单。
6. **累计时段：** 仅在 budget limit 非空且策略生效期间累计，不回填未启用期间
   历史；清空 limit 暂停执行但保留 used。
7. **成本口径：** calculated/estimated 都结算，not_incurred 为 0，unavailable
   进入 unresolved 并阻止后续预算请求；复用 REQ-AI-002 计算，不依赖其 writer。
8. **一致性策略：** provider 前创建持久 intent，finalize 时以内部 request ID
   幂等 settlement，并在同一事务原子更新 aggregate；fact ID 仅关联，
   pending 不静默丢失且可人工 settle/waive。
9. **精度/API：** 内部和数据库使用 Decimal/NUMERIC；旧 JSON number 保留兼容
   投影，新增固定 12 位精确字符串；双字段同传必须等值，Manager 只发送精确字段，
   budget_used 只读。
10. **运行模式：** PostgreSQL traditional 支持预算；独立 DB-less 仅支持易失
    本地配额，预算 fail closed unsupported；Hybrid 明确不支持。
11. **响应契约：** 请求/token 各自使用 `X-RateLimit-*` 三元组，429 带
    `Retry-After`；OpenAI/Responses 使用嵌套 error，Anthropic 使用对应协议体。
12. **产品边界：** REQ-AI-003 完整拥有 ai-rate-limit bundled/schema/校验和
    通用 schema-driven 配置；Manager 展示待挂载、部分/全部本地生效，专用 UX
    与发布向导归 REQ-AI-006；Kong 官方兼容声明限定为概念参考。
13. **规模演进：** 本单交付异步、后端无关的 `RateLimitStore` 和
    `BudgetAccountant/Store` 边界，Memory/PG 只是默认 adapter；Redis 多节点
    配额由 REQ-AI-009 实现，外部 usage/log 存储由 REQ-AI-013 实现，任何
    analytics 后端都不参与强一致预算决策。

## 方案设计阶段必须落实的技术输入

以下范围已经定稿，`design.md` 需给出可编码的具体方案：

1. 如何扩展 `AiAuthContext` 或引入不可变 `VirtualKeyPolicySnapshot`，同时让预算
   主库读取与认证缓存解耦；
2. 异步 `RateLimitStore` 的命令/结果/错误类型、结构化 key namespace、
   `WindowSpec` 与 backend 职责、跨 Endpoint Virtual Key bucket、request/operation
   幂等及保留期、联合原子窗口、opaque reservation/window ID、reset snapshot、
   跨窗口修正、outcome-unknown 恢复、bounded cleanup 和 REQ-AI-009 接入 Redis
   时不改上层契约的边界；
3. `BudgetAccountant/Store` 的 backend-neutral 边界、crate 归属、PG adapter 与
   primary pool 装配、单 key 锁、ledger schema、状态机、事务 SQL、唯一键、
   aggregate 校验/重建；
4. FR-3 固定顺序在 access、`ai-proxy` post-selection preflight 与 finalize 的
   具体接点，以及 reservation token 补偿、short-circuit、解析失败、流中断、
   客户端断开和 log 重入路径；
5. stale pending 的租约/进程身份/超时判定与 reconciliation API，确保不把仍在
   运行的长流请求误标未决；
6. REQ-AI-002 同步成本对象如何成为共享只读输入，同时确保 budget 不等待或依赖
   usage writer；
7. double→numeric 的安全迁移、旧非法数据处理、Decimal 兼容投影、数据库约束和
   rollback 不支持说明；
8. Virtual Key POST/PATCH 与 plugin create/PATCH/PUT-upsert/schema-validate 的
   共用校验，bundled 注册及 REQ-AI-006 后续专用 UX/向导的兼容扩展点；
9. DB-less/Hybrid capability 如何传给 runtime 和 Manager，503 错误如何避免被
   误归类为预算耗尽；
10. 响应头在成功、短路和正常代理路径的注入时机，以及 OpenAI/Anthropic error
    adapter 的复用；
11. Admin 如何按有效插件覆盖规则与 runtime capability 派生唯一生效状态，
    Manager 如何只消费该状态而不是扫描插件或用 key 字段猜测；
12. 预算 DB 热路径的连接池、超时、重试、告警与性能基线；
13. 单节点与企业多节点容量模型：目标 QPS/并发、节点数、Virtual Key 基数、
    每请求 Store/DB 往返、连接池等待、热点 key、内存条目与清理成本、远端
    backend 背压/降级、低基数指标，以及高并发/高基数/超时/结果不确定的负载
    与故障验证方案。

## 验收标准

1. 创建两个 Virtual Key 并在同一 Endpoint 并发调用：每个 key 的 RPM/TPM 独立，
   一个 key 429 不影响另一个；同 key 并发准入不突破当前进程限额，RPM/TPM 任一
   失败时没有部分扣减。同一 key 经两个挂载策略的 Endpoint 并发调用仍共享一份
   窗口，Route/plugin ID 不产生额外额度。
2. `null` TPM/RPM 不计数且不返回该维度头；0、负数、小数和溢出值在 Virtual Key
   POST/PATCH 均返回字段级 400。rename/rotate 不重置窗口或预算；过期/删除 key
   的窗口条目可回收。活动窗口内调高、调低、设 null 和恢复时保留同一 generation
   及既有 count，行为符合 FR-2。
3. `limit_by=virtual_key` 使用认证 key ID；缺身份返回 401，绝不落入 global。
   任意非法 `limit_by` 在 plugin create/PATCH/PUT、schema validate 和声明式
   runtime 解析均被拒绝；缺少 `ai-proxy` 返回指定 500 且不计数/建 intent；
   合法 `global/route/consumer` 与旧 `header_name` 配置回归通过。
4. 成功、RPM 429、TPM 429 和预算 403 的真实 HTTP 响应包含适用的
   limit/remaining/reset 头；429 有正确 `Retry-After`。OpenAI/Responses 与
   Anthropic 错误 envelope、status、type/code 全部符合 FR-4/FR-5；RPM/TPM
   同时超限时稳定选择 requests code 和较晚 reset。
5. TPM 对官方 total、估算 total、无 usage、上游前失败和超过一个窗口的长请求
   分别正确修正；跨窗口完成不会增减新窗口，未知 completion 不会把 prompt
   预扣退成 0。
6. PostgreSQL 下按 key 发起可计价请求后，`budget_used_decimal` 按
   calculated/estimated 成本持续累计；相同内部 request ID 重复 finalize 只扣一次，
   多请求并发 settlement 无丢失更新，analytics writer 丢弃不影响预算。
7. `budget_used < limit` 时请求放行；一次请求可把 used 推到超过 limit，下一请求
   在 provider 前 403；调高到大于 used 后下一请求恢复。调低、清空再恢复、
   disable/enable、rename/rotate 的行为符合 FR-5，进度可显示超过 100%。
   pending/stale/unresolved 存在时清空返回指定 409；结算完成后清空才进入 paused，
   恢复仍保留 used，且不能通过 disable/enable 绕过账务阻断。
8. provider 前 intent 写失败时请求 503 且不打上游；finalize DB 暂时失败、
   重复重试、进程在 intent 后退出和重启均不静默丢账。stale/unresolved 会
   fail closed、可经指定 Admin API 定位和显式 reconciliation；已提交 aggregate
   可由含 opening balance 的 ledger 校验/重建。pending/unresolved key 删除返回
   409，结算后删除不级联清除 ledger。累计或 rebuild 溢出时形成
   `budget_numeric_overflow` unresolved，aggregate 不被部分修改。
9. `cost_status=unavailable` 不按 `$0` 放行：可预判时上游前拒绝，响应后发现时
   形成 unresolved 并阻止后续请求；`not_incurred` 为 0，已发生且可计价的上游
   错误仍扣费。
10. 客户端不能在 Virtual Key POST/PATCH 写 `budget_used(_decimal)` 或
    `key_hash/key_prefix`；金额迁移为 NUMERIC/Decimal，12 位精确字段往返与
    兼容 number 投影正确；limit number/decimal 同传等值可接受、冲突返回 400、
    decimal null 可清空，超 12 位/范围或需舍入的输入被拒绝。真实 PostgreSQL
    migration/约束/事务测试通过，脏旧值会带 key ID/字段 fail-fast，既有非零 used
    生成 opening balance 且 rebuild 不漂移，也不存在 used<=limit 约束。
11. DB-less 的两个 key 可使用本地 RPM/TPM 且重启清零被明确展示；配置 budget
    后返回指定 503/unsupported。Traditional、standalone DB-less、Hybrid 的
    quota/budget capability 与 FR-9 矩阵一致；Hybrid Admin/Manager 两项均显示
    unsupported，DP 实际存在 key 且命中预算策略时不得静默放行，认证未通过时
    仍可先返回 401。
12. Manager 完成 Decimal 预算进度、80% 预警、耗尽/未决/unsupported 徽章、
    本地 TPM/RPM 状态、待挂载与部分/全部 Endpoint 计数、表单校验和
    双语/窄屏/可访问性；Playwright 覆盖编辑、清空、超 100%、rotate 保留累计、
    调高恢复、未配置/暂停仍展示历史 used 和错误状态。
13. 中英文 guide、架构设计、任务状态和 implementation log 与实现一致，明确
    local/多节点限制、下一请求生效、非严格 reservation、Kong-Rust 扩展和
    REQ-AI-006/009 的后续边界。
14. 预算初次/二次 preflight、quota 预扣和 intent 的执行顺序通过故障注入验证：
    预算 403、pricing/capability 503、quota 429 及可确认未提交的 accounting 503
    均不留下错误计数或 intent；commit 结果不确定时不调用 upstream、精确退还
    quota，允许留下可按 request ID 恢复的 pending intent，并最终自动幂等结算
    `not_incurred=0` 或经 reconciliation 收口；已建 intent 后的其他本地失败也
    正确结算 0。
15. 四种预算 503、三种配额 backend 503 和非法链 500 均通过真实 HTTP 验证固定
    status、message、type/code 及 Anthropic 等价体；无 Store snapshot 时不伪造
    配额头。unresolved 可通过 ledger GET 定位，并能以人工 settle 或 waive 幂等
    解除阻断。
16. Memory backend 与确定性 remote fake 通过同一 `RateLimitStore` contract
    suite：联合准入、同 request 重放、settle 重放、全退/部分退、跨窗口 no-op、
    outcome unknown 重查和结构化故障均符合 FR-3；相同 `WindowSpec` 产生相同
    可观察窗口行为，保留期覆盖迟到 settle。插件与预算服务的测试可注入 Store，
    代码审查确认不依赖具体 `DashMap`/PG 类型。设计给出的并发与高基数基准完成
    并记录 p50/p95/p99、吞吐、内存/连接池和错误恢复结果。
