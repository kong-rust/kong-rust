# Kong-Rust

使用 Rust 完全重写的高性能 API 网关，100% 兼容 [Kong Gateway](https://github.com/Kong/kong)。零成本替换，无需修改任何现有配置。

## 为什么选择 Kong-Rust？

Kong ���全球最流行的开源 API 网关，但它运行在 LuaJIT + OpenResty 之上。Kong-Rust 使用 Rust 和 [Cloudflare Pingora](https://github.com/cloudflare/pingora) 重写了核心引擎，同时保持与 Kong 的配置、Admin API、数据库 Schema 和 Lua 插件生态的 **100% 兼容性**。

| | Kong (Lua/OpenResty) | Kong-Rust |
|---|---|---|
| **代理引擎** | OpenResty (Nginx + LuaJIT) | Pingora (Rust, 多线程) |
| **Admin API** | Lapis (Lua) | axum (Rust) |
| **数据库驱动** | pgmoon (Lua) | sqlx (Rust, 异步) |
| **Lua 插件** | 原生运行 | mlua (LuaJIT 绑定) |
| **内存安全** | 手动管理 (GC + FFI) | Rust 所有权系统 |

## 核心特性

- **完全兼容 Kong** — 数据模型、Admin API、`kong.conf` 配置格式、声明式配置（YAML/JSON）、Lua 插件接口（PDK + `ngx.*`）完全一致
- **高性能代理** — Pingora 多线程架构，共享连接池
- **双路由引擎** — 支持 `traditional_compatible` 和 `expressions` 两种路由模式
- **Lua 插件支持** — 通过 mlua + LuaJIT 运行全部 47 个 Kong 内置 Lua 插件
- **负载均衡与健康检查** — 轮询、一致性哈希、主动/被动健康检查
- **TLS 终止与 SNI** — 证书管理和基于 SNI 的路由
- **L4 Stream 代理** — TCP/TLS Passthrough 四层代理，支持 SNI 和 source/dest CIDR 路由
- **Kong Manager UI** — 兼容官方 Kong Manager 前端管理界面
- **多数据源** — PostgreSQL 数据库模式或 db-less 声明式配置模式
- **Hybrid 模式** — Control Plane / Data Plane 分离部署（规划中）

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

- Rust 1.84+（含 Cargo）
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
| `admin_listen` | `0.0.0.0:8001` | Admin API 监听地址 |
| `stream_listen` | `off` | L4 Stream 代理监听（如 `0.0.0.0:9000`） |
| `database` | `postgres` | 数据库模式（`postgres` 或 `off`） |
| `pg_host` | `127.0.0.1` | PostgreSQL 主机 |
| `pg_port` | `5432` | PostgreSQL 端口 |
| `pg_database` | `kong` | PostgreSQL 数据库名 |
| `router_flavor` | `traditional_compatible` | 路由引擎（`traditional_compatible` 或 `expressions`） |

支持 `KONG_` 前缀的环境变量覆盖（例如 `KONG_PG_PORT=5433`）。

## 开发

| 命令 | 说明 |
|------|------|
| `make build` | 编译（debug） |
| `make check` | 快速类型检查 |
| `make test` | 运行所有测试 |
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

| 阶段 | 状态 | 说明 |
|------|------|------|
| 1. 核心模型 | 已完成 | 数据模型、trait、配置解析 |
| 2. 数据库 | 已完成 | PostgreSQL DAO、缓存、db-less、migration |
| 3. 路由引擎 | 已完成 | Traditional + Expressions 路由 |
| 4. 代理引擎 | 已完成 | Pingora 集成、负载均衡、健康检查 |
| 5. 插件系统 | 已完成 | 插件注册、Lua Bridge、PDK |
| 6. Admin API | 已完成 | 完整 CRUD、嵌套端点、Kong Manager 支持 |
| 7. TLS | 已完成 | 证书管理、SNI 路由 |
| 8. 集成测试 | 已完成 | 端到端测试、访问日志 |
| 8c. Stream 代理 | 已完成 | L4 TCP/TLS Passthrough 代理、SNI/CIDR 路由 |
| 9. Hybrid 模式 | 规划中 | CP/DP 集群通信 |

## 许可证

Apache-2.0

## 致谢

- [Kong Gateway](https://github.com/Kong/kong) — 本项目兼容的 API 网关
- [Pingora](https://github.com/cloudflare/pingora) — Cloudflare 的 Rust HTTP 代理框架
- [axum](https://github.com/tokio-rs/axum) — Rust Web 框架
- [mlua](https://github.com/mlua-rs/mlua) — Rust 的 Lua/LuaJIT 绑定
