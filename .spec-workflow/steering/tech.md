# 技术栈

## 项目类型

高性能 API 网关 / 反向代理服务器，兼容 Kong Gateway 的 Rust 重写版本。

## 核心技术

### 语言

- **语言**：Rust（Edition 2021，最低版本 1.94.0）
- **构建工具**：Cargo（workspace 模式）
- **包管理**：crates.io

### 关键依赖

| 分类 | Crate | 版本 | 用途 |
|------|-------|------|------|
| **代理引擎** | `pingora` | workspace | Cloudflare HTTP 代理引擎核心 |
| | `pingora-core` | workspace | Pingora 核心库 |
| | `pingora-proxy` | workspace | 反向代理 trait 和生命周期 |
| | `pingora-load-balancing` | workspace | 负载均衡算法 |
| | `pingora-cache` | workspace | 代理缓存 |
| | `pingora-http` | workspace | HTTP 协议处理 |
| **Web 框架** | `axum` | 0.8 | Admin API HTTP 框架 |
| | `tower` | 0.5 | 中间件抽象层 |
| | `tower-http` | 0.6 | HTTP 中间件（CORS 等） |
| | `hyper` | 1.0 | 底层 HTTP 库 |
| **异步运行时** | `tokio` | workspace | 异步运行时（features: full） |
| **数据库** | `sqlx` | 0.8 | PostgreSQL 异步驱动（非 ORM） |
| **缓存** | `moka` | 0.12 | 高性能内存缓存（模拟 kong.cache） |
| **Lua 集成** | `mlua` | 0.10 | LuaJIT 绑定（运行 Lua 插件） |
| **序列化** | `serde` + `serde_json` | workspace | JSON 序列化/反序列化 |
| **CLI** | `clap` | workspace | 命令行参数解析 |
| **日志** | `tracing` + `tracing-subscriber` + `tracing-appender` | workspace | 结构化日志、追踪和文件输出 |
| **错误处理** | `thiserror` / `anyhow` | workspace | 错误类型定义和传播 |
| **DNS** | `hickory-resolver` | 0.25 | 异步 DNS 解析器（带缓存，替代系统 DNS） |
| **临时文件** | `tempfile` | 3 | 大 body 缓冲溢出到磁盘（SpillableBuffer） |
| **工具** | `uuid`, `regex`, `chrono`, `indexmap`, `rand`, `sha2`, `base64` | workspace | 通用工具库 |

### 应用架构

**Workspace 多 Crate 分层架构：**

```
kong-server（入口）
 ├── kong-core       — 核心数据模型和 trait 定义（无依赖项）
 ├── kong-config     — 配置解析（依赖 kong-core）
 ├── kong-db         — 数据库 DAO + 缓存（依赖 kong-core, kong-config）
 ├── kong-router     — 路由引擎（依赖 kong-core）
 ├── kong-proxy      — Pingora 代理引擎（依赖 kong-core, kong-config, kong-db, kong-router, kong-plugin-system）
 ├── kong-plugin-system — 插件注册/执行框架（依赖 kong-core）
 ├── kong-lua-bridge — Lua 插件兼容层 + PDK（依赖 kong-core, kong-db）
 ├── kong-admin      — Admin API / axum（依赖 kong-core, kong-config, kong-db, kong-plugin-system）
 └── kong-cluster    — CP/DP 集群通信（计划中，依赖 kong-core, kong-config, kong-db）
```

**依赖方向**：上层依赖下层，kong-core 是最底层的零外部 crate 依赖模块。

### 数据存储

- **主存储**：PostgreSQL（与 Kong 共享相同 Schema，直接 SQL 操作，不用 ORM）
- **缓存**：moka 内存缓存（模拟 kong.cache 的 TTL + 负缓存行为）
- **db-less 模式**：从 YAML/JSON 声明式配置文件加载到内存 HashMap
- **数据格式**：JSON（Admin API 交互）、kong.conf（key=value 配置文件）

### 外部集成

- **协议**：HTTP/HTTPS（代理 + Admin API）、WebSocket（Hybrid 模式 CP/DP 通信）、gRPC（代理透传）
- **认证**：TLS 双向认证（Hybrid 模式 mTLS）、Admin API RBAC
- **Lua 集成**：通过 mlua 嵌入 LuaJIT VM，运行 Kong Lua 插件

## 开发环境

### 构建工具

- **构建系统**：Cargo（workspace 模式）+ Makefile 封装常用命令
- **包管理**：Cargo + crates.io
- **开发流程**：`make check` 快速检查 → `make test` 测试 → `make release` 发布构建

### 常用开发命令（Makefile）

**构建：**

| 命令 | 作用 |
|------|------|
| `make build` | 编译整个 workspace（debug） |
| `make release` | Release 构建 |
| `make check` | 快速类型检查（不生成二进制，比 build 快） |
| `make build-crate crate=kong-router` | 编译单个 crate |

**测试：**

| 命令 | 作用 |
|------|------|
| `make test` | 运行所有测试 |
| `make test-crate crate=kong-router` | 运行单个 crate 的测试 |
| `make test-name name=test_route_match` | 按名称匹配运行测试 |
| `make test-verbose` | 运行测试并显示 stdout/stderr |
| `make test-integration` | 只运行集成测试 |

**启动 / 调试：**

| 命令 | 作用 |
|------|------|
| `make run` | 默认配置启动 |
| `make run-conf conf=./kong.conf` | 指定配置文件启动 |
| `make run-debug` | Debug 级别日志启动（`RUST_LOG=debug`） |
| `make run-trace` | Trace 级别日志启动（最详细） |
| `make run-mod-debug mod=kong_router` | 仅对指定模块开启 debug 日志 |

**代码质量：**

| 命令 | 作用 |
|------|------|
| `make fmt` | 格式化所有代码 |
| `make lint` | Clippy 静态分析（`-D warnings`） |
| `make quality` | 格式化 + lint 一起跑 |
| `make fmt-check` | 格式检查（CI 用，不修改文件） |

**Kong Manager GUI（前端）：**

| 命令 | 作用 |
|------|------|
| `make manager-install` | 安装前端依赖（pnpm install） |
| `make manager-build` | 构建静态文件到 `kong-manager/dist/` |
| `make manager-dev` | 开发模式启动（热更新，http://localhost:8080） |
| `make ADMIN_API=http://10.0.0.1:8001 manager-dev` | 指定 Admin API 地址启动 |
| `make manager-preview` | 预览生产构建效果 |

**依赖服务管理：**

| 命令 | 作用 |
|------|------|
| `make services-up` | 启动依赖服务（PostgreSQL 等） |
| `make services-down` | 停止依赖服务并清理数据卷 |
| `make services-logs` | 查看服务日志 |
| `source scripts/dependency_services/up.sh` | 交互式启动服务并导出环境变量（提供 `stop_services` 清理函数） |

**全栈启动：**

| 命令 | 作用 |
|------|------|
| `make dev` | 一键启动：依赖服务（PG）→ db bootstrap → cargo run（使用 kong.conf.default） |
| `make dev-dbless` | db-less 模式启动，无需 docker |
| `make dev-full` | 同时启动 kong-server（后台）+ kong-manager（前台） |

**Docker：**

| 命令 | 作用 |
|------|------|
| `make docker-build` | 构建 Docker 镜像（多阶段构建） |
| `make docker-push` | 推送镜像到 Registry |
| `make docker-run` | 运行 Docker 容器 |
| `make docker-stop` | 停止 Docker 容器 |

**清理：**

| 命令 | 作用 |
|------|------|
| `make clean` | 清理 Rust 构建产物 |
| `make manager-clean` | 清理前端依赖和构建产物 |
| `make clean-all` | 清理所有 |

**其他：**

| 命令 | 作用 |
|------|------|
| `make deps` | 查看依赖树 |
| `make members` | 列出 workspace 成员 |

### 代码质量

- **静态分析**：`cargo clippy`（通过 `make lint`）
- **格式化**：`cargo fmt` / rustfmt（通过 `make fmt`）
- **测试框架**：Rust 内置测试（`#[test]`、`#[tokio::test]`）
- **集成测试**：各 crate 的 `tests/` 目录

### 版本控制

- **VCS**：Git
- **分支策略**：主干开发（main 分支），功能分支按需创建

## 部署和分发

- **目标平台**：Linux（主要）、macOS（开发）
- **分发方式**：单一二进制文件（`kong-server`）或 Docker 镜像
- **安装要求**：PostgreSQL 15+（数据库模式），或无数据库（db-less 模式）
- **配置文件**：`kong.conf.default`（开发默认配置，兼容 Kong 格式）
- **���动方式**：
  - 开发：`make dev`（自动 bootstrap + 启动）或 `cargo run -- -c kong.conf.default`
  - 生产：`./target/release/kong-server -c /path/to/kong.conf`
  - Docker：`make docker-build && make docker-run`（多阶段构建，兼容 Kong 官方镜像布局）
- **数据库初始化**：`cargo run -- -c kong.conf.default db bootstrap`（必须在首次启动前执行）
- **Docker**：
  - `Dockerfile`：多阶段构建（builder + runtime Debian slim），兼容 Kong 官方用户/目录布局
  - `docker-entrypoint.sh`：支持 Docker Secrets（`KONG_*_FILE` 环境变量），并默认将 `KONG_ADMIN_LISTEN` 暴露为 `0.0.0.0:8001`
  - 端口：8000（HTTP 代理）、8443（HTTPS 代理）、8001（Admin API）、8444（Admin SSL）、8002（Kong Manager GUI）

## 技术约束

### 性能要求

- 单节点吞吐量 ≥ 原版 Kong
- P99 延迟 ≤ 原版 Kong
- 内存占用 ≤ 原版 Kong
- Pingora 多线程共享连接池优化上游复用

### 兼容性要求

- 配置文件格式与 Kong 完全兼容（kong.conf）
- Admin API 响应格式与 Kong 完全兼容
- 数据库 Schema 与 Kong 完全兼容（直接操作 Kong 的 PostgreSQL 表）
- Lua 插件接口（Handler + Schema + PDK）与 Kong 完全兼容

### 安全要求

- 核心代码无 unsafe Rust（FFI 边界除外）
- Lua 沙箱隔离（mlua 安全模式）
- TLS 1.2/1.3 支持
- Hybrid 模式 CP/DP 间 mTLS 双向认证

## 关键技术决策

| 决策 | 选择 | 原因 |
|------|------|------|
| 代理引擎 | Pingora（而非从零实现） | Cloudflare 生产验证，多线程共享连接池，完善的生命周期钩子 |
| Admin API 框架 | axum（而非 actix-web） | 与 Pingora 共享 tokio 运行时，类型安全路由，tower 中间件生态 |
| 数据库驱动 | sqlx 直接 SQL（而非 ORM） | 确保与 Kong 数据库 Schema 100% 一致，无 ORM 映射偏差 |
| 缓存库 | moka（而非 dashmap + 手动 TTL） | 内置 TTL、容量限制、高并发性能 |
| Lua 集成 | mlua + LuaJIT（而非 rlua） | 活跃维护、支持 LuaJIT、安全沙箱、与 async Rust 良好集成 |

## 已知局限

- **ngx.* 兼容层不完整**：仅覆盖内置 47 个插件使用到的 ngx API，第三方插件可能需要补充
- **Pingora 约束**：部分 Kong 行为（如 ngx.timer）需要映射到 tokio 异步任务，语义可能有细微差异
- **单数据库**：目前仅支持 PostgreSQL，Kong 已弃用的 Cassandra 支持不做实现
