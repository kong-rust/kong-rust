# TODOS

## P0: 稳定性加固 + Kong 测试用例对齐 (Phase 0) — 最高优先级

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

---

## P1: AI 网关 — Rust 原生 LLM Proxy (Phase 2)

Rust 原生实现企业级 LLM 代理能力，不依赖 Lua 插件。可能需新建 `kong-ai` crate。

- **能力清单:** Token-based 限流 (TPM/RPM)、Multi-model 负载均衡 + Fallback、Virtual API Key 管理、Token 成本追踪、语义缓存（向量相似度）、Prompt Guard
- **对标:** LiteLLM + Kong Enterprise AI Gateway + Higress
- **Why:** AI 网关是项目未来重心和核心差异化。Rust 原生性能远超 Python (LiteLLM) 和 Lua (Kong)
- **Effort:** XL (human) → L (CC)
- **Depends on:** Phase 1 Hybrid 模式完成

## P1: AI 网关 — MCP Gateway 基础对齐 (Phase 3)

MCP Server 注册/发现/路由/认证/可观测性。对齐市场核心能力即可。

- **对标:** IBM ContextForge, Microsoft MCP Gateway, Kong Enterprise
- **Why:** 为 Skill/Agent Gateway 做基础设施准备
- **Effort:** L (human) → M (CC)
- **Depends on:** LLM Proxy 基础完成

## P1: AI 网关 — Skill / Agent Gateway (Phase 4)

超越 MCP 的更高层抽象，项目未来差异化方向。Skill 注册/发现/编排、Agent 通信路由、身份管理。

- **Why:** MCP 可能被逐步淘汰，Skill 是更好的能力抽象层
- **Context:** 需要深入调研 Skill 定义和协议标准后确定具体架构
- **Effort:** XL (human) → L (CC)
- **Depends on:** MCP Gateway 基础完成

## P2: 多 CP 高可用 (active-active)

支持多个 CP 节点同时运行，DP 可连接任意 CP，CP 间通过共享 DB 保持一致。

- **Why:** 生产环境单 CP 是单点故障
- **Context:** Kong Enterprise 支持多 CP，Kong OSS 不支持。当前 ConfigApplier trait 为多 CP 留了扩展空间
- **Effort:** L (human) → M (CC)
- **Depends on:** 阶段 9 Hybrid 模式完成

## P2: 验证 KIC 与 Kong-Rust Admin API 兼容性

用官方 kong-ingress-controller 连接 Kong-Rust 的 Admin API，验证能否正常管理路由/服务/插件。

- **Why:** KIC 通过 Admin API 管理 Kong，Kong-Rust 的 Admin API 100% 兼容则无需额外开发
- **Context:** 如果 KIC 直接可用，零开发成本打开 K8s 生态
- **Effort:** S (human) → S (CC)
- **Depends on:** Admin API 基本稳定
