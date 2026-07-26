# Kong-Rust 需求单列表（Requirement Backlog）

> 本文档是项目需求管理的唯一入口。所有功能开发按需求单（REQ）维度逐个交付，
> 每个需求单必须依次经过三个阶段：**需求分析 → 方案设计 → 编码实现**。
> 交付流程规范见 [AGENTS.md](../../AGENTS.md) 的 Requirement Delivery Workflow 章节。

## 使用说明

- **状态流转**：`📋 待启动` → `📝 需求分析` → `🎨 方案设计` → `🔨 编码实现` → `✅ 已完成`（阶段完成即更新本表）
- **文档结构**：每个需求一个文件夹 `docs/pm/REQ-XXX/`——需求分析定稿写入 `REQ-XXX/analysis.md`，方案设计写入 `REQ-XXX/design.md`；本文件中需求单只保留摘要与链接。未启动的需求以范围草案形式留在本文件内，进入需求分析阶段时抽取为独立文档
- **前端同步原则**：功能需求的 Kong Manager 支持与后端能力**同一需求单内交付**；纯后端需求必须在「前端范围」显式标注 `无（豁免：<原因>）`
- **Effort 口径**：沿用 TODOS.md 惯例，S=几小时 / M=1-3 天 / L=1-2 周 / XL=2-4 周（AI 辅助开发时间）
- 实现层任务状态同步到 `docs/tasks.md`，实现完成后按 AGENTS.md 要求补 implementation log

## 需求总览

| ID | 需求 | 优先级 | 状态 | Effort | 依赖 |
|----|------|--------|------|--------|------|
| [REQ-AI-001](#req-ai-001) | Virtual Key 运行时认证 | P0 | ✅ 已完成 | M | 无 |
| [REQ-AI-002](#req-ai-002) | Token 成本核算与用量事实表 | P0 | ✅ 已完成 | L | 无 |
| [REQ-AI-003](#req-ai-003) | Virtual Key 配额与预算控制 | P0 | 🧪 主链路完成 / 发布验证中 | L | 001, 002 |
| [REQ-AI-004](#req-ai-004) | AI 可观测性对齐（Prometheus + TTFT） | P1 | 📋 待启动 | S | 无 |
| [REQ-AI-005](#req-ai-005) | 模型健康回报与故障重试 | P1 | 📋 待启动 | M | 无 |
| [REQ-AI-006](#req-ai-006) | AI 策略插件 schema 补全与 Manager 表单 | P1 | 📋 待启动 | S | 003 |
| [REQ-AI-007](#req-ai-007) | AI 响应缓存 MVP（精确匹配） | P1 | 📋 待启动 | M | 006 |
| [REQ-AI-008](#req-ai-008) | 语义缓存（向量相似度） | P2 | 📋 待启动 | L | 007 |
| [REQ-AI-009](#req-ai-009) | 分布式实时配额（Redis）与内存限流治理 | P1 | 📋 待启动 | L | 003 |
| [REQ-AI-010](#req-ai-010) | Prompt Guard 增强 | P1 | 📋 待启动 | S | 无 |
| [REQ-AI-011](#req-ai-011) | MCP Gateway（Phase 3） | P2 | 📋 待启动 | L | 009 建议先行 |
| [REQ-AI-012](#req-ai-012) | Agent Gateway（Phase 4） | P3 | 📋 待启动 | XL | 011 |
| [REQ-AI-013](#req-ai-013) | AI 用量事实外部存储与生命周期治理 | P1 | 📋 待启动 | L | 002 |

**里程碑规划：**

- **M1 — Key 治理与成本闭环**（Phase 2b 真正完成）：REQ-AI-001 → 002 → 003，穿插 quick win 004 / 005。交付后 Virtual Key 从「管理面记录」变为具备明确 local 能力边界的单节点 enforcement，摘掉前端警告 banner。
- **M1E — AI 配额与用量存储规模化**：REQ-AI-009 + 013。完成后仅表示 RPM/TPM 具备多副本 Redis 配额、usage/log 具备外部存储能力；不代表 Hybrid、预算账本或整个数据面已经完成水平扩展。
- **M2 — 策略插件功能与配置闭环**：REQ-AI-006 → 007 → 010。多副本配额与外部用量存储能力仍以 M1E 为准。
- **M3 — 进阶与新子网关**：REQ-AI-008、011、012。

---

## REQ-AI-001

### Virtual Key 运行时认证（Virtual Key Runtime Authentication）

- **优先级 / Effort：** P0 / M
- **状态：** ✅ 已完成（2026-07-25）
- **依赖：** 无
- **📄 需求分析：** [REQ-AI-001/analysis.md](REQ-AI-001/analysis.md)（9 条 FR、7 条验收标准、3 项已确认决策）
- **📄 方案设计：** [REQ-AI-001/design.md](REQ-AI-001/design.md)
- **📄 实现记录：** [task-15-4](../implementation-logs/task-15-4_2026-07-25_virtual-key-auth.md)（tasks.md 15.4）

**摘要**：`ai_virtual_keys` 的 DB / Admin / 前端三层已交付但运行时零接入。新增独立 `ai-key-auth` 插件（priority 774）：凭证提取（Bearer / x-api-key / 自定义 header）→ SHA256 查 key → enabled/expires/allowed_models（支持前缀通配符）校验 → ctx 注入 `AiAuthContext` + `consumer_id`；错误体按客户端协议自适应；CUD/rotate 接入缓存失效通道（≤1s 生效）。前端同单交付：向导认证开关、VirtualKeys 页文案更新、Playground 带 key 调试。

---

## REQ-AI-002

### Token 成本核算与用量事实表（Cost Accounting & Usage Facts）

- **优先级 / Effort：** P0 / L（需求分析后由 M 调整）
- **状态：** ✅ 已完成（2026-07-26）
- **依赖：** 无（`virtual_key_id` 字段可空，REQ-AI-001 完成后自动携带）
- **📄 需求分析：** [REQ-AI-002/analysis.md](REQ-AI-002/analysis.md)（11 条 FR、10 条验收标准、10 项已定稿决策）
- **📄 方案设计：** [REQ-AI-002/design.md](REQ-AI-002/design.md)
- **📄 实现记录：** [REQ-AI-002 — Token 成本核算与用量事实表](../implementation-logs/req-ai-002_2026-07-26_ai-usage-analytics.md)

**摘要**：为包含 `ai-proxy` 的每个客户端请求形成一条请求级元数据事实，覆盖上游前拒绝、
解析失败、成功、上游错误与流中断；provider usage 优先，缺失值显式标记估算/混合/不可用。
内置 OpenAI / Anthropic / Gemini 静态价表可被 Model 分方向覆盖，未知价格为未定价而非
零成本，并把生效单价与版本固化到历史事实。PostgreSQL 走有界非阻塞队列异步批写，
DB-less 使用本节点易失环形缓冲；提供共享过滤口径的明细/汇总 API。Manager 新增“调用统计”
（用量分析 + 元数据调用日志），保持 AI Endpoint 为默认入口，不保存 prompt、响应正文或凭据。

---

## REQ-AI-003

### Virtual Key 配额与预算控制（Quota & Budget Enforcement）

- **优先级 / Effort：** P0 / L（需求分析后由 M 调整）
- **状态：** 🧪 核心编码与主链路验证完成 / 发布档位验证中（2026-07-26；
  尚未验收完成）
- **依赖：** REQ-AI-001（key 身份，已完成）、REQ-AI-002（价格/成本口径，
  已完成；usage facts 仍只供 analytics）
- **📄 需求分析：** [REQ-AI-003/analysis.md](REQ-AI-003/analysis.md)
  （11 条 FR、16 条验收标准、13 项已定稿决策）
- **📄 方案设计：** [REQ-AI-003/design.md](REQ-AI-003/design.md)
- **📝 实现记录：**
  [req-ai-003_2026-07-26_quota-budget-enforcement.md](../implementation-logs/req-ai-003_2026-07-26_quota-budget-enforcement.md)

**摘要**：在同时挂载 `ai-key-auth`、`ai-proxy` 与
`ai-rate-limit(limit_by=virtual_key)` 的 Endpoint 上，以认证后的稳定 key ID
执行 key 自身的本地 60 秒 TPM/RPM；非法枚举、缺失身份或无 ai-proxy 的无效链
fail closed，不再串入 global。预算定义为 PostgreSQL traditional 模式下的 USD
生命周期消费截止线：
独立 Decimal ledger 以内部 request ID 幂等结算并原子累计 `budget_used`，已耗尽
请求前置 403，调高后立即恢复。由于 completion 成本在响应后确定，已准入/并发
请求允许结算后超过 100%，下一请求开始拒绝，不宣称严格零超支。DB-less 仅支持
易失本地配额，预算与 Hybrid 明确 unsupported。Manager 同单交付进度、预警/耗尽
徽章和可验证的待挂载/本地生效状态。首版 Memory/PostgreSQL 通过后端无关的
`RateLimitStore` 与 `BudgetStore`/Admin/governance traits 装配，不把本机/PG
私有语义固化到插件；Redis 多节点配额归 REQ-AI-009，外部 usage/log 存储归
REQ-AI-013，完整策略发布向导仍归 REQ-AI-006。

---

## REQ-AI-004

### AI 可观测性对齐（Prometheus ai_metrics + TTFT 导出）

- **优先级 / Effort：** P1 / S（quick win）
- **状态：** 📋 待启动
- **依赖：** 无

**背景与价值**

具体 bug：Rust 侧 ai-proxy 往 `log_serialize` 写 `ai.{proxy,usage,latency}`，而 Lua Prometheus exporter 读 Kong 官方 `ai_metrics.*` 结构——键名不匹配导致 `ai_llm_requests_total` 等 4 个已定义指标**永远为空**。TTFT 已记录但不导出。

**需求范围**

- 后端：`log_serialize` 输出对齐 Kong 官方 `ai_metrics` 结构（是否双写 `ai.*` 保兼容在方案设计定）；`ai_llm_requests/tokens/cost/latency` 指标出数；导出 TTFT / TPOT；补限流拒绝、缓存命中计数埋点
- 前端：无（豁免：指标由 Prometheus/Grafana 消费，Manager 侧数据展示由 REQ-AI-002 用量页承担）
- 文档：guide 双语版可观测性章节

**验收标准**

1. 发起 AI 请求后 `/metrics` 可抓到非零的 `ai_llm_*` 指标（含 provider/model 标签）
2. file-log / http-log 等日志插件输出包含 `ai_metrics` 结构与 TTFT
3. 与 Kong 官方 ai_metrics 消费端（Prometheus 插件）兼容性验证通过

---

## REQ-AI-005

### 模型健康回报与故障重试（Balancer Health Feedback & Retries）

- **优先级 / Effort：** P1 / M
- **状态：** 📋 待启动
- **依赖：** 无

**背景与价值**

`ModelGroupBalancer::report_success/report_failure` 与冷却逻辑已完整实现但**代理链从不调用**（死代码）；`retries` 配置读取后不使用。当前宣传的「多模型 Fallback」只在选路时过滤，运行时没有健康反馈闭环，provider 故障时流量不会自动避开。

**需求范围**

- 后端：
  1. 代理链在上游 429/5xx/连接失败时调用 `report_failure`（429 触发 10s 冷却、连续 3 次失败 30s 冷却），成功时调用 `report_success`
  2. `retries` 生效：非流式（及流式首字节前）失败自动切换同组下一 target 重试，语义对齐 Kong `retries` 配置
  3. Admin API 暴露模型组 balancer 健康快照（冷却中 / 健康 / 失败计数）
- 前端：Models 页展示健康状态徽章（健康 / 冷却中），基于健康快照 API
- 文档：guide 双语版 fallback 章节更新为真实行为

**验收标准**

1. 单 provider 持续 5xx 时，流量在冷却窗口内自动落到下一优先级 target，客户端无感知
2. 429 触发冷却可通过健康快照 API 观测到
3. 重试成功的请求返回正常响应；重试耗尽返回最后一次上游错误
4. 集成测试模拟 provider 故障覆盖冷却、恢复、重试三条路径

---

## REQ-AI-006

### AI 策略插件 schema 补全与 Manager 配置表单

- **优先级 / Effort：** P1 / S
- **状态：** 📋 待启动
- **依赖：** REQ-AI-003（复用 `ai-rate-limit` schema 与校验）

**背景与价值**

当前 `ai-rate-limit` / `ai-cache` / `ai-prompt-guard` 三个插件在 Admin schema
中的 config fields 都是空数组，Kong Manager 无法提供可用的策略配置体验。
REQ-AI-003 已交付 `ai-rate-limit` 的基础 schema 与通用 schema-driven 配置；
本需求补齐其专用 UX、另外两个 schema 和发布向导闭环。

**需求范围**

- 后端：补全 `ai-cache` / `ai-prompt-guard` 的完整 config fields；复用
  REQ-AI-003 已交付的 `ai-rate-limit` schema/校验，不重复定义
- 前端：三个插件提供专用、易用的表单体验；Endpoint 发布向导支持勾选挂载限流 /
  Prompt Guard 策略（发布时一并创建插件）
- 文档：guide 双语版插件配置章节

**验收标准**

1. Manager 中三个插件表单可渲染、校验、保存，往返（保存→回显）一致
2. schema 校验与插件运行时解析一致（非法配置在保存时被拒）
3. 通过发布向导挂载的策略插件实际生效

---

## REQ-AI-007

### AI 响应缓存 MVP（精确匹配 + 命中短路）

- **优先级 / Effort：** P1 / M
- **状态：** 📋 待启动
- **依赖：** REQ-AI-006（配置表单）；与 REQ-AI-009 共用存储后端基建（方案设计统一）

**背景与价值**

当前 `ai-cache` 只算 SHA256 缓存键，`cache_hit` 恒为 false、`log()` 为空实现——**本质上不缓存任何东西**。先交付精确匹配缓存闭环，语义缓存（REQ-AI-008）在其上叠加。

**需求范围**

- 后端：
  1. 缓存存储抽象 trait + 内存（moka）后端 MVP，Redis 后端可选（连接配置与 REQ-AI-009 共用）
  2. 命中短路：access 阶段查到缓存直接返回响应 + `X-Cache-Status: Hit`，未命中标记 Miss
  3. 响应回写：非流式响应在 log/body 阶段回写；流式响应聚合后回写（是否首版支持流式回写在方案设计定）
  4. TTL、条目大小上限、`extract_cache_key` 支持 content-part 数组
- 前端：ai-cache 配置表单（TTL / 策略 / skip header）；用量分析页展示缓存命中率
- 文档：guide 双语版缓存章节更新为真实行为

**验收标准**

1. 相同请求第二次命中缓存，不打上游，延迟显著低于首次
2. skip header 生效；TTL 过期后重新回源
3. 命中率通过指标 / 用量页可观测
4. 缓存后端故障时降级为直接回源（不影响可用性）

---

## REQ-AI-008

### 语义缓存（Embedding + 向量相似度检索）

- **优先级 / Effort：** P2 / L
- **状态：** 📋 待启动
- **依赖：** REQ-AI-007

**背景与价值**

精确匹配缓存对改写一个字的 prompt 即 miss。语义缓存按向量相似度命中，是 AI 网关的标志性能力。战略文档已定技术方向：embedding 优先调外部 API（OpenAI text-embedding-3-small），向量索引优先 usearch 内嵌（备选 hora / 线性扫描）。

**需求范围**

- 后端：embedding 生成客户端（带缓存与降级）、usearch 向量索引集成、相似度阈值配置、与精确缓存的分层查找（先精确后语义）
- 前端：ai-cache 表单增加语义开关、相似度阈值、embedding 模型配置
- 文档：guide 双语版语义缓存章节

**验收标准**

1. 语义相近但文本不同的 prompt 在阈值内命中缓存
2. embedding API 不可用时自动降级为精确匹配（不阻塞请求）
3. 阈值调整即时生效；命中率按精确 / 语义分开统计

---

## REQ-AI-009

### 分布式实时配额（Redis 后端）与内存限流治理

- **优先级 / Effort：** P1 / L
- **状态：** 📋 待启动
- **依赖：** REQ-AI-003（配额语义与 `RateLimitStore` 契约）；Redis 连接、TLS、
  认证、池化和健康检查基建与 REQ-AI-007 统一，缓存与限流领域接口不共用

**背景与价值**

REQ-AI-003 首版配额状态仍在单进程内存：多节点各算各的、重启清零，不能作为
企业多副本的全局额度。现有内存后端还存在 DashMap 只重置窗口但不删除 key 的
高基数增长风险，窗口长度也固定为 60 秒。本需求在不改变 REQ-AI-003
准入/结算/响应契约的前提下增加 Redis backend。

**需求范围**

- 后端：
  1. 为 REQ-AI-003 的异步 `RateLimitStore` 增加 Redis backend；首阶段保持相同的
     `fixed_first_hit/60s` WindowSpec，不同时引入 sliding/multi-window
  2. 使用 Lua script/Redis Function 让 `admit` 与 `settle` 分别在单次原子操作中
     完成 RPM/TPM 联合准入或结算、request/operation ID 幂等和
     reservation/window snapshot；不能用多个独立 `INCRBY` 拼接出看似原子的结果
  3. 定义版本化 key namespace、Redis Cluster hash tag、backend 权威时钟、TTL
     与 reservation/幂等结果保留期，至少覆盖
     `window + 最大请求生命周期 + settlement 重试宽限期`，支持长流跨窗口、
     迟到 settle 和配置热更新
  4. 明确 standalone/Sentinel/Cluster 支持矩阵、TLS/ACL/secret reference、
     连接池、超时、重试、脚本版本、failover/reshard 与 local→Redis 切换
  5. Store 返回结构化 unavailable/timeout/outcome-unknown/overloaded 错误；
     outcome unknown 先按 request ID 幂等重查。fail-open/fail-closed 显式配置，
     默认保持 REQ-AI-003 的 fail-closed；fail-open 不得伪造配额头，也不得无提示
     切回本地并继续宣称全局一致
  6. 内存 backend 完成过期清理、容量指标与窗口长度配置；duration/algorithm
     修改从下一 generation 生效，不重置活动窗口
  7. 定义 local→Redis/backend 切换时当前窗口的迁移或显式边界，不能无提示重置
     额度；预算 preflight 失败后、Redis refund 前进程退出时，reservation 必须可
     由 request/operation ID 恢复或按有界补偿流程收口
- 前端：Manager 展示 `local/distributed/degraded/unavailable` 与当前 backend；
  Redis 凭据和连接仍由 `kong.conf` 管理，不在页面编辑
- 文档：guide 双语版限流章节、部署文档 Redis 配置说明

**验收标准**

1. 两个及以上网关节点共享同一 Redis 时，RPM/TPM 联合计数、remaining/reset
   与单节点 contract suite 一致，任一维度失败不发生部分扣减
2. 脚本已执行但响应丢失时不会重复扣减；可按 request ID 恢复，同一 settlement
   operation 重放只生效一次
3. Redis 宕机、超时、failover/reshard 时按显式降级策略行为正确，Manager/Status
   不会把 local fallback 标成 distributed
4. Cluster 模式所有联合原子数据位于同一 hash slot；节点时钟偏移不改变窗口边界
5. 多节点高并发、高 key 基数和长流跨窗口 soak 测试达到方案设计基线，指标不使用
   key/request 等高基数标签
6. 内存 backend 长时间高基数运行内存占用有界，过期清理和容量指标生效
7. 预算失败后 refund 前进程退出可恢复，不永久多扣；local→Redis 切换不会无提示
   重置当前窗口，切换语义与 Manager 状态一致

---

## REQ-AI-010

### Prompt Guard 增强

- **优先级 / Effort：** P1 / S
- **状态：** 📋 待启动
- **依赖：** 无

**背景与价值**

安全缺陷：guard 只检查 `messages` 数组，`llm/v1/responses` 的 `input` 字段完全不检查——responses 路由下护栏**形同虚设**（此项为 P1 的主因）。另有：只检查 user role、正则每请求重新编译（高 QPS CPU 开销）、无请求体时返回 500 类错误。

**需求范围**

- 后端：
  1. 支持检查 `v1/responses` 的 `input` 字段（含 content-part 数组）
  2. 正则编译缓存（按配置指纹）
  3. 可配置检查的 role 范围（默认 user，可扩展 system/assistant/tool）
  4. 无请求体时行为修正（放行或 400，不再 500）
- 前端：Prompt Guard 规则编辑器优化（规则列表编辑 + 正则在线测试）
- 文档：guide 双语版 Prompt Guard 章节
- 说明：语义级注入检测（分类器/embedding）不在本单范围，后续单独立项

**验收标准**

1. responses 路由下 deny 规则可拦截 `input` 中的违规内容
2. 高 QPS 下正则不重复编译（基准对比）
3. role 范围配置生效；空请求体不再返回 5xx

---

## REQ-AI-011

### MCP Gateway（Phase 3）

- **优先级 / Effort：** P2 / L
- **状态：** 📋 待启动（需先做需求分析细化拆单）
- **依赖：** 建议 REQ-AI-009 先行（复用通用限流基建）

**背景与价值**

战略路线 Phase 3。对标 IBM ContextForge、Microsoft MCP Gateway、Higress。战略定位为「基础对齐」，为 Agent Gateway 做基础设施准备，不做过度投入。

**需求范围（粗粒度，启动时细化拆为子需求单）**

- 后端：新建 `kong-mcp` crate；MCP Server 注册 / 发现（Admin API + `/mcp/tools` 聚合发现）；JSON-RPC over HTTP/SSE 协议代理（不依赖第三方 SDK）；per-tool 认证与调用频率限制；工具调用可观测性
- 前端：MCP Server 管理页（注册 / 工具列表 / 健康状态）与功能同步交付
- 文档：MCP Gateway guide 双语版

**验收标准（里程碑级）**

1. 注册的 MCP Server 可通过网关被 MCP 客户端正常发现并调用工具（含 SSE 流式）
2. per-tool 限流与认证生效
3. 工具调用延迟 / 错误率指标可观测

---

## REQ-AI-012

### Agent Gateway（Phase 4）

- **优先级 / Effort：** P3 / XL
- **状态：** 📋 待启动（需先做需求分析细化拆单）
- **依赖：** REQ-AI-011

**背景与价值**

战略路线 Phase 4，差异化竞争力所在（市场上唯一 API+LLM+MCP+Agent 四子网关 Rust 原生项目的收官一环）。对标 Google A2A、LangGraph Platform。A2A 协议仍在演进，本需求最晚启动。

**需求范围（粗粒度，启动时细化拆为子需求单）**

- 后端：新建 `kong-agent` crate；Agent 注册与发现（Agent Card）；A2A 协议代理（SSE 流式任务）；per-agent 认证 / 授权；基于 session ID 的会话状态路由（复用 consistent-hashing）
- 前端：Agent 拓扑图、调用链路追踪页与功能同步交付
- 文档：Agent Gateway guide 双语版

---

## REQ-AI-013

### AI 用量事实外部存储与生命周期治理

- **优先级 / Effort：** P1 / L
- **状态：** 📋 待启动
- **依赖：** REQ-AI-002（规范化事实、成本口径与 Admin API）

**背景与价值**

REQ-AI-002 当前把 usage fact 写入未分区、无 retention 的 PostgreSQL
`ai_usage_logs`，多节点 writer 还通过同一 advisory lock 串行批次；DB-less
使用本节点有界内存。该默认档位适合开发和中小规模部署，但海量 append-only
调用事实会与配置、Admin 查询和预算账本争用数据库资源，不能作为企业部署的唯一
存储路径。

**需求范围（草案）**

- 后端：
  1. 将现有 `AiUsageStore` 拆分为可独立装配的 `AiUsageSink` 与
     `AiUsageQueryBackend`，write sink 与 query backend 独立配置；部署模式、
     持久/易失属性和物理 backend ID 分离，通过 capability 协商 snapshot、
     稳定分页与 summary
  2. 定义带 schema version、确定性 event/request ID、Decimal 字符串和隐私约束的
     稳定事件 envelope；首期至少交付一个可查询的外部分析拓扑，在需求分析中从
     Elasticsearch/OpenSearch、ClickHouse 或等价存储中基于压测选定。若使用
     Kafka 等消息管道，必须同时包含到可查询外部存储的消费链，PG 只能作为有期限
     的迁移桥
  3. 外部写入支持异步 bulk、条数/字节上限、压缩、并发、结构化逐项 ACK、幂等
     重放、结果不确定、独立有界队列和可选 spool/DLQ，绝不等待在代理热路径；
     endpoint、TLS、认证和 secret reference 可配置，永久失败/poison event 隔离
     后不得阻塞整批
  4. 提供 retention、index rollover/partition、容量告警、查询可见性 SLA、
     backend lag/健康指标，以及 PG→外部双写、对账和 query backend 切换方案
  5. Manager/Admin DTO 与过滤口径保持稳定；backend 私有 snapshot 使用 opaque
     token，不能把 PG sequence 或 Memory ring generation 固化为公共协议；
     backend 切换后旧 cursor 必须有版本化兼容或确定性失效错误
- 前端：Manager 继续通过统一 Admin API 查询，并展示 backend、持久/易失状态、
  数据新鲜度、retention 和降级/积压告警；不直接访问外部存储
- 文档：外部 backend 部署、容量规划、retention、迁移/回滚与故障排查指南

**验收标准（草案）**

1. 多节点持续与突发写入压测达到设计基线，代理热路径不执行外部存储 await，
   外部 backend 故障不影响预算正确性
2. ACK 丢失、429/超时和 bulk 部分失败只重试必要事件，按 event/request ID 对账
   后没有重复统计或静默丢失；event 当前版/前一版兼容，未知新增字段可忽略，
   poison event 被隔离且不阻塞其他事件
3. 外部 query backend 通过与 PG 相同的过滤、Decimal、summary 和稳定分页
   contract suite；首期正常查询不再依赖 PG，其他不支持的 capability 明确返回，
   不伪造完整 snapshot
4. retention/index/partition 与容量告警生效，状态和指标可观测 queue、lag、
   retry、drop、DLQ/spool，且不含事实正文、凭据或高基数 ID 标签
5. 双写迁移按时间窗和 request ID 完成请求数、token、成本对账，切换查询后端
   不丢历史、不重复，支持明确回滚
6. sink 与 query backend 使用不同实现的组合通过 contract test；状态/API 分别
   返回部署模式、durability 和 backend ID，切换前生成的 cursor 按版本化规则
   继续可用或返回确定性失效错误

---

## 需求单模板（新增需求时复制）

新增需求时先在本文件按下述模板登记草案；进入需求分析阶段后，将详细内容抽取到
`docs/pm/REQ-XXX/analysis.md`（需求分析）与 `docs/pm/REQ-XXX/design.md`（方案设计），
本文件缩减为摘要 + 链接（参考 REQ-AI-001 的形式）。

```markdown
## REQ-XX-NNN

### 标题（English Title）

- **优先级 / Effort：** PN / S|M|L|XL
- **状态：** 📋 待启动
- **依赖：** REQ-...
- **📄 需求分析：** REQ-XX-NNN/analysis.md（进入 📝 阶段后创建）
- **📄 方案设计：** REQ-XX-NNN/design.md（进入 🎨 阶段后创建）

**背景与价值**

为什么做、现状缺口、不做的代价。

**需求范围（草案）**

- 后端：...
- 前端：...（或「无（豁免：<原因>）」）
- 文档：...

**验收标准（草案）**

1. 可验证的行为断言...

**容量与规模化约束（数据面需求必填）**

- 目标 QPS/并发、节点数、租户/key 基数、事件大小与增长、retention；
- 热路径 I/O、状态归属和水平一致性、后端抽象与 capability；
- 队列/连接池/批量/超时、背压/降级、低基数指标；
- 单节点、多节点、高基数与后端故障的负载验证口径。
```

`analysis.md` 的章节结构参考 [REQ-AI-001/analysis.md](REQ-AI-001/analysis.md)：
背景与价值 / 用户故事 / 现状事实与约束 / 功能需求（FR）/ 非功能需求 / 非目标 /
关联缺陷 / 决策记录 / 容量与规模化约束 / 验收标准。
