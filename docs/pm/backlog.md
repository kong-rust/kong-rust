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
| [REQ-AI-002](#req-ai-002) | Token 成本核算与用量事实表 | P0 | 📋 待启动 | M | 无 |
| [REQ-AI-003](#req-ai-003) | Virtual Key 配额与预算控制 | P0 | 📋 待启动 | M | 001, 002 |
| [REQ-AI-004](#req-ai-004) | AI 可观测性对齐（Prometheus + TTFT） | P1 | 📋 待启动 | S | 无 |
| [REQ-AI-005](#req-ai-005) | 模型健康回报与故障重试 | P1 | 📋 待启动 | M | 无 |
| [REQ-AI-006](#req-ai-006) | AI 策略插件 schema 补全与 Manager 表单 | P1 | 📋 待启动 | S | 无 |
| [REQ-AI-007](#req-ai-007) | AI 响应缓存 MVP（精确匹配） | P1 | 📋 待启动 | M | 006 |
| [REQ-AI-008](#req-ai-008) | 语义缓存（向量相似度） | P2 | 📋 待启动 | L | 007 |
| [REQ-AI-009](#req-ai-009) | 分布式限流（Redis）与内存限流治理 | P2 | 📋 待启动 | M | 无 |
| [REQ-AI-010](#req-ai-010) | Prompt Guard 增强 | P1 | 📋 待启动 | S | 无 |
| [REQ-AI-011](#req-ai-011) | MCP Gateway（Phase 3） | P2 | 📋 待启动 | L | 009 建议先行 |
| [REQ-AI-012](#req-ai-012) | Agent Gateway（Phase 4） | P3 | 📋 待启动 | XL | 011 |

**里程碑规划：**

- **M1 — Key 治理与成本闭环**（Phase 2b 真正完成）：REQ-AI-001 → 002 → 003，穿插 quick win 004 / 005。交付后 Virtual Key 从「管理面记录」变为「企业级 key enforcement」，摘掉前端警告 banner。
- **M2 — 策略插件完备**：REQ-AI-006 → 007 → 010 → 009。四个 AI 插件全部达到生产可用。
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

- **优先级 / Effort：** P0 / M
- **状态：** 📋 待启动
- **依赖：** 无（`virtual_key_id` 字段可空，REQ-AI-001 完成后自动携带）

**背景与价值**

`calculate_cost` 是 15 行纯函数且生产代码**零调用点**；无内置定价表、无任何 usage 持久化表、无 AI analytics API。没有事实表，成本仪表盘、预算控制（REQ-AI-003）、按 key 计费都无从谈起。这是轨道 C「成本仪表盘」的数据地基。

**需求范围**

- 后端：
  1. 内置主流模型定价表（静态数据 + `ai_models.input_cost/output_cost` 可覆盖），标注价格快照日期
  2. 新表 `ai_usage_logs`（forward-only 迁移）：时间、route/service、provider/model、virtual_key_id/consumer_id、prompt/completion/total tokens、token 来源（官方 usage / 估算）、cost、e2e 延迟、TTFT、状态码、是否流式、缓存状态
  3. log 阶段异步批量落库（mpsc + 批量 insert，不阻塞代理路径）；DB-less 模式降级策略在方案设计阶段定（内存环形缓冲 / 仅日志输出）
  4. Admin API：`GET /ai-usage`（分页 + 过滤）、`GET /ai-usage/summary`（按 model / virtual key / 时间窗聚合）
- 前端：AI Gateway Overview 增加成本卡片；新增「用量分析」页（消耗趋势图、按 model / virtual key 下钻、Top 排行）；模型调用日志列表页（可脱敏）
- 文档：guide 双语版用量章节、`design.md`

**验收标准**

1. 每次 AI 请求（含流式、含失败请求）落一条 usage 记录，cost 计算正确且标注 token 来源
2. 聚合 API 返回值与明细一致；分页与时间过滤正确
3. 前端仪表盘展示真实数据（非 mock）
4. 落库为异步路径，代理延迟无可测回归；落库失败不影响代理请求

---

## REQ-AI-003

### Virtual Key 配额与预算控制（Quota & Budget Enforcement）

- **优先级 / Effort：** P0 / M
- **状态：** 📋 待启动
- **依赖：** REQ-AI-001（key 身份）、REQ-AI-002（成本数据）

**背景与价值**

`tpm_limit` / `rpm_limit` / `budget_limit` / `budget_used` 字段运行时全部不被读取；`ai-rate-limit` 的 `limit_by=virtual_key` 静默降级为 global（且任意非法值也当 global 不报错）。本需求完成后 Virtual Key 治理闭环，M1 里程碑达成。

**需求范围**

- 后端：
  1. `ai-rate-limit` 接通 `limit_by=virtual_key`：读取 key 自身 tpm/rpm_limit，非法 `limit_by` 值改为配置校验报错
  2. 预算扣减：log 阶段将请求 cost 累加到 `budget_used`（批量/异步，方案设计定并发累加策略）；超预算请求拒绝
  3. 超限响应：429（限流）/ 403（预算耗尽），OpenAI 风格错误体 + `X-RateLimit-*` 响应头
- 前端：VirtualKeys 页展示 budget 使用进度条、TPM/RPM 配置生效状态、超限徽章
- 文档：guide 双语版配额章节

**验收标准**

1. 按 key 的 TPM / RPM 独立生效，互不串扰；未配置的 key 不受影响
2. `budget_used` 随请求持续累计，重启后从 DB 恢复；超预算后拒绝，调高 `budget_limit` 后恢复放行
3. 非法 `limit_by` 配置在 Admin API 保存时被拒绝（不再静默降级）
4. 集成测试覆盖限流触发、预算耗尽、恢复三条路径

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
- **依赖：** 无

**背景与价值**

`ai-rate-limit` / `ai-cache` / `ai-prompt-guard` 三个插件在 Admin schema 中的 config fields 是**空数组**，Kong Manager 无法渲染配置表单，只能手填 JSON——策略插件事实上不可被产品化使用。

**需求范围**

- 后端：`schemas.rs` 补全三个插件的完整 config fields（类型、默认值、校验规则），与插件实际配置结构一致
- 前端：三插件在 Plugins 页可用表单配置；Endpoint 发布向导支持勾选挂载限流 / Prompt Guard 策略（发布时一并创建插件）
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

### 分布式限流（Redis 后端）与内存限流治理

- **优先级 / Effort：** P2 / M
- **状态：** 📋 待启动
- **依赖：** 无（存储基建与 REQ-AI-007 方案统一）

**背景与价值**

限流状态为单进程内存：多节点各算各的、重启清零，无法支撑生产多副本部署。另有两处内存后端缺陷：DashMap 只重置窗口从不删 key（高基数下无界增长）；窗口长度 60s 硬编码。

**需求范围**

- 后端：
  1. `RateLimitStore` 增加 Redis 后端（`INCRBY key cost EX window` 或双桶近似滑动窗口，方案设计定）
  2. 内存后端 key 过期清理；窗口长度可配置
  3. Redis 不可用时的降级策略（fail-open / fail-closed 可配）
- 前端：无（豁免：Redis 连接为 kong.conf 网关级配置；插件侧窗口配置由 REQ-AI-006 表单承载）
- 文档：guide 双语版限流章节、部署文档 Redis 配置说明

**验收标准**

1. 两个网关节点共享同一 Redis 时限流计数全局一致
2. Redis 宕机时按配置的降级策略行为正确
3. 内存后端长时间高基数运行内存占用有界（key 清理生效）

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
```

`analysis.md` 的章节结构参考 [REQ-AI-001/analysis.md](REQ-AI-001/analysis.md)：
背景与价值 / 用户故事 / 现状事实与约束 / 功能需求（FR）/ 非功能需求 / 非目标 /
关联缺陷 / 决策记录 / 验收标准。
