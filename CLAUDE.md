# Kong-Rust 项目指南

## 项目概述

Kong-Rust 是使用 Rust + Cloudflare Pingora 完全重写 Kong API 网关的项目，目标是 100% 兼容 Kong，零成本替换。

- **语言**：Rust 2021 Edition（最低 1.84）
- **代理引擎**：Pingora（Cloudflare HTTP 代理）
- **Admin API**：axum 0.8
- **数据库**：sqlx 0.8（PostgreSQL，直接 SQL，无 ORM）
- **Lua 集成**：mlua（LuaJIT 绑定）
- **缓存**：moka（内存缓存）

## Workspace 结构（11 个 crate）

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
├── kong-cluster/       — CP/DP 集群通信（阶段 9，已完成）
├── kong-ai/            — AI Gateway（provider 适配 / v1/responses 协议 / 成本追踪）
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

### Docker 构建与测试

| 命令 | 作用 |
|------|------|
| `DOCKER_PLATFORM=linux/arm64 make DOCKER_TAG=fengyi95/kong-rust:latest docker-build` | 构建 arm64 镜像（M 系列 Mac 本地测试用） |
| `make DOCKER_TAG=fengyi95/kong-rust:latest docker-build` | 构建 amd64 镜像（线上发布，默认平台） |
| `make docker-push` | 推送镜像到 Docker Hub |
| `make docker-run` | db-less 模式运行容器 |
| `make docker-run-pg` | PostgreSQL 模式运行容器 |
| `make docker-stop` | 停止容器 |

**Docker 端到端测试流程**（本地验证镜像是否正常工作）：

```bash
# 1. 构建 arm64 测试镜像
DOCKER_PLATFORM=linux/arm64 make DOCKER_TAG=fengyi95/kong-rust:latest docker-build

# 2. 用 /Users/dawxy/kong/docker-compose.yml 启动完整环境
cd /Users/dawxy/kong && docker compose up -d

# 3. 等待健康检查通过后验证
curl -s http://127.0.0.1:8001/        # Admin API
curl -s http://127.0.0.1:8001/status   # 状态检查
curl -s http://127.0.0.1:80/           # 代理端口（无路由时 404）
curl -s http://127.0.0.1:8002/         # Kong Manager GUI
docker exec kong-kong-1 kong health    # 容器内健康检查

# 4. 构建 amd64 线上镜像并推送
make DOCKER_TAG=fengyi95/kong-rust:latest docker-build
make DOCKER_TAG=fengyi95/kong-rust:latest docker-push
```

**Docker Compose 测试环境**：`/Users/dawxy/kong/docker-compose.yml`
- PostgreSQL 15 + Kong-Rust，端口：80（代理）、443（Stream）、6443（HTTPS）、8001（Admin）、8002（GUI）、8007（Status）
- 数据持久化：`/Users/dawxy/kong/db`，SSL 证书：`/Users/dawxy/kong/certs/`

## 核心设计原则

1. **兼容优先**：所有外部行为与 Kong 完全一致
2. **Rust 原生**：核心路径用 Rust 实现
3. **直接 SQL**：不用 ORM，确保与 Kong 数据库 Schema 100% 一致
4. **最小侵入**：不修改 Kong 的 Lua 插件代码，不改变数据库 Schema

## 项目文档索引

项目文档统一存放在 `docs/` 目录：

| 文件 | 内容 |
|------|------|
| `docs/requirements.md` | 9 个需求（R1-R9）：代理引擎、路由、Admin API、数据库、Lua 插件、配置、健康检查、TLS、Hybrid 模式 |
| `docs/design.md` | 9 个组件设计：kong-core 到 kong-cluster 的详细接口和架构 |
| `docs/tasks.md` | 20 个阶段（1-20）的任务清单，含进度概览表和已知问题 |
| `docs/implementation-logs/` | 38 个已完成任务的实现日志（修改文件、代码统计、artifact 信息） |

### 进度概览

- **已完成**：80 个任务（阶段 1-15, 16.3-16.5, 17.1, 19B.1-19B.3, 20.1），涵盖核心模型、配置、数据库、路由、代理引擎（L7 + L4 Stream）、插件系统、Admin API（+ /cache + /debug/node/log-level + /timers）、TLS、健康检查、负载均衡、集成测试、Access Log、异步 DNS、Body Buffering、Docker 镜像构建、HTTP 代理性能优化、HTTP/2、ws_id 兼容、WebSocket 代理、QA 测试与 Bug 修复、Hybrid 模式、AI Gateway v1/responses、gRPC 代理、Graceful Shutdown、AI 高级路由(token-size + 语义) + 语义缓存
- **待实现**：9 个任务（阶段 16-19）— KeySet/Key 端点、Stream TLS Termination、RBAC、Lua 沙箱加固、Proxy Cache、OpenTelemetry、性能基准测试、Redis 向量后端(19B.4)
- **总计**：89 个任务（2026-04-19 审计修正：8 阶段补入 8.12a 子任务；4.3/6.3/8.15 的虚报声明已从任务描述中移除，缺失能力已在阶段 16-17 的待办中覆盖；2026-05-01 新增阶段 19B 4 个任务）
- **已知问题**：16 个（QA 发现，已全部修复 ✅，详见 `docs/tasks.md` 阶段 14）

## 强制要求：变更时同步更新文档

**每次实现新功能、修复 Bug 或进行重要代码变更时，必须同步更新：**

1. **更新 `docs/tasks.md`**：如果涉及已有任务，更新状态标记；新增任务追加到对应阶段
2. **记录实现日志**：在 `docs/implementation-logs/` 创建日志文件，记录修改文件、代码统计、artifact 信息
3. **更新 `docs/design.md`**：如果涉及架构变更或新组件
4. **更新 `docs/requirements.md`**：如果需求范围有变化

**不要跳过这些步骤**——docs/ 是项目的持久知识库，确保每个 AI agent 都能快速了解项目全貌。

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
