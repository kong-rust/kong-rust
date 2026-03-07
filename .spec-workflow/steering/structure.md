# 项目结构

## 目录组织

```
kong-rust/
├── Cargo.toml                    # Workspace 根配置（定义 members 和共享依赖）
├── Cargo.lock                    # 依赖锁定文件（二进制项目需提交）
├── .gitignore                    # Git 忽略规则
├── crates/                       # 所有 Workspace 成员 crate
│   ├── kong-core/                # 核心数据模型和 trait（零业务依赖）
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # crate 入口，re-export 所有公共模块
│   │       ├── error.rs          # KongError 统一错误类型
│   │       ├── models/           # 与 Kong Schema 完全一致的数据模型
│   │       │   ├── mod.rs
│   │       │   ├── common.rs     # Protocol, ForeignKey, HashOn 等公共类型
│   │       │   ├── service.rs    # Service 模型
│   │       │   ├── route.rs      # Route 模型
│   │       │   ├── consumer.rs   # Consumer 模型
│   │       │   ├── upstream.rs   # Upstream + HealthcheckConfig 模型
│   │       │   ├── target.rs     # Target 模型
│   │       │   ├── plugin.rs     # Plugin 模型
│   │       │   ├── certificate.rs # Certificate 模型
│   │       │   ├── sni.rs        # SNI 模型
│   │       │   ├── ca_certificate.rs # CaCertificate 模型
│   │       │   └── vault.rs      # Vault 模型
│   │       └── traits/           # 核心 trait 接口
│   │           ├── mod.rs
│   │           ├── entity.rs     # Entity trait（table_name, primary_key）
│   │           ├── dao.rs        # Dao<T> trait（泛型 CRUD）
│   │           └── plugin.rs     # PluginHandler trait（生命周期方法）
│   │
│   ├── kong-config/              # 配置解析（兼容 kong.conf）
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs         # KongConfig 结构体（所有配置字段 + 默认值）
│   │       ├── parser.rs         # kong.conf 文件解析器（key=value, # 注释）
│   │       └── listen.rs         # ListenAddr 类型（解析 "0.0.0.0:8000 ssl" 格式）
│   │
│   ├── kong-db/                  # 数据库 DAO 层 + 缓存 + Migration
│   │   ├── Cargo.toml
│   │   ├── migrations/           # SQL migration 文件
│   │   │   └── core/
│   │   │       └── 000_base.sql  # 初始建表：schema_meta + 10 个核心表 + 索引
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── database.rs       # Database 结构体（PgPool 管理，仅负责连接池）
│   │       ├── migrations.rs     # Migration 引擎（schema_state/bootstrap/up/finish/reset）
│   │       ├── cache.rs          # KongCache（moka 缓存，模拟 kong.cache）
│   │       ├── dbless.rs         # db-less 模式（声明式配置加载到内存）
│   │       └── dao/
│   │           ├── mod.rs        # DAO 接口和通用逻辑
│   │           └── postgres.rs   # PostgreSQL DAO 实现（sqlx 直接 SQL）
│   │
│   ├── kong-router/              # 路由引擎
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # Router 统一入口（根据 flavor 选择引擎）
│   │       ├── traditional.rs    # 传统路由匹配（hosts/paths/methods/headers/snis 优先级）
│   │       ├── expressions.rs    # 表达式路由（ATC 表达式语法解析和匹配）
│   │       └── stream.rs         # Stream (L4) 路由引擎（source/dest CIDR + SNI 匹配）
│   │
│   ├── kong-proxy/               # 基于 Pingora 的代理引擎
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs            # KongProxy（ProxyHttp trait 实现，L7 HTTP 代理）
│   │   │   ├── stream.rs         # KongStreamProxy（ServerApp trait 实现，L4 TCP/TLS 代理）
│   │   │   ├── stream_tls.rs     # TLS ClientHello SNI 解析器（手动解析 TLS record）
│   │   │   ├── balancer.rs       # 负载均衡器（round-robin/least-conn/consistent-hashing/latency）
│   │   │   ├── health.rs         # 健康检查器（主动 + 被动）
│   │   │   └── tls.rs            # TLS 证书管理（SNI 匹配，上游 mTLS）
│   │   └── tests/
│   │       └── proxy_e2e.rs      # 代理端到端测试
│   │
│   ├── kong-plugin-system/       # 插件注册和执行框架
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs            # PluginSystem（注册、排序、链式执行）
│   │
│   ├── kong-lua-bridge/          # Lua 兼容层（mlua + LuaJIT）
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs            # LuaBridge 入口
│   │   │   ├── vm.rs             # LuaJIT VM 池管理（per-worker）
│   │   │   ├── loader.rs         # Lua 插件加载器 + ngx.* 兼容层
│   │   │   └── pdk/
│   │   │       └── mod.rs        # PDK 接口实现（kong.request/response/service/...）
│   │   └── tests/
│   │       └── lua_plugin_compat.rs # Lua 插件兼容性测试
│   │
│   ├── kong-admin/               # Admin API（axum）
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs            # Admin API 应用（路由注册、中间件、AppState）
│   │   │   └── handlers/
│   │   │       └── mod.rs        # CRUD handlers + 特殊端点
│   │   └── tests/
│   │       └── admin_api_compat.rs # Admin API 兼容性测试
│   │
│   ├── kong-cluster/             # 集群通信层（计划中，阶段 9）
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── role.rs           # ClusterRole 枚举
│   │       ├── tls.rs            # mTLS 双向认证
│   │       ├── cp/               # Control Plane
│   │       │   ├── mod.rs
│   │       │   ├── ws_server.rs  # WebSocket 服务端（cluster_listen）
│   │       │   ├── config_push.rs # 配置导出和全量推送
│   │       │   ├── client_manager.rs # DP 客户端状态管理
│   │       │   └── hash.rs       # 多级配置哈希计算
│   │       ├── dp/               # Data Plane
│   │       │   ├── mod.rs
│   │       │   ├── ws_client.rs  # WebSocket 客户端
│   │       │   ├── config_apply.rs # 配置接收和应用
│   │       │   ├── heartbeat.rs  # PING/PONG 心跳（30s 间隔）
│   │       │   └── reconnect.rs  # 断线重连（5-10s 随机延迟）
│   │       └── sync_v2/          # 增量同步（JSON-RPC 2.0）
│   │           ├── mod.rs
│   │           ├── rpc.rs        # JSON-RPC 2.0 编解码
│   │           ├── delta.rs      # Delta 计算和应用
│   │           └── version.rs    # 版本号管理
│   │
│   └── kong-server/              # 主入口二进制
│       ├── Cargo.toml
│       └── src/
│           └── main.rs           # 启动入口（配置加载 → DB → 路由 → 插件 → Proxy + Admin）
│
├── kong-manager/                 # Kong Manager GUI（官方开源前端）
│   ├── package.json              # 依赖定义（Vue + TypeScript + Vite）
│   ├── vite.config.ts            # Vite 构建配置（dev 端口 8080）
│   ├── src/                      # Vue 前端源码
│   └── dist/                     # 构建产物（gitignore）
│
├── kong.conf.default             # 开发环境默认配置（兼容 Kong 的 kong.conf 格式）
│
├── scripts/                      # 开发和测试脚本
│   └── dependency_services/      # 依赖服务管理（参考原版 Kong 结构）
│       ├── docker-compose-test-services.yml  # Docker Compose 服务定义
│       ├── common.sh             # 核心逻辑（启动/停止、端口提取、环境变量导出）
│       ├── up.sh                 # 入口脚本（source 调用，提供 stop_services）
│       └── 00-create-pg-db.sh    # PG 初始化脚本（创建 kong + kong_tests 库）
│
├── .claude/                      # Claude Code 配置
│   └── settings.local.json       # 本地权限设置
│
└── .spec-workflow/               # 规划和设计文档
    ├── specs/kong-rust/          # 功能规格
    │   ├── requirements.md       # 需求文档（R1-R9）
    │   ├── design.md             # 设计文档（组件 1-9）
    │   └── tasks.md              # 任务清单（阶段 1-9）
    ├── Implementation Logs/      # 已完成任务的实现日志
    ├── steering/                 # 项目导航文档
    │   ├── product.md            # 产品概述
    │   ├── tech.md               # 技术栈
    │   └── structure.md          # 项目结构（本文件）
    ├── approvals/                # 审批快照
    ├── templates/                # 文档模板
    └── archive/                  # 存档
```

## 命名规范

### 文件

- **Crate 名称**：`kebab-case`（如 `kong-core`, `kong-lua-bridge`）
- **Rust 源文件**：`snake_case.rs`（如 `health_check.rs`, `config_push.rs`）
- **模块目录**：`snake_case/`（如 `models/`, `handlers/`, `sync_v2/`）
- **测试文件**：`<描述性名称>.rs` 放在 `tests/` 目录（如 `proxy_e2e.rs`）

### 代码

- **类型/结构体/枚举**：`PascalCase`（如 `KongProxy`, `ClusterRole`, `HealthcheckConfig`）
- **函数/方法**：`snake_case`（如 `match_route`, `apply_full_config`）
- **常量**：`UPPER_SNAKE_CASE`（如 `CLUSTERING_PING_INTERVAL`, `MAX_RETRY`）
- **变量**：`snake_case`（如 `config_hash`, `dp_client`）
- **Trait**：`PascalCase`（如 `PluginHandler`, `Dao`, `Entity`, `ConfigApplier`）

## 导入规范

### 导入顺序

```rust
// 1. 标准库
use std::collections::HashMap;
use std::sync::Arc;

// 2. 外部 crate
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// 3. Workspace 内部 crate
use kong_core::models::Service;
use kong_db::Database;

// 4. 当前 crate 内部模块
use crate::handlers;
use super::config;
```

### 模块组织

- 每个 crate 的 `lib.rs` 负责 re-export 公共 API
- `mod.rs` 用于组织子模块（如 `models/mod.rs`, `handlers/mod.rs`）
- 测试使用 `tests/` 目录（集成测试）和 `#[cfg(test)] mod tests`（单元测试）

## 代码组织原则

### 文件内结构

```rust
// 1. 模块声明和导入
use ...;

// 2. 常量定义
const PING_INTERVAL: u64 = 30;

// 3. 类型/结构体定义
pub struct Foo { ... }

// 4. Trait 实现
impl SomeTrait for Foo { ... }

// 5. 固有方法
impl Foo {
    pub fn new() -> Self { ... }
    pub fn public_method() { ... }
    fn private_helper() { ... }
}

// 6. 单元测试
#[cfg(test)]
mod tests { ... }
```

### 模块边界

```
依赖方向（严格单向，禁止循环依赖）：

kong-core（最底层，零内部依赖）
    ↑
kong-config, kong-router, kong-plugin-system
    ↑
kong-db（依赖 kong-core + kong-config）
    ↑
kong-lua-bridge（依赖 kong-core + kong-db）
kong-admin（依赖 kong-core + kong-config + kong-db + kong-plugin-system）
kong-proxy（依赖除 kong-admin 外的所有 crate）
kong-cluster（依赖 kong-core + kong-config + kong-db）
    ↑
kong-server（顶层入口，依赖所有 crate）
```

**边界规则：**
- `kong-core` 只包含数据结构和 trait，不包含业务逻辑
- `kong-db` 不依赖 `kong-router` 或 `kong-proxy`（数据层不感知代理逻辑）
- `kong-admin` 不依赖 `kong-proxy`（Admin API 和代理引擎独立）
- `kong-lua-bridge` 不依赖 `kong-proxy`（Lua 桥接通过 trait 抽象解耦）

## 代码规模指导

- **单个文件**：建议 ≤ 500 行，超过时考虑拆分子模块
- **单个函数**：建议 ≤ 50 行，复杂逻辑抽取 helper 函数
- **嵌套深度**：建议 ≤ 4 层，深嵌套通过提前 return 或 match 展平
- **Crate 规模**：当单个 crate 超过 20 个文件时，考虑拆分

## 文档标准

- 所有 `pub` 类型和方法必须有 `///` 文档注释
- 复杂算法（如路由优先级排序、一致性哈希）需要行内注释说明逻辑
- 与 Kong 行为对应的实现需要标注参考的 Kong 源码路径
