# Implementation Log: REQ-AI-002 — Token 成本核算与用量事实表

**日期：** 2026-07-26

**需求：** `docs/pm/REQ-AI-002/analysis.md`

**方案：** `docs/pm/REQ-AI-002/design.md`

## 交付结论

REQ-AI-002 为命中启用 `ai-proxy` 插件链的客户端请求新增了一条独立的
best-effort analytics 链路。它在完整请求生命周期结束时生成一条元数据事实，
归一化 OpenAI、Anthropic、Gemini 与 OpenAI-compatible usage，解析请求时有效
价格并使用 Decimal 计算标准 USD 列表价成本，最后通过非阻塞 writer 写入
PostgreSQL 或 DB-less 本节点内存 ring。

该事实表面向运营分析，不是 Provider 账单、财务审计账本或 REQ-AI-003 的硬预算
扣减路径。队列满、数据库长期故障或进程崩溃可能导致未持久化事实丢失，代理可用性
优先，丢失与降级通过状态、指标和日志暴露。

## 实现范围

### 请求生命周期与事实边界

- `kong-core` 增加通用 `RequestLifecycle`、Route/Service/workspace 快照、结构化
  termination hint 和 transport error；request ID、UTC 起点和单调时钟只有一个
  source of truth。
- `kong-plugin-system` 增加同步、不可阻塞的
  `RequestLifecycleObserver`，并在插件短路和错误时记录结构化原因。
- `kong-proxy` 在有效插件链解析后尽早调用 begin observer，在最终 status、
  transport 与发送结果确定后调用 finalize observer，再执行普通插件 log。
- `AiUsageCollector` 只为有效链包含启用 `ai-proxy` 的请求建立草稿。正常响应、
  上游前拒绝、解析/配置/选模错误、上游错误、客户端断开和流中断共享同一个幂等
  收口；无 Route 的 404 和普通非 AI Route 不创建事实。
- 流式响应同时支持 SSE 与 NDJSON，按 media type（忽略大小写和参数）选择解析器；
  增量 UTF-8 解码可重组跨 HTTP chunk 的多字节字符，独立 EOS 会 flush 末尾事件，
  heartbeat 不会被误算为 TTFT。
- Router 与 Route 元数据由同一读写锁发布，生命周期 observer 取得的匹配结果与
  route/workspace 快照不会跨两次热更新；相关缓存锁中毒时恢复旧值继续收口，避免
  已开始的 AI 请求静默缺失事实。
- 当前上游没有 provider retry，一条客户端请求只生成一条事实，
  `attempt_count` 只能为 0 或 1。未来启用跨 Provider retry 时必须增加 attempt
  子事实，不能把跨模型 usage 合并到单一最终模型。

### Usage 归一化、价格与成本

- Provider 官方 usage 优先；未知字段保持 null，只有 prompt 与 completion 都已知
  时才派生 total。请求级来源区分
  `provider/estimated/mixed/unavailable`。
- OpenAI/Responses 保留官方 total，并提取 reasoning、cache read/write；
  Anthropic prompt 包含 input、cache creation 与 cache read，stream output 使用
  最后累计值；Gemini completion 包含 candidate + thinking，并另存 thinking/cache
  breakdown。
- 内置价表位于
  `crates/kong-ai/src/usage/data/model_prices.json`，随代码发布并带 catalog
  version、快照日期、官方来源、有效期和条件。匹配只接受精确 ID、显式 alias 或
  数据文件声明的受控 prefix；`openai_compat` 不继承 OpenAI 价格。
- Input/output 分方向执行 Model 显式覆盖 > 内置价 > unmatched，显式 0 是有效
  免费价。每个覆盖方向的版本同时包含 Model revision、方向与规范价格 hash。
- 单价和成本全程使用 `rust_decimal::Decimal`；PG 使用 `NUMERIC(28,12)`，新增
  API 金额使用固定 12 位字符串。请求事实固化两个方向的价格来源、版本、快照日和
  有效期，价表或 Model 后续更新不会重算历史。
- Pricing 状态区分 `matched/unmatched/unsupported/not_applicable`，cost 状态区分
  `calculated/estimated/not_incurred/unavailable`。Provider cache、非标准 tier、
  built-in tool、非文本模态、附加计价及未覆盖的长上下文阶梯会保留 usage，但不会
  把基础项成本伪装为完整成本。
- `/ai-models` 保留既有 number 类型价格作为兼容投影，同时增加
  `input_cost_decimal/output_cost_decimal` 与服务端计算的
  `effective_pricing`。Manager 使用精确字符串读写覆盖价。

### Store、writer 与运行模式

- migration `006_ai_usage_logs.sql` 将 Model 价格升级为 `NUMERIC(28,12)`，创建
  无配置实体外键的 `ai_usage_logs` 及状态 CHECK、唯一 request ID 和查询索引；
  migration forward-only 并已登记到迁移序列。
- `kong-ai::usage::AiUsageStore` 是专用事实接口，不复用通用 `Dao<T>`。它统一
  snapshot、稳定明细分页、summary 与批写契约；PG 与 Memory 使用相同 DTO、过滤
  和聚合口径。
- PostgreSQL writer 使用有界 `mpsc::try_send`，默认按 256 条或 500ms flush，
  初次写入失败后有 3 次有界退避重试。批写事务取得 advisory lock，使已提交
  `MAX(ingest_seq)` 可作为安全水位；`UNIQUE(request_id)` 提供数据库幂等。
  COMMIT 返回不确定结果时使用独立 `WriteOutcomeUnknown` 语义并继续幂等重试，
  不会在缺少提交证据时误报“确认丢失”。
- DB-less 使用本节点有界 ring，不写入声明式配置 Store。容量淘汰增加 generation；
  snapshot 绑定 high watermark、generation 和 Store instance，淘汰或重启后返回
  409，而不是继续返回可能不完整的页面；实例/淘汰校验先于水位校验，跨重启
  snapshot 不会被误报为普通参数错误。
- Traditional + PostgreSQL 返回持久数据；traditional + `database=off` 返回本
  节点易失数据；Hybrid CP/DP 禁用 collector/writer，不上传、不聚合，Admin 查询
  可达时先返回 501。
- 新增五项配置及启动校验：
  `ai_usage_queue_capacity=8192`、
  `ai_usage_batch_size=256`、
  `ai_usage_flush_interval_ms=500`、
  `ai_usage_shutdown_timeout_ms=5000`、
  `ai_usage_dbless_capacity=10000`。所有值必须大于 0，且 batch 不得大于 queue；
  queue/ring 上限均为 1,000,000，batch 上限为 1,129，避免配置制造不可执行的
  内存或 PostgreSQL bind 规模。

### Admin API、Manager 与可观测性

- `GET /ai-usage` 提供默认 24 小时、最长 90 天的严格过滤查询，以
  `(started_at DESC,id DESC)` 排序，使用 snapshot + offset 稳定分页。
- `GET /ai-usage/summary` 与明细共享事实集合和 snapshot，返回已知 token 小计及
  unknown/coverage、可计算成本小计、完整状态计数、延迟和一个可选 breakdown。
  时间 breakdown 由 PG 与 DB-less 共用一份 `chrono-tz` 计划，支持 DST
  23/25 小时日、重复/缺失午夜、历史秒级 offset 和日期线回拨；分类 breakdown
  返回 Top N 与 other。
- 查询固定 default workspace，拒绝 `workspace_id`；Hybrid 固定 501，
  DB-less snapshot 淘汰固定 409，查询超时/不可用固定 503。查询参数严格拒绝
  未知键、重复键、非法编码和 NUL；500/503 响应不回显 SQL、连接串或内部诊断。
- Kong Manager 新增 `/ai-gateway/usage` 和 `/ai-gateway/usage/logs`，包括
  24h/7d/30d/自定义窗口、KPI、原生 SVG 趋势、模型和 Virtual Key 排行、调用日志
  与详情、URL 过滤状态、稳定翻页，以及 Models/Virtual Keys “查看用量”下钻。
  日志查询只传维度过滤和 snapshot，由服务端恢复其精确时间窗，避免相对时间范围
  在页面切换时产生毫秒漂移；日志查询错误也不会污染已成功加载的统计页状态。
  首次引导仅在 AI 接口首页自动打开；统计父页通过显式 prop 向懒加载子页传递
  controller，避免首次加载或热更新期间注入上下文失效。
- `/status` 增加 `ai_usage_writer`，Prometheus status 输出增加
  `kong_ai_usage_writer_*` counters 和 queue gauges，覆盖入队、写入、重复、丢失、
  写失败、重试、关闭超时与 DB-less 淘汰。

## 数据与隐私边界

事实、API、cursor、Manager 和 writer 诊断日志均不保存或返回 prompt、响应正文、
请求/响应 headers、Authorization、`x-api-key`、Provider `auth_config`、Virtual
Key 明文或 `key_hash`。为定位问题可以保存 Virtual Key 名称和非敏感 prefix。
配置实体删除不会级联删除事实，API 读取请求时快照而不 join 当前配置实体。

既有显式 opt-in 的 `log_payloads` 兼容日志行为未由本需求改变，但其 payload 不会
进入 usage fact。

## 最终验证证据

- `cargo test -p kong-ai --locked`：全量库测试、provider/codec、认证、代理与流式
  集成测试全部通过；其中库测试 85/85、流式回归 14/14。
- `cargo test -p kong-admin --locked`：14 个库测试、41 个 Admin API 兼容测试和
  1 个 schema 测试全部通过，覆盖明细/summary、严格查询、Hybrid 优先级、错误
  隐私及 Model 有效价格投影。
- `cargo test -p kong-proxy --locked`：38 个库测试、12 个 phase-chain 测试和
  26 个 proxy E2E 全部通过；并发热更新回归证明 route/workspace 快照原子一致。
- `cargo test -p kong-db --locked`、`cargo test -p kong-core -p
  kong-plugin-system --locked`、`cargo test -p kong-config --lib --locked` 均通过；
  配置测试 27/27。
- `cargo check -p kong-server --locked` 与 `make check` 通过，证明最终 workspace
  和运行时装配可编译；`git diff --check` 通过。
- 真实 PostgreSQL 16 环境通过全新 bootstrap、`005 -> 006` 升级和 migration
  注册验证；在独立临时数据库上执行 PG Store summary 聚合测试通过，验证
  migration 后的真实 SQL 路径。
- Kong Manager `pnpm lint`、`pnpm build` 通过；usage Playwright E2E 7/7 通过，
  覆盖统计页、日志页、过滤/翻页/详情和来源页下钻。lint/build 仅保留既有
  Header 组件与 Vite/Monaco 分包告警。
- 运行中的真实 Admin API 已验证 summary 返回 1 条请求事实，随后只携带 snapshot
  的日志查询返回 HTTP 200 和同一条事实。真实 Chrome 已在运行中的 Manager 完成
  “统计 → 日志 → 统计”点击巡检，日志页和返回后的统计页均展示同一条真实事实；
  应用内浏览器的本地标签连接不稳定，未作为最终人工验收依据。
- 仓库级 `make lint` 被既有 `kong-core` 六处 `derivable_impls` 基线告警阻断；
  首个失败发生在本需求未修改的 `models/common.rs`、`traits/dao.rs` 和 `lib.rs`，
  未为消除基线而扩大本需求 diff。全仓格式检查同样存在大量既有格式差异，本需求
  新增核心文件的局部 rustfmt 检查及最终差异空白检查均通过。

## 剩余风险

- migration 和真实 PG Store 已在 PostgreSQL 16 验证，但尚缺 PostgreSQL 15
  兼容环境的同套证据。
- 真实 PG 已覆盖 migration 和 summary 聚合主路径，尚未形成一套同时覆盖批写
  COMMIT 不确定结果、advisory lock 争用、稳定翻页、Top/Other、DST 与 query
  timeout 的故障注入端到端矩阵。
- 尚无代表性压力测试证明启用 collector 后代理 p95 回归不超过 5%；高并发下队列
  满、数据库恢复和关闭 drain 仍需故障注入。
- PG summary 的大窗口和高基数过滤需要 `EXPLAIN (ANALYZE, BUFFERS)` 证实索引与
  内存占用符合预期。
- 首版没有自动 retention、partition、archive、export 或 delete API，
  `ai_usage_logs` 会持续增长；90 天 API 查询上限不会删除更早数据。上线前应建立
  表/索引容量、writer drop 和查询延迟监控，并另立数据生命周期需求。
- 本实现不支持 DB-less 跨节点汇总、Hybrid DP→CP 上传、Provider 发票对账、折扣/
  税费/多币种、Provider cache/工具/非文本等附加计价，也不执行 Virtual Key
  `budget_used` 或硬预算拒绝。
