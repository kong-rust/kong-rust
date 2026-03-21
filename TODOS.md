# TODOS

> 执行路径：三轨并行。轨道 A（传统网关加固）、轨道 B（AI 网关引擎）、轨道 C（AI 网关控制台）。A/B 无技术依赖，C 依赖 B 的 Admin API。
> 详细战略文档见 [docs/designs/ai-gateway-strategy.md](docs/designs/ai-gateway-strategy.md)

---

## 轨道 A：传统网关加固

### P0: 稳定性加固 + Kong 测试用例对齐 (Phase 0) — 最高优先级

busted 兼容层直接跑 Kong 官方 spec 文件，验证所有核心路径的兼容性和稳定性。

- **执行步骤:**
  1. busted + spec.helpers 核心兼容层（框架搭建）
  2. 02-integration spec 对齐（Admin API 兼容性验证）
  3. 03-plugins 核心插件 spec（key-auth, rate-limiting, cors, request-transformer 等高频插件）
  4. 根据 spec 失败结果修复稳定性问题
  5. 逐步扩展到更多插件 spec（47+ 内置插件）
- **Why:** 传统网关是所有后续能力的地基。没有 Kong spec 覆盖的兼容性声明是空话
- **Effort:** XL (human) → L (CC)
- **Depends on:** 无（立即开始）

### P1: Hybrid CP/DP 模式 (Phase 1)

完成 Kong 兼容能力，支持 Traditional + Hybrid 两种部署模式。

- **Phase 1a:** V1 全量推送 + mTLS + 心跳 + 重连 + 角色分支
- **Phase 1b:** V2 增量同步 + snappy + 配置哈希 + 混合兼容测试
- **Phase 1c:** 配置回滚 + 缓存降级 + 审计日志 + Prometheus 集群指标
- **Why:** 传统网关封顶，生产环境必需
- **Effort:** XL (human) → L (CC)
- **Depends on:** Phase 0 基本完成

---

## 轨道 B：AI 网关

### P1: LLM 网关 — Phase 2a-MVP（与 Phase 0 并行启动）

OpenAI 协议代理 + Token 计数 + 单 provider。最小可演示的 LLM Gateway。

- **新建 `kong-ai` crate**，包含通用限流器基础设施（LLM/MCP/Agent 三网关共享）
- **交付标准:** 能成功代理 OpenAI `/v1/chat/completions`，Token 计数写入 access log
- **Token 计数:** `tiktoken-rs` 支持 GPT 系列，其他 provider 用 char/4 近似
- **对标:** LiteLLM, Kong Enterprise AI Gateway, Portkey, Cloudflare AI Gateway
- **Why:** 品牌已定位为 AI 网关，需要尽快有 AI 代码兑现定位
- **Effort:** L (human) → M (CC)
- **Depends on:** 无（与 Phase 0 并行启动）

### P1: LLM 网关 — Phase 2a-Full

多 provider 负载均衡 + Fallback + Anthropic/Gemini 支持。

- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 2a-MVP 完成

### P1: LLM 网关 — Phase 2b

Virtual API Key（映射虚拟 key→真实 provider key，设置预算/限额）+ Token 成本追踪。

- **Effort:** M (human) → S (CC)
- **Depends on:** Phase 2a-Full 完成

### P1: LLM 网关 — Phase 2c

语义缓存（向量相似度缓存 LLM 响应）。Embedding 来源：优先外部 API，后期本地 ONNX 模型。

- **Effort:** L (human) → M (CC)
- **Depends on:** Phase 2b 完成

### P1: LLM 网关 — Phase 2d

Prompt Guard（正则+语义级 prompt injection 检测和内容过滤）。

- **Effort:** M (human) → S (CC)
- **Depends on:** Phase 2a-Full 完成（不依赖 2b/2c）

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
