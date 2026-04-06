# TODOS

> 执行路径：三轨并行。轨道 A（传统网关加固）、轨道 B（AI 网关引擎）、轨道 C（AI 网关控制台）。A/B 无技术依赖，C 依赖 B 的 Admin API。
> 详细战略文档见 [docs/designs/ai-gateway-strategy.md](docs/designs/ai-gateway-strategy.md)

---

## 轨道 A：传统网关加固

### P0: 稳定性加固 + Kong 测试用例对齐 (Phase 0) — ✅ 已完成

busted 兼容层直接跑 Kong 官方 spec 文件，验证所有核心路径的兼容性和稳定性。

- **最终结果:** 375/375 (100%) — 8 个 spec 全部 0 failures, 0 errors
  - smoke (59/59), services (75/75), consumers (99/99), plugins (26/26), tags (14/14), kong_routes (87/87), request-id (10/10), uri-encoding (5/5)
- **已完成内容:**
  1. busted + spec.helpers 核心兼容层（1800+ 行 Lua shim）
  2. FlexibleBody 提取器（JSON + form-urlencoded + multipart）
  3. 完整 Admin API 兼容（CRUD 验证、PATCH 深度合并、PUT 替换语义、唯一约束 409）
  4. X-Kong-Request-Id、/tags、/endpoints、/schemas、/config 端点
  5. DB-less 模式基础支持（POST /config 声明式配置）
  6. 插件 config 默认值填充、schema 验证
- **Why:** 传统网关是所有后续能力的地基。没有 Kong spec 覆盖的兼容性声明是空话
- **Effort:** XL (human) → L (CC)
- **Completed:** 2026-03-21

### P1: Hybrid CP/DP 模式 (Phase 1) — ✅ 已完成

完成 Kong 兼容能力，支持 Traditional + Hybrid 两种部署模式。

- **已完成内容:**
  - Phase 1a: kong-cluster crate（CP/DP 状态管理、V1 全量推送 + V2 JSON-RPC 增量同步、mTLS shared/pki 双模式、30s PING 心跳 + 5-10s 随机重连、磁盘缓存降级、broadcast channel 零拷贝广播）
  - ClusterListenerTask（TCP listener + TLS acceptor + WebSocket upgrade）、DpConnectorTask（连接 → basic_info → 配置接收 → 断线重连）
  - Admin API: GET /clustering/data-planes + GET /clustering/status
  - 46 个集成测试全部通过（CP/DP 单元级、V1/V2 协议、Kong Lua 哈希兼容、TLS 配置、E2E WebSocket）
  - Phase 1c（配置版本回滚 + 审计日志 + Prometheus）待后续版本
- **Completed:** 2026-04-06
- **Eng Review:** 12 个架构决策已确认，详见 docs/designs/kong-rust-roadmap.md

---

## 轨道 B：AI 网关

### P1: LLM 网关 — Phase 2a-MVP — ✅ 已完成

OpenAI 协议代理 + Token 计数 + 单 provider。

- **已完成内容:** kong-ai crate、ai-proxy 插件（非流式+流式 SSE）、OpenAI driver、三级 Token 计数（provider > tiktoken > 估算）
- **Completed:** 2026-03-22

### P1: LLM 网关 — Phase 2a-Full — ✅ 已完成

多 provider 负载均衡 + Fallback + Anthropic/Gemini/Qwen/混元 + 智能路由。

- **已完成内容:** 4 个 provider driver（OpenAI、Anthropic、Gemini、OpenAI Compat）、ModelGroupBalancer（加权 RR + 优先级 fallback + 冷却）、Anthropic 双向 codec、双协议暴露（OpenAI + Claude）、ModelRouter（正则匹配 + 加权选择）
- **Completed:** 2026-03-22

### P1: LLM 网关 — Phase 2b — ✅ 已完成

Virtual API Key + Token 成本追踪。

- **已完成内容:** ai_virtual_keys 表 + Admin API CRUD（创建/轮换/脱敏）、TokenCounter + calculate_cost（按模型定价）
- **Completed:** 2026-03-22

### P1: LLM 网关 — Phase 2c — ✅ 已完成

语义缓存（缓存键基础设施）。

- **已完成内容:** ai-cache 插件（SHA256 缓存键、last_question/all_questions 策略、skip header）。Redis 后端集成待后续版本
- **Completed:** 2026-03-22

### P1: LLM 网关 — Phase 2d — ✅ 已完成

Prompt Guard。

- **已完成内容:** ai-prompt-guard 插件（正则 deny/allow patterns + 消息长度限制 + block/log_only 模式）
- **Completed:** 2026-03-22

### P2: AI 智能路由引擎（Phase 2 后续增强）

在 ai-proxy 基础上构建更丰富的路由策略。基础的正则匹配路由和加权路由已完成，以下是待实现的高级路由策略。

- ~~**Model 通配符路由:** 支持 `gpt-4*` 匹配 gpt-4, gpt-4-turbo, gpt-4o 等~~ ✅ 已通过 `model_routes` 正则匹配实现
- ~~**加权随机路由:** 按成本/延迟/可用性加权随机选择~~ ✅ 已通过 `model_routes` targets 加权轮询实现
- **精确 Token 计数:** 接入专用 tokenizer（tiktoken-rs / 自研模型 tokenizer），替代 len/4 估算
- **Token 感知路由:** 根据 prompt token 长度选择模型（短请求→便宜模型，长上下文→大窗口模型）
- **成本优化路由:** 自动选择满足能力要求的最便宜 provider
- **内部推理模型感知:** 支持 `provider_type=internal`，感知自研 LLM 的 IP 列表/服务发现/动态扩缩容
- **ModelGroupBalancer 接入 DAO:** 将 access 阶段从内联配置升级为通过 model name 查 DAO 获取 model group + balancer.select()
- **Why:** AI 网关的核心差异化能力
- **Context:** 基础正则匹配+加权路由已在 2026-03-22 实现。剩余为高级策略
- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 2 基础完成

### P1: MCP / Skill 网关 (Phase 3)

MCP Server 注册/发现/路由/认证/可观测性。新建 `kong-mcp` crate。

- **对标:** IBM ContextForge, Microsoft MCP Gateway, Kong Enterprise, Higress
- **Why:** 为 Agent Gateway 做基础设施准备
- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 2a-Full 的通用限流器基础设施

### P1: Agent 网关 (Phase 4)

Agent 注册/发现、A2A 协议代理、Agent 身份管理、会话状态路由。新建 `kong-agent` crate。

- **对标:** Google A2A Protocol, LangGraph Platform, CrewAI, solo.io
- **Why:** 差异化竞争力，超越 MCP 的更高层抽象
- **Effort:** XL (human) → L (CC)
- **Depends on:** Phase 3 MCP Gateway 完成

---

## 轨道 C：AI 网关控制台

### P1: 企业版 Kong Manager — Phase 5a（基础框架）

全新前端项目，替换 Kong Manager OSS。React 19 + Next.js 15 + shadcn/ui + Tailwind CSS 4。

- **交付物:** 完整覆盖 Kong Manager OSS 全部 14 个实体 CRUD，现代化 UI + 四子网关统一仪表盘
- **对标:** Kong Enterprise Manager (Konnect), Portkey Dashboard, Helicone Dashboard
- **Why:** 现有 Kong Manager OSS 是纯 CRUD 表单，无 AI 能力、无仪表盘、无可观测性。企业版控制台是 AI 网关产品化的关键一环
- **Effort:** XL (human) → L (CC)
- **Depends on:** Phase 2a-MVP 完成（需要 AI 相关 Admin API 就绪）

### P1: LLM 管理面板 — Phase 5b

LLM Provider 配置、Virtual API Key 管理、Token 成本仪表盘、模型调用日志、Fallback 链可视化编辑、Prompt Guard 规则管理。

- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 5a + Phase 2b 完成

### P1: Agent/MCP 管理面板 — Phase 5c

Agent 注册/拓扑图、MCP Server 管理、工具调用链路追踪、Skill 编排可视化画布。

- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 5b + Phase 3/4 完成

---

## P2: 非核心路径

### 多 CP 高可用 (active-active)

支持多个 CP 节点同时运行，DP 可连接任意 CP，CP 间通过共享 DB 保持一致。

- **Why:** 生产环境单 CP 是单点故障
- **Context:** Kong Enterprise 支持多 CP，Kong OSS 不支持。当前 ConfigApplier trait 为多 CP 留了扩展空间
- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 1 Hybrid 模式完成

### 验证 KIC 与 Kong-Rust Admin API 兼容性

用官方 kong-ingress-controller 连接 Kong-Rust 的 Admin API，验证能否正常管理路由/服务/插件。

- **Why:** KIC 通过 Admin API 管理 Kong，Kong-Rust 的 Admin API 100% 兼容则无需额外开发
- **Context:** 如果 KIC 直接可用，零开发成本打开 K8s 生态
- **Effort:** S (human) → S (CC)
- **Depends on:** Admin API 基本稳定
