# Kong-Rust 项目指南

## 项目概述

Kong-Rust 是使用 Rust + Cloudflare Pingora 完全重写 Kong API 网关的项目，目标是 100% 兼容 Kong，零成本替换。

- **语言**：Rust 2021 Edition（最低 1.84）
- **代理引擎**：Pingora（Cloudflare HTTP 代理）
- **Admin API**：axum 0.8
- **数据库**：sqlx 0.8（PostgreSQL，直接 SQL，无 ORM）
- **Lua 集成**：mlua（LuaJIT 绑定）
- **缓存**：moka（内存缓存）

## Workspace 结构（10 个 crate）

```
crates/
├── kong-core/          — 核心数据模型和 trait（零依赖底层）
├── kong-config/        — 配置解析（kong.conf 格式）
├── kong-db/            — PostgreSQL DAO + 缓存 + db-less + migration
├── kong-router/        — 路由引擎（traditional + expressions）
├── kong-proxy/         — Pingora 代理引擎 + 负载均衡 + 健康检查
├── kong-plugin-system/ — 插件注册/执行框架
├── kong-lua-bridge/    — Lua 兼容层 + PDK + ngx.*
├── kong-admin/         — Admin API（axum）
├── kong-cluster/       — CP/DP 集群通信（阶段 9，待实现）
└── kong-server/        — 主入口二进制
```

依赖方向：kong-core（最底层）→ kong-server（顶层入口），严格单向，禁止循环依赖。

## 常用命令

| 命令 | 作用 |
|------|------|
| `make build` | 编译（debug） |
| `make check` | 快速类型检查 |
| `make test` | 运行所有测试 |
| `make dev` | 一键启动：PG + cargo run |
| `make dev-dbless` | db-less 模式启动 |
| `make fmt` | 格式化 |
| `make lint` | Clippy 检查 |
| `make services-up` / `make services-down` | 启停依赖服务 |

## 核心设计原则

1. **兼容优先**：所有外部行为与 Kong 完全一致
2. **Rust 原生**：核心路径用 Rust 实现
3. **直接 SQL**：不用 ORM，确保与 Kong 数据库 Schema 100% 一致
4. **最小侵入**：不修改 Kong 的 Lua 插件代码，不改变数据库 Schema

## Spec-Workflow 索引

项目使用 `.spec-workflow/` 目录管理规划和设计文档，这是项目的核心知识库。

### 导航文档（Steering）

| 文件 | 内容 |
|------|------|
| `.spec-workflow/steering/product.md` | 产品目标、用户画像、核心特性、成功标准 |
| `.spec-workflow/steering/tech.md` | 技术栈、依赖清单、开发命令、技术约束 |
| `.spec-workflow/steering/structure.md` | 目录结构、命名规范、导入规范、代码组织原则 |

### 功能规格（Specs）

| 文件 | 内容 |
|------|------|
| `.spec-workflow/specs/kong-rust/requirements.md` | 9 个需求（R1-R9）：代理引擎、路由、Admin API、数据库、Lua 插件、配置、健康检查、TLS、Hybrid 模式 |
| `.spec-workflow/specs/kong-rust/design.md` | 9 个组件设计：kong-core 到 kong-cluster 的详细接口和架构 |
| `.spec-workflow/specs/kong-rust/tasks.md` | 15 个阶段（1-11）的任务清单，含状态标记（`[x]` 已完成 / `[ ]` 待实现） |

### 实现日志

`.spec-workflow/specs/kong-rust/Implementation Logs/` 目录记录每个已完成任务的实现详情（修改文件、代码统计、API/组件/函数等 artifact 信息）。

### 进度概览

- **阶段 1-8e, 10-11**：已完成（54 个任务），涵盖核心模型、配置、数据库、路由、代理引擎（L7 + L4 Stream）、插件系统、Admin API、TLS、健康检查、负载均衡、集成测试、Access Log、异步 DNS、Body Buffering、Docker 镜像构建、HTTP 代理性能优化
- **阶段 9**：待实现（7 个任务），Hybrid 模式 CP/DP 集群通信

## 强制要求：自动更新 Spec-Workflow

**每次实现新功能、修复 Bug 或进行重要代码变更时，必须同步更新 spec-workflow：**

1. **更新 tasks.md**：如果涉及已有任务，更新状态标记；如果是新增任务，追加到对应阶段
2. **记录 Implementation Log**：使用 `mcp__spec-workflow__log-implementation` 工具记录实现详情（artifact 信息至关重要，供未来 AI agent 检索复用）
3. **更新 structure.md**：如果新增了文件/目录/crate，更新项目结构文档
4. **更新 design.md**：如果涉及架构变更或新组件，更新设计文档
5. **更新 requirements.md**：如果需求范围有变化，更新需求文档

**不要跳过这些步骤**——spec-workflow 是项目的持久知识库，确保每个 AI agent 都能快速了解项目全貌。

## gstack

- **所有网页浏览一律使用 `/browse` skill**，不要使用 `mcp__claude-in-chrome__*` 工具
- 可用 skills 列表：
  - `/office-hours` - 办公时间
  - `/plan-ceo-review` - CEO 评审计划
  - `/plan-eng-review` - 工程评审计划
  - `/plan-design-review` - 设计评审计划
  - `/design-consultation` - 设计咨询
  - `/review` - 代码审查
  - `/ship` - 发布
  - `/browse` - 网页浏览
  - `/qa` - 质量保证
  - `/qa-only` - 仅 QA
  - `/design-review` - 设计评审
  - `/setup-browser-cookies` - 设置浏览器 cookies
  - `/retro` - 复盘
  - `/investigate` - 调查
  - `/document-release` - 发布文档
  - `/codex` - Codex
  - `/careful` - 谨慎模式
  - `/freeze` - 冻结
  - `/guard` - 守卫
  - `/unfreeze` - 解冻
  - `/gstack-upgrade` - 升级 gstack
- 如果 gstack skills 无法使用，运行 `cd .claude/skills/gstack && ./setup` 重新构建二进制并注册 skills
