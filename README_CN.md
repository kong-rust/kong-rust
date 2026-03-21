# Kong-Rust

Rust 原生 **AI 网关** —— API 网关、LLM 网关、Agent 网关、MCP/Skill 网关，单一二进制文件。在 API 网关场景下 100% 兼容 [Kong Gateway](https://github.com/Kong/kong) —— 同样的功能，更强的性能，直接替换。

## 为什么选择 Kong-Rust？

AI 时代需要新一代网关。传统 API 网关处理 HTTP 流量；LLM 代理处理模型调用；MCP 网关路由工具访问 —— 但没有一个能覆盖全局。Kong-Rust 将四种网关统一为 **一个 Rust 原生 AI 网关**，基于 [Cloudflare Pingora](https://github.com/cloudflare/pingora) 构建。

```
┌─────────────────────────────────────────────────┐
│              Kong-Rust  AI 网关                  │
│                                                 │
│  ┌───────────┐ ┌───────────┐ ┌───────────────┐ │
│  │ API 网关   │ │ LLM 网关   │ │  Agent 网关   │ │
│  │(Kong 100%)│ │           │ │               │ │
│  └───────────┘ └───────────┘ └───────────────┘ │
│  ┌──────────────────────────────────────────┐   │
│  │         MCP / Skill 网关                  │   │
│  └──────────────────────────────────────────┘   │
│                                                 │
│  Rust · Pingora · 单一二进制                      │
└─────────────────────────────────────────────────┘
```

| | Kong (Lua) | LiteLLM (Python) | Kong-Rust |
|---|---|---|---|
| **API 网关** | 完整 | 无 | **完整（100% Kong 兼容，性能更强）** |
| **LLM 网关** | Lua 插件 | 完整（100+ provider） | Rust 原生（规划中） |
| **Agent 网关** | 无 | 无 | Rust 原生（规划中） |
| **MCP / Skill 网关** | 企业版 | 基础 | Rust 原生（规划中） |
| **引擎** | OpenResty (Nginx + LuaJIT) | uvicorn | **Pingora (Rust, 多线程)** |
| **语言** | Lua | Python | **Rust** |

## 核心特性

### API 网关（Kong 兼容 — 更快更强）

Kong 能做的，Kong-Rust 都能做 —— 而且更快、更安全。

- **100% Kong 兼容** — 数据模型、Admin API、`kong.conf` 配置格式、声明式配置（YAML/JSON）、Lua 插件接口（PDK + `ngx.*`）完全一致。现有 Kong 部署零配置修改即可迁移。
- **性能更强** — Pingora 多线程架构取代 OpenResty 单线程事件循环。共享连接池、零拷贝代理、无 GC 停顿。真正的多核利用，无需 worker 进程开销。
- **内存安全** — Rust 所有权系统消除 use-after-free、缓冲区溢出和数据竞争，这些是 C/Lua FFI 边界的常见隐患。
- **双路由引擎** — 支持 `traditional_compatible` 和 `expressions` 两种路由模式，LRU 路由缓存加速热路径
- **完整 Lua 插件生态** — 通过 mlua + LuaJIT 运行全部 47 个 Kong 内置 Lua 插件，无需重写任何插件
- **负载均衡与健康检查** — 轮询、一致性哈希、最少连接、延迟优先。主动/被动健康检查，自动恢复。
- **TLS 终止与 SNI** — 证书管理、基于 SNI 的路由、HTTP/2 ALPN、上游 mTLS
- **L4 Stream 代理** — TCP/TLS Passthrough 四层代理，支持 SNI 和 source/dest CIDR 路由
- **Kong Manager UI** — 兼容官方 Kong Manager 前端管理界面
- **多数据源** — PostgreSQL 数据库模式或 db-less 声明式配置模式
- **Hybrid 模式** — Control Plane / Data Plane 分离部署（规划中）

### LLM 网关（规划中）

- **Token 限流** — 按 key/route/consumer 的 TPM/RPM 限制
- **多模型负载均衡与 Fallback** — 多个 LLM provider 作为上游，自动故障转移
- **虚拟 API Key 管理** — 发行虚拟 key 映射到真实 provider key，设置预算/限额
- **Token 成本追踪** — 按 key/team/route 的 token 用量和费用统计
- **语义缓存** — 向量相似度缓存 LLM 响应
- **Prompt Guard** — 正则 + 语义级提示词注入检测

### Agent 网关（规划中）

- **Agent 通信路由** — 路由和管理 Agent 间流量
- **Agent 身份与访问控制** — 按 Agent 的认证和授权
- **Agent 可观测性** — 延迟、错误率、使用量指标

### MCP / Skill 网关（规划中）

- **MCP Server 注册** — 通过 Admin API 注册、发现、版本管理 MCP Server
- **MCP 路由与负载均衡** — 工具调用路由到 MCP Server，支持故障转移
- **Skill 编排** — Skill 注册、组合、执行
- **认证与可观测性** — 按 tool/agent 的访问控制，调用指标

## 架构

```
kong-server（主入口二进制）
 ├── kong-core          — 核心数据模型和 trait
 ├── kong-config        — 配置解析（kong.conf 格式）
 ├── kong-db            — PostgreSQL DAO + 缓存 + db-less + migration
 ├── kong-router        — 路由引擎（traditional + expressions + L4 stream）
 ├── kong-proxy         — Pingora 代理引擎（L7 HTTP + L4 Stream）+ 负载均衡 + 健康检查
 ├── kong-plugin-system — 插件注册/执行框架
 ├── kong-lua-bridge    — Lua 兼容层 + PDK + ngx.*
 ├── kong-admin         — Admin API（axum）
 └── kong-cluster       — CP/DP 集群通信（规划中）
```

## 快速开始

### 环境要求

- Rust 1.94.0+（含 Cargo）
- PostgreSQL 15+（数据库模式），或无需数据库（db-less 模式）
- Docker（可选，用于托管 PostgreSQL）

### 数据库模式

```bash
# 启动 PostgreSQL（通过 Docker）
make services-up

# 一键启动：PostgreSQL + 数据库初始化 + 运行
make dev
```

### DB-less 模式

```bash
# 无需数据库
make dev-dbless
```

### 手动配置

```bash
# 编译
cargo build --workspace

# 初始化数据库
cargo run -p kong-server -- -c kong.conf.default db bootstrap

# 启动
cargo run -p kong-server -- -c kong.conf.default
```

### 验证

```bash
# Admin API
curl http://localhost:8001/

# 创建 Service
curl -X POST http://localhost:8001/services \
  -H 'Content-Type: application/json' \
  -d '{"name":"httpbin","url":"https://httpbin.org"}'

# 创建 Route
curl -X POST http://localhost:8001/services/httpbin/routes \
  -H 'Content-Type: application/json' \
  -d '{"paths":["/httpbin"]}'

# 通过网关代理请求
curl http://localhost:8000/httpbin/get
```

## Kong Manager

Kong-Rust 兼容官方 [Kong Manager](https://docs.konghq.com/gateway/latest/kong-manager/) 前端管理界面。

`8001` 是 Admin API 端口，`8002` 是 Kong Manager GUI 端口。`/services` 这类 Admin API 端点应访问 `8001`，不是 `8002`。

```bash
# 安装依赖
make manager-install

# 开发模式启动（默认: http://localhost:8002）
make manager-dev
```

## 配置

Kong-Rust 使用与 Kong 相同的 `kong.conf` 配置格式。完整配置项请参考 `kong.conf.default`。

主要配置项：

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `proxy_listen` | `0.0.0.0:8000` | HTTP 代理监听地址 |
| `admin_listen` | `127.0.0.1:8001` | Admin API 监听地址 |
| `stream_listen` | `off` | L4 Stream 代理监听（如 `0.0.0.0:9000`） |
| `database` | `postgres` | 数据库模式（`postgres` 或 `off`） |
| `pg_host` | `127.0.0.1` | PostgreSQL 主机 |
| `pg_port` | `5432` | PostgreSQL 端口 |
| `pg_database` | `kong` | PostgreSQL 数据库名 |
| `router_flavor` | `traditional_compatible` | 路由引擎（`traditional_compatible` 或 `expressions`） |

支持 `KONG_` 前缀的环境变量覆盖（例如 `KONG_PG_PORT=5433`）。
测试同时支持官方 Kong 风格的 `KONG_TEST_*` 和 `KONG_SPEC_TEST_*` 变量，测试入口会在调用 `cargo test` 前自动映射为实际生效的 `KONG_*` 变量，并默认使用 `KONG_DATABASE=postgres`，与 Kong 默认测试策略保持一致。

示例：

```bash
KONG_TEST_DATABASE=postgres KONG_TEST_PG_PORT=55432 make test
KONG_TEST_DATABASE=off make test
./scripts/run-cargo-test.sh --print-effective-env
```

## 开发

| 命令 | 说明 |
|------|------|
| `make build` | 编译（debug） |
| `make check` | 快速类型检查 |
| `make test` | 运行所有测试（默认等价于 `KONG_TEST_DATABASE=postgres`） |
| `make test-pg` | 自动启动本地 PostgreSQL 测试依赖，并以 `KONG_TEST_DATABASE=postgres` 运行测试 |
| `make test-dbless` | 以 `KONG_TEST_DATABASE=off` 运行测试 |
| `make fmt` | 格式化代码 |
| `make lint` | Clippy 静态分析 |
| `make dev` | 全栈启动（PG + 初始化 + 运行） |
| `make dev-dbless` | DB-less 模式启动 |

更多命令请参考 [Makefile](Makefile)。

## 兼容性

Kong-Rust 的目标是与 Kong Gateway 100% 行为兼容：

- **Admin API** — 所有核心实体的 CRUD 端点：Services、Routes、Consumers、Plugins、Upstreams、Targets、Certificates、SNIs、CA Certificates、Vaults
- **数据库 Schema** — 直接操作 Kong 的 PostgreSQL 表（无 ORM，使用 sqlx 原生 SQL）
- **配置文件** — 读取相同格式的 `kong.conf`（key=value）
- **Lua 插件** — 通过 mlua + LuaJIT 运行 Kong Lua 插件，提供完整 PDK 支持
- **迁移** — 使用 `decK dump` 从现有 Kong 导出配置，直接导入 Kong-Rust

## 项目进度

### 传统 API 网关

| 阶段 | 状态 | 说明 |
|------|------|------|
| 1. 核心模型 | 已完成 | 数据模型、trait、配置解析 |
| 2. 数据库 | 已完成 | PostgreSQL DAO、缓存、db-less、migration |
| 3. 路由引擎 | 已完成 | Traditional + Expressions 路由 |
| 4. 代理引擎 | 已完成 | Pingora 集成、负载均衡、健康检查 |
| 5. 插件系统 | 已完成 | 插件注册、Lua Bridge、PDK |
| 6. Admin API | 已完成 | 完整 CRUD、嵌套端点、Kong Manager 支持 |
| 7. TLS | 已完成 | 证书管理、SNI 路由 |
| 8. 集成测试 | 已完成 | 端到端测试、访问日志、L4 Stream 代理 |
| 9. Hybrid 模式 | 规划中 | CP/DP 集群通信 |

### AI 网关路线图

| 阶段 | 状态 | 说明 |
|------|------|------|
| Phase 0 | 进行中 | 稳定性加固 — Kong 官方 spec 测试对齐 |
| Phase 1 | 规划中 | Hybrid CP/DP 模式（传统网关封顶） |
| Phase 2 | 规划中 | LLM Proxy — Token 限流、多模型负载均衡与 Fallback、虚拟 API Key、成本追踪、语义缓存、Prompt Guard |
| Phase 3 | 规划中 | MCP Gateway — Server 注册、发现、路由、认证、可观测性 |
| Phase 4 | 规划中 | Skill / Agent Gateway — Skill 编排、Agent 路由、身份管理 |

**所有 AI 能力将以 Rust 原生代码实现** —— 不依赖 Lua 插件。这是 Kong-Rust 相对于 Kong（Lua）和 LiteLLM（Python）的核心性能优势。

详细路线图请参阅 [docs/designs/kong-rust-roadmap.md](docs/designs/kong-rust-roadmap.md)。

## 文档

| 文档 | 说明 |
|------|------|
| [产品路线图](docs/designs/kong-rust-roadmap.md) | 产品路线图与技术战略 |
| [设计文档](docs/design.md) | 架构与组件设计 |
| [需求文档](docs/requirements.md) | 功能与非功能需求 |
| [任务跟踪](docs/tasks.md) | 任务进度 |
| [待办事项](TODOS.md) | 优先级排序的待办清单 |

## 许可证

Apache-2.0

## 致谢

- [Kong Gateway](https://github.com/Kong/kong) — 本项目兼容的 API 网关
- [Pingora](https://github.com/cloudflare/pingora) — Cloudflare 的 Rust HTTP 代理框架
- [axum](https://github.com/tokio-rs/axum) — Rust Web 框架
- [mlua](https://github.com/mlua-rs/mlua) — Rust 的 Lua/LuaJIT 绑定
