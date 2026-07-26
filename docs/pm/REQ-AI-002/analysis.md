# REQ-AI-002 需求分析 — Token 成本核算与用量事实表

> Cost Accounting & Usage Facts — Requirement Analysis
>
> - **优先级 / Effort：** P0 / L（分析后由 M 调整；涉及代理生命周期、存储、查询与完整 Manager 页面）
> - **需求分析定稿：** 2026-07-26（基于现有产品路线、代码基线与官方价格源定稿）
> - **依赖：** 无；REQ-AI-001 已完成，存在 Virtual Key 时自动关联身份
> - **需求单索引：** [../backlog.md](../backlog.md)
> - **方案设计：** [design.md](design.md)（已于 2026-07-26 定稿）

## 背景与价值

当前 AI 代理已经能从 OpenAI、Anthropic、Gemini 与 OpenAI-compatible 响应中提取
部分 token usage，也能在请求侧计算 prompt token 估值；`AiModel` 已有
`input_cost` / `output_cost` 字段，`calculate_cost` 也实现了按百万 token 计算
USD 成本的公式。但这些能力没有接入生产调用链：

- `calculate_cost` 仅在单元测试中使用，生产代码零调用点；
- 没有用量事实表、异步写入器、明细查询或汇总 API；
- 现有 AI 日志缺少 Virtual Key、最终状态、TTFT、缓存状态、价格来源与 token
  来源；
- 缺 token 或缺价格会被现有纯函数按 `0` 计算，无法区分“真实免费”和“未知”；
- 仓库中只有不可达的旧资源数量 Overview，没有真实调用量、成本趋势或调用日志。

没有稳定、可追溯的请求级事实，就无法可靠交付成本分析，也无法为
REQ-AI-003 提供一致的价格与成本口径。本需求交付的是**运营分析级**用量与标准
列表价成本估算，不把它宣称为供应商账单、预算执行账本或审计级计费账本。

## 目标与成功定义

1. 对命中包含 `ai-proxy` 的 Route 的每个客户端请求形成一条请求级事实，健康运行
   时不漏记、不重复，并覆盖成功、提前拒绝、上游失败和流中断。
2. 优先保存 provider 官方 usage；无法获得时明确标注估算、混合或不可用，绝不
   用 `0` 代替未知值。
3. 按请求发生时的有效 USD 单价计算并固化成本；价格更新后历史结果不漂移。
4. 提供共享过滤口径的明细与汇总 API，Manager 展示真实成本、token 趋势、排行
   和元数据调用日志。
5. PostgreSQL 写入不阻塞代理热路径；独立 DB-less 模式仍可查看有明确易失性
   提示的本节点数据。

## 用户故事

- 作为网关运维者，我可以查看最近 24 小时、7 天或 30 天的请求数、token 和估算
  成本，识别增长趋势与未定价流量。
- 作为平台负责人，我可以按实际模型、Virtual Key、Route 或 Service 下钻，定位
  主要成本来源和错误请求。
- 作为模型管理员，我可以配置每百万 token 的自定义覆盖价，并知道当前请求采用
  的是覆盖价、内置价还是未定价。
- 作为故障排查人员，我可以用 request ID 找到一次调用的状态、模型、token、
  e2e、TTFT、流式与缓存元数据，但不会在分析系统中看到 prompt、响应正文或凭据。
- 作为 DB-less 用户，我可以查看本节点近期调用，同时清楚知道数据有容量上限、
  重启会清空且不会跨节点聚合。

## 术语与统计口径

| 术语 | 本需求定义 |
|------|------------|
| AI 请求 | 已匹配 Route，且最终有效插件链包含启用的 `ai-proxy` 的客户端请求；未匹配 Route 的 404 和不含 `ai-proxy` 的普通代理请求不计入 |
| 请求事实 | 一个客户端请求的最终汇总记录；一次请求只有一条，不按未来重试次数拆成多条 provider attempt |
| 官方 usage | provider 响应中明确返回的 token 数 |
| 估算 usage | TokenizerRegistry、响应文本估算或最终字符估算得到的 token 数 |
| 标准化 usage | 把 provider 原生字段映射为可跨 provider 汇总和计价的 prompt/completion；例如 Gemini completion 包含 candidate 与 thinking token |
| 标准成本 | 标准在线推理的 input/output token 列表价估算，币种固定为 USD |
| 定价状态 | `matched` 表示 input/output 有效单价均已解析；`unmatched` 表示至少一个方向缺价；`unsupported` 表示请求采用了首版不支持的计价模式；未调用 provider 时为 `not_applicable` |
| 成本不可计算 | 已调用 provider，但因 usage 缺失、价格未匹配或计价模式不支持而无法计算；成本为 `null`，不是 `0`，并携带原因 |
| e2e | 从网关收到请求到 log/finalize 阶段的完整耗时，包含认证、策略、解析、选模、上游和响应处理 |
| TTFT | 流式请求从网关收到请求到首个可解析上游 SSE 事件的耗时；非流式、首事件未到或失败时为 `null` |
| 时间范围 | `start` 包含、`end` 不包含，即 `[start, end)`；传输使用 RFC 3339 UTC |
| 结果类别 | `success`、`gateway_rejected`、`gateway_error`、`upstream_error`、`client_disconnected` 或 `stream_interrupted` |
| 流式模式 | `stream` 表示经 `allow/deny/always` 策略处理后的有效请求模式，不表示错误响应一定以 SSE 返回 |
| 缓存状态 | 指 Kong-Rust AI 响应缓存，不指 provider prompt cache；取值为 `not_configured`、`unavailable`、`bypass`、`miss` 或 `hit` |

## 现状事实与约束（2026-07-26 基于代码核实）

- `AiRequestState` 已保存实际 `AiModel`、`AiProviderConfig`、stream、三项
  `TokenUsage`、prompt 估值、较晚启动的计时器和部分流式 TTFT；`RequestCtx`
  已有 route/service/consumer 和最终响应状态。
- 真正覆盖完整请求生命周期的 `request_start_time` 与每请求 `request_id` 只在
  `kong-proxy::KongCtx`，尚未贯通到插件事实上下文。现有 `ai-proxy` e2e 从完成
  解析和选模后才开始，不能直接沿用。
- `AiAuthContext` 可提供 `virtual_key_id`、key 名与 `consumer_id`；但认证白名单
  拒绝发生在该上下文写入前，早期失败允许身份字段为空。
- `ai-proxy` 只在 access 成功末尾创建 `AiRequestState`。`ai-key-auth`、
  `ai-prompt-guard`、`ai-rate-limit` 提前短路，或 `ai-proxy` 自身解析/选模失败
  时，现有 `ai-proxy.log` 不会形成 AI 日志；事实采集不能以
  `AiRequestState` 是否存在作为记录条件。
- OpenAI、Anthropic、Gemini 与 OpenAI-compatible 已能提取官方 usage，但内部
  模型只有 prompt/completion/total 三个可空总量，没有来源和 provider
  prompt-cache 细分。流式路径还会忽略官方 `total_tokens` 后自行相加。
- TokenizerRegistry 超时或不可用时会自动退化到字符估算，但当前只返回 `u64`，
  不保留具体 tokenizer provenance。本需求至少需要保留“官方 / 估算 / 混合 /
  不可用”四级口径。
- `AiModel.input_cost` / `output_cost` 的单位已约定为 USD / 1M tokens，但数据库
  使用 `DOUBLE PRECISION`，Admin API 尚未拒绝负数或非有限值，Manager 也没有
  标明单位、覆盖语义和快照日期。
- DB-backed model 有稳定 ID；inline model 没有稳定 ID，`model_routes` 当前还会
  生成请求级临时 UUID。事实维度必须允许 ID 为空，并同时保存模型组名和实际
  provider model 名称快照。
- 通用 `Dao<T>` 只支持 UUID 正序游标与少量等值过滤，没有时间范围、稳定倒序
  游标、批量写入或 SQL 聚合能力；usage 需要专用 repository/query 接口。
- `ai-cache` 目前只有缓存键基础设施，`cache_hit` 恒为 false，无法区分未配置、
  bypass、miss 与功能尚不可用。
- `/ai-gateway` 已明确以 AI Endpoint 为默认入口；现有 `Overview.vue` 不可达。
  `docs/design.md` 已为 analytics 预留“调用统计”信息架构入口。
- Manager 没有直接图表依赖；趋势图的实现方式或新增依赖在方案设计阶段确定。

## 功能需求

### FR-1 请求覆盖与唯一性

1. 凡符合“AI 请求”定义的请求，均在最终响应或连接终止时尝试生成一条事实：
   - 正常非流式与流式响应；
   - `ai-key-auth`、Prompt Guard、限流等上游前短路；
   - 请求体/配置/模型解析失败；
   - 上游 4xx、5xx、连接失败与超时；
   - 客户端断开或流中断。
2. 早期失败允许 provider、模型、身份、usage、TTFT 与 cost 等字段为空，但
   route/service、request ID、最终状态/结果和完整 e2e 应尽可能存在。
3. 当前代理没有 provider 重试，一个请求的上游尝试数只能为 0 或 1。一个客户端
   请求只生成一条请求事实；REQ-AI-005 将来启用重试前，必须新增关联的 attempt
   子事实，并把请求事实定义为跨 attempt 汇总，不能把多个 attempt 的 token/cost
   归到单一 provider/model，也不能因多个 attempt 重复请求计数。
4. `ai-proxy.logging.log_statistics=false` 只影响 Kong 兼容的序列化日志，不关闭
   本需求的元数据事实采集。

### FR-2 请求事实字段

`ai_usage_logs` 及 DB-less 等价记录至少包含：

- 标识与时间：事实 ID、唯一 request ID、内部单调写入序号、`started_at`、
  `finished_at`、可空 `workspace_id` 快照；
- 网关维度：route/service 的可空 ID 与请求时名称快照；
- AI 维度：最终 provider/model 的可空稳定 ID、provider 名/类型、客户端请求模型、
  model group、实际 provider model 名称快照，以及 `attempt_count`（首版为 0/1）；
- 身份维度：可空 `virtual_key_id`、key 名/prefix 快照、`consumer_id`；
- usage：可空的标准化 prompt/completion/total tokens、可空 reasoning/thinking
  与 provider prompt-cache read/write breakdown、usage 来源；
- 价格：请求时采用的 input/output 每百万 token 单价；input/output 各自的价格
  来源、版本、快照日期与有效期；定价状态与币种 USD；
- 结果：可空 `cost_usd`、成本状态、可空的成本不可计算原因列表、最终状态码、
  结果类别、完整 e2e、可空 TTFT、是否已尝试上游、有效 stream 模式与缓存状态。

事实记录不依赖配置实体长期存在。删除 Route、Service、Model、Provider、Virtual
Key 或 Consumer 不得级联删除历史事实；UI 通过快照名称或“已删除 + ID”继续展示。

首版产品仍是单一默认 workspace：事实优先固化匹配 Route 的 `workspace_id`，缺失时
使用当前默认 workspace ID；`/ai-usage*` 只查询默认 workspace，不接受调用方选择
workspace。完整的多 workspace 路由、Admin API 上下文与 analytics 隔离不在本单
范围，不能仅因事实表预留了字段就宣称已支持。

结果类别与统计口径固定如下：

- `success`：请求正常完成、最终下游状态为 2xx，且没有 transport/stream 错误；
- `gateway_rejected`：在调用 provider 前被认证、Prompt Guard、限流或其他网关策略
  主动短路；
- `gateway_error`：网关解析、配置、选模、转换或内部处理失败；
- `upstream_error`：已尝试 provider，但收到非 2xx 响应、连接失败或超时；
- `client_disconnected`：下游客户端在请求完成前主动断开；
- `stream_interrupted`：流已经开始，非客户端主动断开但未正常结束。

同一请求只取一个最终类别；优先级为 `client_disconnected` >
`stream_interrupted` > `gateway_rejected` > `gateway_error` > `upstream_error` >
`success`。`successful_requests` 仅统计 `success`，其他类别均计入
`failed_requests`；未能向客户端发送最终状态时 `status_code` 为 `null`。

### FR-3 Token 归集与来源

1. prompt、completion 是用于跨 provider 汇总和成本公式的标准化 billable usage；
   total 优先保留 provider 官方总量，官方 total 存在时不得被本地相加覆盖。
   provider 提供 reasoning/thinking breakdown 时另行保存，不能丢弃或重复计数。
2. 首版 provider 映射至少满足：
   - OpenAI / OpenAI-compatible 使用官方 prompt/completion，官方已纳入
     completion 的 reasoning token 不再重复相加，并提取
     `prompt_tokens_details.cached_tokens` 等 cache-read breakdown，以及 provider
     暴露的 `cache_write_tokens`；Chat、Responses、非流式与流式终态 usage
     使用同一归一化规则；
   - Anthropic prompt 标准化为
     `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`，
     completion 使用 `output_tokens`，并分别保留两个 cache breakdown。非流式
     使用最终 usage；流式从 `message_start` 取得 input/cache，从最后一个
     `message_delta.usage` 取得累计 output（及其明细），在 `message_stop` 后定稿；
     不得把多个 delta 的累计值再次求和，也不能把 `message_start` 的初始 output
     当作最终 completion；
   - Gemini prompt 使用 `promptTokenCount`，completion 使用
     `candidatesTokenCount + thoughtsTokenCount`；`thoughtsTokenCount` 另存为
     breakdown，total 保留 `totalTokenCount`，并提取 `cachedContentTokenCount`。
     缺少可选 thoughts/cache breakdown 在归一化求和时按 0 处理，但事实中保留为
     “未报告”而非伪造的官方 0；不能把缺失的 candidate/input/output 主字段当 0。
3. 官方字段缺失时：
   - prompt 优先使用请求侧 TokenizerRegistry 结果；
   - completion 仅在可安全观察完整响应文本时估算，否则保持 `null`；
   - 只有 prompt 与 completion 都已知时才派生 total，不能把缺失项当作 0。
4. `usage_source` 至少支持 `provider`、`estimated`、`mixed`、`unavailable`。
   流中断导致部分官方值、部分估值时标记为 `mixed`。
5. prompt/completion/total 表示实际或估算的 **provider usage**，不表示被网关拒绝
   的请求大小。未到达上游的本地拒绝三项均为 `null`、来源为 `unavailable`，
   `pricing_status=not_applicable`、`cost_status=not_incurred` 且成本为 0；
   已到达上游却没有完整 usage 时不得假装官方精确值。

### FR-4 内置价格与覆盖规则

1. 提供随代码发布、可审查和测试的静态价表，不在请求路径联网查询价格。首版最低
   覆盖下表列出的标准在线文本 input/output 价格；`openai_compat` 默认不套用
   OpenAI 价格，需显式配置覆盖价。
2. 每条价目包含 provider、明确的模型 ID/受控 alias、input/output USD /
   1M tokens、来源 URL、价表版本、快照日期及 `[effective_from, effective_to)`
   UTC 有效期，以及适用的请求模式/token 阈值。按请求 `started_at` 和已知 usage
   选择有效价目；没有覆盖该时间点的价目即 `unmatched`，已知请求超出适用条件即
   `unsupported`，过期或条件不符的价格不得静默沿用。usage 缺失本身不改变模型
   价目匹配结果，但会使成本 `unavailable`。模型匹配只允许显式 ID、alias 或价表
   声明的受控前缀，不做模糊包含匹配。
3. 价格来源以 2026-07-26 核验的官方页面为准：
   - [OpenAI API Models / Pricing](https://developers.openai.com/api/docs/models)
   - [Anthropic Claude Pricing](https://platform.claude.com/docs/en/about-claude/pricing)
   - [Gemini Developer API Pricing](https://ai.google.dev/gemini-api/docs/pricing)
   - [Gemini Latest Models](https://ai.google.dev/gemini-api/docs/latest-model)
4. 首版最低内置价目如下；单位均为 USD / 1M tokens：

   | Provider | 模型 ID / 显式 alias | UTC 有效期 | Input | Output |
   |----------|-----------------------|------------|------:|-------:|
   | OpenAI | `gpt-5.6-sol`、`gpt-5.6` alias | `[2026-07-26T00:00:00Z, ∞)` | 5.00 | 30.00 |
   | OpenAI | `gpt-5.6-terra` | `[2026-07-26T00:00:00Z, ∞)` | 2.50 | 15.00 |
   | OpenAI | `gpt-5.6-luna` | `[2026-07-26T00:00:00Z, ∞)` | 1.00 | 6.00 |
   | Anthropic | `claude-fable-5` | `[2026-07-26T00:00:00Z, ∞)` | 10.00 | 50.00 |
   | Anthropic | `claude-opus-4-8` | `[2026-07-26T00:00:00Z, ∞)` | 5.00 | 25.00 |
   | Anthropic | `claude-sonnet-5` | `[2026-07-26T00:00:00Z, 2026-09-01T00:00:00Z)` | 2.00 | 10.00 |
   | Anthropic | `claude-sonnet-5` | `[2026-09-01T00:00:00Z, ∞)` | 3.00 | 15.00 |
   | Anthropic | `claude-haiku-4-5-20251001`、`claude-haiku-4-5` alias | `[2026-07-26T00:00:00Z, ∞)` | 1.00 | 5.00 |
   | Gemini | `gemini-3.6-flash` | `[2026-07-26T00:00:00Z, ∞)` | 1.50 | 7.50 |
   | Gemini | `gemini-3.5-flash` | `[2026-07-26T00:00:00Z, ∞)` | 1.50 | 9.00 |
   | Gemini | `gemini-3.5-flash-lite` | `[2026-07-26T00:00:00Z, ∞)` | 0.30 | 2.50 |

   允许实现时补充更多有官方依据的固定价目，但所有新增 ID/alias 都必须出现在版本化
   数据文件和全量测试中。动态 `latest` alias、阶梯价模型和预览模型不做隐式匹配。
   表中三款 GPT-5.6 内置基础价只适用于 prompt tokens `<= 272_000`；从
   `272_001` 起整次请求适用的 2× input / 1.5× output 阶梯价不在首版公式内，
   因而标记 `unsupported`。Model 同时显式覆盖 input/output 时视为用户声明的
   扁平价，不套用内置长上下文阈值；cache、工具和其他附加计费仍需单独支持。
5. `AiModel.input_cost` 与 `output_cost` 分方向独立覆盖内置价：
   `Some(value)` 优先，`None` 才回退；显式 `0` 表示免费。Admin API 拒绝负数、
   NaN 和 Infinity。
6. 标准计价请求的两个方向均解析到有效单价时，`pricing_status=matched`；缺少
   任一方向时为 `unmatched`；计价条件超出首版支持范围时为 `unsupported`；
   未调用 provider 时为 `not_applicable`。请求发生时每个方向的有效单价、来源、
   版本和快照日期分别写入事实，后续修改配置或内置价表不重算历史记录。
7. 首版不实现 provider prompt-cache 的折扣/写入/存储计价；只要官方 usage
   显示非零 cache read/write token，就必须标记 `pricing_status=unsupported`、
   `cost_status=unavailable` 和 `unsupported_pricing`，不得按普通 input 全价
   伪装成已精确计算。标准化 token 仍进入 usage 小计。

### FR-5 成本计算与精度

1. 标准公式为
   `(prompt_tokens × input_price + completion_tokens × output_price) / 1_000_000`。
2. 成本状态至少区分：
   - `calculated`：所需 token 均来自 provider 且价格完整；
   - `estimated`：使用了任一估算 token，但价格与计算项完整；
   - `not_incurred`：确认未调用 provider，或未来由 Kong-Rust 响应缓存直接命中；
   - `unavailable`：已调用 provider，但成本无法完整计算。
3. `unavailable` 必须返回至少一个机器可读原因，枚举至少覆盖
   `missing_prompt_usage`、`missing_completion_usage`、`unmatched_input_price`、
   `unmatched_output_price` 和 `unsupported_pricing`；其 `cost_usd` 为 `null`。
   `not_incurred` 才是已知的 `0`。已匹配价格但 usage 缺失时，定价覆盖仍成立，
   成本可计算覆盖不成立。
4. 金额持久化和聚合使用至少 12 位小数的十进制定点精度；新增的事实、有效价格、
   成本、汇总及 Model 精确金额字段在 Admin API 中以十进制字符串返回，Manager
   不使用 JavaScript 浮点重新汇总。为满足“现有 AI API 保持兼容”，
   `/ai-models` 既有 `input_cost/output_cost` 保留 JSON number 兼容投影，同时
   新增精确 decimal 字符串字段；新代码只使用后者。

### FR-6 异步写入与运行模式

1. PostgreSQL 模式新增 forward-only `ai_usage_logs` 迁移和按查询模式设计的索引。
   请求路径只做有界、非阻塞入队，不等待数据库 I/O；后台按条数或时间批量写入。
2. 健康运行条件下，同一 request ID 恰好落一条并在默认配置下 2 秒内可查询。
   队列满、数据库长期不可用或进程崩溃时允许丢失尚未持久化的数据，以保证代理
   可用性；不得静默丢失，必须记录告警并暴露 enqueued、written、dropped、
   write-failure 指标。优雅关闭时在有界超时内 drain。
3. 独立运行的 DB-less 网关使用有界内存环形缓冲，不写入声明式 `DblessStore`。
   满容量时淘汰最旧记录并计数；`GET /ai-usage*` 返回本节点真实数据，同时明确
   返回/展示 `ephemeral=true`、容量、最早可用时间和“重启清空”提示。若翻页期间
   环形淘汰使既定 snapshot 的待读取记录消失，后续请求返回 HTTP 409 和
   `analytics_snapshot_expired`，不得用不完整页面继续；Manager 提示数据窗口已
   滚动并允许刷新取得新 snapshot。
4. Hybrid analytics 本单明确为**不支持**：Data Plane 不暴露 Admin API，Control
   Plane 不承载代理流量；在没有 DP→CP usage 上传的前提下无法提供可查询事实。
   Control Plane 的 `/ai-usage*` 返回 HTTP 501 和机器可读的
   `analytics_unsupported_in_hybrid`，Manager 展示能力不可用而不是零数据。
   DP→CP 上传与跨节点汇总后续单独立项。

### FR-7 用量明细 API

新增 `GET /ai-usage`：

- 采用 `{ data, offset, next, snapshot }` 响应外形，`size` 默认 100、最大 1000；
- 固定按 `(started_at DESC, id DESC)` 排序。首个请求确定当前最大内部写入序号，
  返回不透明 `snapshot`；`offset` 同时携带该 snapshot 和末项排序键。后续页只查询
  不高于该写入水位的记录，因此同一 snapshot 内相同时间戳无重复或漏项；
  并发新增数据只在刷新并取得新 snapshot 后出现；
- 缺省时间窗为最近 24 小时；显式时间窗必须同时提供 `start`、`end`，且不超过
  90 天。支持 `request_id`、`route_id`、`service_id`、`provider_id`、
  `provider_type`、`requested_model`、`model_group`、`actual_model`、
  `virtual_key_id`、`consumer_id`、`status_code`、`outcome`、`stream`、
  `cache_status`、`usage_source`、`pricing_status`、`cost_status` 过滤；
- `request_id` 按当前网关生成的 32 位小写十六进制值做大小写敏感的精确匹配，
  不支持前缀或模糊查询；API 不提供 workspace 选择器，固定查询默认 workspace；
- 非法/超限时间范围、UUID、request ID、枚举、时区、size 或显式传入
  `workspace_id` 返回 400；
- 返回事实快照字段，不返回 prompt/response body、请求/响应 headers、客户端
  凭据、provider 凭据、Virtual Key 原文或 `key_hash`。

### FR-8 用量汇总 API

新增 `GET /ai-usage/summary`：

1. 与明细 API 复用完全相同的时间边界、过滤器和可选 `snapshot`；未传时生成并
   返回新 snapshot，传入时按同一写入水位查询，支持与明细稳定对账。
2. 返回总请求数、各 `outcome` 数量、成功/失败数、prompt/completion/total token
   的**已知小计**、可计算成本、四种 pricing status 数量、四种 cost status
   数量、平均 e2e、P95 e2e、平均 TTFT 与缓存命中数。
3. 每个 token 小计同时返回 known/unknown request count；页面不得把 SQL/内存聚合
   跳过 `null` 后的结果称为完整总量。覆盖率口径固定为：
   - prompt/completion/total usage coverage = 已尝试上游且对应 token 已知的请求数
     / 已尝试上游请求数；
   - estimated usage ratio = `usage_source` 为 `estimated` 或 `mixed` 的请求数 /
     已尝试上游且至少一个 token 字段已知的请求数；
   - pricing coverage = `pricing_status=matched` 的请求数 / 已尝试上游请求数；
   - cost-calculable coverage = `cost_status` 为 `calculated` 或 `estimated` 的请求数 /
     已尝试上游请求数。
   分母为 0 时比率返回 `null`。`not_incurred` 不进入上述覆盖率分母，但必须
   单独返回数量。
4. 每个汇总请求最多选择一种 breakdown：时间桶、provider、实际 model、
   model group、Virtual Key、Route 或 Service，避免时间与高基数维度笛卡尔积。
   时间桶支持 hour/day 并接受 IANA `timezone`；hour 最长 31 天且最多 744 桶，
   day 最长 90 天且最多 90 桶，DST 切换由指定时区规则处理。分类 breakdown
   默认 Top 10、`limit` 最大 100，另返回 `other` 汇总以保持可对账。
5. 同一 snapshot 下的总计、时间序列和分组结果必须能与相同过滤条件下的明细逐项
   对账；存在 `pricing_status!=matched` 或 `cost_status=unavailable` 记录时，
   成本总计是“可计算部分之和”，并同时返回各状态数量，不能暗示为完整账单。
6. PostgreSQL 与 DB-less 汇总查询均设 5 秒执行时限；超时返回 HTTP 503 和
   `analytics_query_timeout`，不得返回没有明确标识的截断/部分结果。

### FR-9 Kong Manager 调用统计

1. 保持 `/ai-gateway` 的 AI Endpoint 默认入口不变；在 AI Gateway 二级导航中
   新增“调用统计”，页面内提供“用量分析”和“调用日志”。
2. 用量分析默认最近 24 小时，提供 24h / 7d / 30d 和自定义时间范围；首屏至少
   展示可计算成本小计、总请求、prompt/completion/total token 已知小计、估算
   占比与 usage/pricing/cost-calculable coverage。覆盖不足时必须紧邻数值提示
   未知请求数，不能把小计标成完整“总量”。
3. 趋势图可切换成本/token；提供 Top model 与 Top Virtual Key 排行并可下钻。
   Models、Virtual Keys 行增加“查看用量”，携带过滤条件进入同一页面。
4. 调用日志表展示时间、request ID、状态、route/service、provider/model、
   key 的非敏感标识、token/source、分方向价格来源、pricing/cost status 与不可
   计算原因、e2e/TTFT、stream/cache；支持与 API 对齐的过滤（含 request ID
   精确查询）、稳定翻页和从汇总跳转。
5. 过滤条件写入 URL query，刷新、后退和分享后保持；时间由浏览器本地选择转换
   为 UTC，并明确显示当前 IANA 时区。
6. 明确区分 loading、时间窗无调用、过滤无匹配、analytics 不可用和 API 错误：
   错误可重试且不清空条件；未知 token/TTFT/cost 显示 `—`、“未定价”或“无法
   计算”及原因，真实 0 才显示 0。
7. DB-less 页面显示本节点易失性提示；图表具备文字图例、tooltip 和可访问名称，
   排行具备表格语义，窄屏可用；所有新增文案支持 English / 简体中文。

### FR-10 Model 价格交互

1. Models 表单将成本字段明确标为“自定义覆盖价（USD / 1M tokens）”，空值说明为
   使用内置价格。
2. 列表/编辑界面展示有效 input/output 价格、来源（内置 / 覆盖 / 未定价）和内置
   价表快照日期；未知价格不得显示 `$0`。
3. 本需求只展示 Virtual Key 的用量，不更新或假装执行 `budget_used`；预算累计与
   拒绝仍由 REQ-AI-003 交付。

### FR-11 文档与运维可见性

- 更新中英文 AI Gateway guide：价格单位与优先级、成本是标准列表价估算、
  token/pricing/cost 状态、API 示例、Manager 操作、DB-less/Hybrid 限制和数据
  增长风险。
- 更新架构设计与实现记录（在后续门禁完成时），记录事实采集边界、专用查询接口、
  投递语义和隐私边界。
- 异步 writer 的队列满、写入失败、恢复和 DB-less 淘汰必须有可定位日志；日志中
  不包含 prompt、响应正文或凭据。

## 非功能需求

- **性能：** 代理请求路径不得执行 usage SQL 或等待批量 writer；启用事实采集后，
  代表性压力测试的代理 p95 延迟回归不超过 5%，错误率不增加。
- **可靠性：** 正常条件下一请求一事实；降级时代理优先，数据丢失与淘汰必须可
  观测。该保证不是跨进程崩溃的 exactly-once。
- **一致性：** 明细与汇总共享过滤实现和 `[start, end)` 语义；历史成本使用请求时
  价格快照，不随价表变化。
- **隔离：** 首版仅开放默认 workspace 的查询，调用方不能选择或混合 workspace；
  事实保留 `workspace_id` 仅为未来迁移准备。独立 DB-less 明确为本节点数据，
  Hybrid 明确不支持 analytics。
- **隐私：** 仅保存调用元数据。不得保存 prompt、响应正文、Authorization、
  `x-api-key`、provider `auth_config`、Virtual Key 原文或 `key_hash`。
- **兼容：** 未包含 `ai-proxy` 的 Route 行为与性能不变；现有 AI API、Endpoint
  默认入口和日志插件契约保持兼容。
- **可维护：** 内置价表是独立、版本化、带来源和测试的数据资产，价格更新不需要
  修改核心计算逻辑。

## 非目标（本单不做）

- Virtual Key 的 TPM/RPM、`budget_used` 累加、预算拒绝与恢复（REQ-AI-003）。
- Prometheus `ai_metrics` 对齐、TTFT/TPOT 指标导出（REQ-AI-004）；本单只持久化
  已定义口径的 TTFT。
- provider 账单对账、组织折扣、税费、多币种、Batch/Flex/Priority、长上下文
  阶梯价、地域溢价、prompt-cache 读写价、图片/音频/视频、server tool 等附加
  费用。检测到这些条件时保留可用 usage，但定价标为 `unsupported`、成本标为
  `unavailable`，不能只算基础项后宣称成本完整。
- 请求/响应 payload 存储、内容脱敏预览或全文搜索。
- provider attempt 级重试明细、Hybrid DP→CP 上传和跨节点全局汇总。
- 多 workspace 的路由解析、Admin API 上下文、analytics 查询与跨 workspace
  汇总；这些能力交付前，非默认 workspace 的 analytics 明确不支持。
- 自动保留期、归档、导出、删除 API 和历史日志回填；首版数据持续增长风险必须在
  运维文档说明，生命周期治理后续立项。
- 实现 AI 响应缓存。`cache_status` 为 REQ-AI-007 预留真实 hit/miss；当前不得把
  恒 false 伪装为 miss。
- 将该事实表作为财务审计、零丢失计费或预算执行账本。REQ-AI-003 必须采用独立的
  原子、可恢复 accounting 路径做硬预算扣减，只能把本表用于展示与对账。

## 需求分析决策记录（2026-07-26）

1. **事实粒度：** 一个包含 `ai-proxy` 的客户端请求一条，包含上游前拒绝和所有
   失败。首版上游尝试数为 0/1；未来重试使用 request 汇总 + attempt 子事实，
   provider/model 分组成本来自 attempt，不把跨模型成本归到最终模型。
2. **成本定位：** USD 标准在线 token 列表价估算，不宣称供应商账单；未知值为
   `null/unavailable`，只有已知未调用 provider 才是零成本。本表是 best-effort
   analytics，不作为 REQ-AI-003 的硬预算账本。
3. **价格优先级与覆盖：** input/output 分方向执行 Model 显式覆盖 > 内置静态
   价表；显式 0 合法，每个方向的价格、来源和版本分别固化；定价状态与成本状态
   分离，首版最低模型清单按 FR-4 定稿。
4. **Token 来源与归一化：** 使用 `provider / estimated / mixed / unavailable`，
   官方 total 优先，缺失值不按 0 处理；provider 原生 usage 先归一化为可计价
   prompt/completion，并保留 thinking/reasoning/cache breakdown。
5. **持久化语义：** PostgreSQL 为有界非阻塞队列 + 异步批写，健康路径恰好一条；
   异常时 best-effort、代理优先且丢失可观测。
6. **运行模式与 workspace：** 独立 DB-less 使用有界本节点内存环形缓冲并提供
   同一查询 API，重启清空；Hybrid 因 DP 无 Admin API 且无 DP→CP 上传而不支持
   analytics。首版查询固定在默认 workspace，多 workspace analytics 后续立项。
7. **隐私：** 模型调用日志只含事实元数据，不保存 payload、headers 或任何密钥；
   因此不提供仅在前端隐藏内容的“脱敏开关”。
8. **Manager 信息架构：** 新增“调用统计”，不改变 `/ai-gateway` 的 Endpoint
   默认入口；旧的不可达 `Overview.vue` 不作为本需求落点。
9. **时间口径：** API 使用 UTC 与 `[start, end)`；时间桶接受 IANA 时区；e2e
   使用完整网关时钟，流式 TTFT 使用首个可解析 SSE 事件。
10. **数据生命周期：** 首版不自动删除或归档；优先保留历史事实并公开增长风险，
    保留期治理另行立项。

## 方案设计阶段必须落实的技术输入

以下不再是产品范围选择，但需在 `design.md` 中给出具体实现：

1. 早于 AI 策略插件初始化的最小 usage context，以及 access 错误仍能进入 finalize
   的代理生命周期改造；
2. `KongCtx` 的 request ID、完整起始时钟、最终状态/错误类别如何安全传给 collector；
3. 专用 `AiUsageStore` 的 crate 归属、PostgreSQL 批写/查询和 DB-less ring 实现；
4. 迁移表列、十进制类型、索引、稳定游标编码、默认 workspace 约束，以及为
   REQ-AI-005 预留的 request/attempt 扩展点；
5. bounded queue 容量、批次大小、flush 周期、失败/关闭策略和 writer 指标；
6. 内置价表文件格式、alias 规则、测试样本与更新流程；
7. summary SQL、时间桶/DST、P95 算法、统一过滤器、Top/other 聚合和 5 秒超时；
8. Manager 趋势图采用原生 SVG 还是新增轻量依赖；
9. 当前 AI Playwright 中过时的 “management metadata only” 断言应在编码前修正，
   以恢复可信测试基线。

## 验收标准

1. 对同一含 `ai-proxy` Route 分别发起成功非流式、成功流式、认证拒绝、策略拒绝、
   非法请求、上游 4xx/5xx、连接失败与流中断请求；健康运行下每个 request ID
   最终恰好一条事实，早期失败的可空字段不被伪造为 0。
2. OpenAI、Anthropic、Gemini 的官方 usage 被正确标准化并保存；官方 total 不被
   覆盖；Anthropic 三类 input 正确求和，流式用最后一个 delta 的累计 output 且
   不重复累加；Gemini thinking token 计入 completion 且保留 breakdown；OpenAI
   Chat/Responses（含流式）及三类 provider 的非零 cache read/write token 可被
   检测并保留。缺失/混合/估算场景符合 FR-3。
3. FR-4 声明的全部模型 ID、显式 alias、单价与禁止动态 alias 规则均通过表驱动
   测试；Model 分方向覆盖与混合来源、显式零价、未知模型、非零 provider cache
   token、GPT-5.6 prompt 为 272,000/272,001 的阶梯边界、非法负数/NaN/Infinity、
   Sonnet 促销价切换边界和价表升级后历史不漂移均有测试；input/output 各自固化
   正确的 source/version/effective period。
4. `cost_usd` 与状态符合 FR-5，十进制精度满足要求；定价未匹配/不支持或成本
   不可计算的请求不会展示为 `$0`；价格已匹配但 usage 缺失时仍能区分 pricing
   coverage 与 cost-calculable coverage。summary 返回各 pricing/cost status 数量、
   可计算成本小计和 FR-8 定义的全部覆盖率；任何 token `null` 均进入对应
   unknown count。
5. `/ai-usage` 的 snapshot + 倒序游标在相同时间戳和并发新增数据下，对既定
   snapshot 无页内重复或漏项；新增数据在刷新后可见；全部过滤（含 request ID
   精确匹配）、`[start,end)`、90 天上限和 400 参数校验通过；查询固定为默认
   workspace，显式 workspace 选择被拒绝且非默认数据不混入。
6. `/ai-usage/summary` 的 totals、时间桶与 model/Virtual Key 分组，可与相同
   snapshot、相同条件下的明细逐项对账；指定 IANA 时区跨 DST 的 hour/day 桶正确；
   breakdown/Top/桶数上限、`other` 汇总和 5 秒超时错误均可验证，且无未标识的
   部分结果。
7. Manager 的“调用统计”使用真实 Admin API 数据完成时间选择、趋势、排行、下钻、
   稳定日志翻页、Models/Virtual Keys 跳转；loading、两类空态、错误重试、未定价
   和中英文/窄屏/可访问性状态可验证。
8. 请求/响应 payload、headers、provider 凭据、Virtual Key 原文与 `key_hash`
   不出现在表、API、Manager 或 writer 错误日志；删除配置实体后历史事实仍可读。
9. PostgreSQL writer 在默认配置下 2 秒内可查询，代理路径无 DB await；数据库故障
   或队列满不影响代理响应，并产生 dropped/write-failure 指标与告警；压力测试
   p95 延迟回归不超过 5%。
10. 独立 DB-less 环形缓冲支持同一明细/汇总查询，容量淘汰和重启清空语义可验证
    并在 API/Manager 明示；并发淘汰导致 snapshot 失效时返回指定 409 且不混入
    部分结果；Hybrid CP 返回指定 501 错误且 Manager 不显示零数据；PG forward
    migration 注册、真实升级、schema/索引测试通过。
