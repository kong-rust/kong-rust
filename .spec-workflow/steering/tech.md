# 技术栈

## 项目类型

高性能 API 网关 / 反向代理服务器，兼容 Kong Gateway 的 Rust 重写版本。

## 核心技术

### 语言

- **语言**：Rust（Edition 2021，最低版本 1.84）
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
| **日志** | `tracing` + `tracing-subscriber` | workspace | 结构化日志和追踪 |
| **错误处理** | `thiserror` / `anyhow` | workspace | 错误类型定义和传播 |
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

- **构建系统**：Cargo（`cargo build --workspace`）
- **包管理**：Cargo + crates.io
- **开发流程**：`cargo check` 快速检查 → `cargo test` 测试 → `cargo build --release` 发布构建

### 代码质量

- **静态分析**：`cargo clippy`
- **格式化**：`cargo fmt`（rustfmt）
- **测试框架**：Rust 内置测试（`#[test]`、`#[tokio::test]`）
- **集成测试**：各 crate 的 `tests/` 目录

### 版本控制

- **VCS**：Git
- **分支策略**：主干开发（main 分支），功能分支按需创建

## 部署和分发

- **目标平台**：Linux（主要）、macOS（开发）
- **分发方式**：单一二进制文件（`kong-server`）
- **安装要求**：PostgreSQL 15+（数据库模式），或无数据库（db-less 模式）
- **运行命令**：`kong-server -c /path/to/kong.conf`

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
