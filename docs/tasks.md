# 任务文档：Kong-Rust

## 进度概览

| 阶段 | 描述 | 任务数 | 完成 | 待办 |
|------|------|--------|------|------|
| 1 | 项目基础和核心模型 | 3 | 3 | 0 |
| 2 | 配置和数据库 | 6 | 6 | 0 |
| 3 | 路由引擎 | 3 | 3 | 0 |
| 4 | 代理引擎 | 3 | 3 | 0 |
| 5 | 插件系统 | 4 | 4 | 0 |
| 6 | Admin API | 4 | 4 | 0 |
| 7 | TLS 和证书管理 | 1 | 1 | 0 |
| 8 | 集成、启动、优化 | 19 | 18 | 1 |
| 9 | Hybrid 模式 | 7 | 0 | 7 |
| 10 | Docker 镜像 | 6 | 6 | 0 |
| 11 | HTTP 代理性能优化 | 7 | 7 | 0 |
| 12 | 协议与 TLS 进阶 | 1 | 1 | 0 |
| 13 | 数据库兼容与 WebSocket | 2 | 2 | 0 |
| **合计** | | **66** | **58** | **8** |

---

## 阶段 1：项目基础和核心模型

- [x] **1.1** 初始化 Rust Workspace 项目结构 `[NFR]`
  - 创建 Cargo.toml workspace 配置和 10 个 crate 目录
  - 文件：`Cargo.toml`, `crates/*/Cargo.toml`

- [x] **1.2** 定义核心数据模型 `[R1, R3]`
  - Service、Route、Consumer、Upstream、Target、Plugin、Certificate、Sni 等全部核心模型，字段与 Kong Schema 完全一致
  - 文件：`crates/kong-core/src/models/*.rs`

- [x] **1.3** 定义核心 Trait 接口 `[R1, R5]`
  - PluginHandler trait、Dao trait、Entity trait、KongError 统一错误类型
  - 文件：`crates/kong-core/src/traits/*.rs`, `crates/kong-core/src/error.rs`

## 阶段 2：配置和数据库

- [x] **2.1** 实现配置解析器 `[R6]`
  - 解析 kong.conf（key=value）、KONG_* 环境变量覆盖、ListenAddr 解析
  - 文件：`crates/kong-config/src/*.rs`

- [x] **2.2** 实现 PostgreSQL DAO 层 `[R4]`
  - sqlx 直接 SQL 实现全部实体 CRUD + 分页，不用 ORM
  - 文件：`crates/kong-db/src/dao/*.rs`, `crates/kong-db/src/lib.rs`

- [x] **2.3** 实现缓存层 `[R4]`
  - moka 内存缓存，模拟 kong.cache 行为（TTL + 负缓存）
  - 文件：`crates/kong-db/src/cache.rs`

- [x] **2.4** 实现 db-less 模式 `[R4]`
  - 从 YAML/JSON 声明式配置加载到内存 HashMap
  - 文件：`crates/kong-db/src/dbless.rs`

- [x] **2.5** 实现 Database Migration 机制 `[R4]`
  - schema_meta 版本追踪 + 000_base.sql 建表 + include_str! 编译期嵌入
  - 文件：`crates/kong-db/migrations/core/000_base.sql`, `crates/kong-db/src/migrations.rs`

- [x] **2.6** 实现完整 migrations 命令集 `[R4]`
  - schema_state/bootstrap/up/finish/reset 公开 API + db 子命令
  - 文件：`crates/kong-db/src/migrations.rs`, `crates/kong-server/src/main.rs`

## 阶段 3：路由引擎

- [x] **3.1** 实现传统路由匹配 `[R2]`
  - hosts/paths/methods/headers/snis 优先级匹配，通配符 host、正则 path
  - 文件：`crates/kong-router/src/traditional.rs`

- [x] **3.2** 实现表达式路由 `[R2]`
  - ATC 表达式语法解析和匹配
  - 文件：`crates/kong-router/src/expressions.rs`

- [x] **3.3** 实现路由器统一入口 `[R2]`
  - Router 统一管理 traditional/expressions，根据 router_flavor 切换
  - 文件：`crates/kong-router/src/lib.rs`

## 阶段 4：代理引擎

- [x] **4.1** 实现 Pingora 代理服务 `[R1]`
  - ProxyHttp trait 实现，Pingora 生命周期 → Kong 插件阶段映射
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] **4.2** 实现负载均衡器 `[R1]`
  - round-robin、least-conn、consistent-hashing（ketama）、latency 四种算法 + Target 权重
  - 文件：`crates/kong-proxy/src/balancer.rs`

- [x] **4.3** 实现健康检查器 `[R7]`
  - 主动检查（HTTP/TCP/gRPC 探测）+ 被动检查（请求错误计数）
  - 文件：`crates/kong-proxy/src/health.rs`

## 阶段 5：插件系统

- [x] **5.1** 实现插件注册表和执行框架 `[R5]`
  - 注册、配置验证、PRIORITY 排序、链式执行，全局/Service/Route/Consumer 四级合并
  - 文件：`crates/kong-plugin-system/src/*.rs`

- [x] **5.2** 实现 LuaJIT VM 管理 `[R5]`
  - mlua per-worker VM 池，获取/归还/清理
  - 文件：`crates/kong-lua-bridge/src/vm.rs`

- [x] **5.3** 实现 PDK 兼容层 `[R5]`
  - kong.request/response/service/client/ctx/log/cache/router/node/ip 全部命名空间
  - 文件：`crates/kong-lua-bridge/src/pdk/*.rs`

- [x] **5.4** 实现 Lua 插件加载器 + ngx.* 兼容层 `[R5]`
  - handler.lua/schema.lua 加载，LuaPluginHandler 实现 PluginHandler trait，ngx.* 兼容 API
  - 文件：`crates/kong-lua-bridge/src/loader.rs`, `crates/kong-lua-bridge/src/pdk/ngx.rs`

## 阶段 6：Admin API

- [x] **6.1** 实现 Admin API 基础框架 `[R3]`
  - axum 搭建，Kong 兼容分页/错误响应格式，泛型 CRUD handler
  - 文件：`crates/kong-admin/src/lib.rs`

- [x] **6.2** 实现核心实体 CRUD 端点 `[R3]`
  - 12 个实体 CRUD + 嵌套资源端点（/services/{id}/routes 等）
  - 文件：`crates/kong-admin/src/handlers/*.rs`

- [x] **6.3** 实现特殊 Admin API 端点 `[R3]`
  - /、/status、/config、/endpoints、/plugins/enabled、/schemas/*、/tags、/cache、/debug/node/log-level
  - 文件：`crates/kong-admin/src/handlers/info.rs`, `schemas.rs`, `tags.rs`, `cache.rs`, `debug.rs`

- [x] **6.4** 修复 Kong Manager SPA 刷新 404 `[R3]`
  - SPA fallback 到 index.html，保留静态资源服务
  - 文件：`crates/kong-admin/src/lib.rs`

## 阶段 7：TLS 和证书管理

- [x] **7.1** 实现 TLS 证书管理和 SNI 匹配 `[R8]`
  - Pingora TLS 回调中基于 SNI 选择证书，Service 级客户端证书 + 上游 TLS 验证
  - 文件：`crates/kong-proxy/src/tls.rs`

## 阶段 8：集成、启动和优化

### 8a：核心集成

- [x] **8.1** 实现主入口和启动流程 `[R1, R6]`
  - 配置加载 → DB → 路由 → 插件 → Proxy + Admin 监听，CLI 参数，优雅关闭
  - 文件：`crates/kong-server/src/main.rs`

- [x] **8.2** 集成测试 — Admin API 兼容性 `[R3]`
  - 全部 CRUD 端点请求/响应格式验证
  - 文件：`crates/kong-admin/tests/admin_api_compat.rs`

- [x] **8.3** 集成测试 — Lua 插件兼容性 `[R5]`
  - key-auth、rate-limiting、cors、request-transformer 等常用插件
  - 文件：`crates/kong-lua-bridge/tests/lua_plugin_compat.rs`

- [x] **8.4** 端到端测试 — 代理功能 `[R1, R2, R7]`
  - 路由匹配、strip_path、preserve_host、负载均衡、重试、超时、健康检查
  - 文件：`crates/kong-proxy/tests/proxy_e2e.rs`

- [x] **8.5** 日志输出到文件 `[R6]`
  - tracing + tracing-appender，文件 + stderr 双写，RUST_LOG 优先
  - 文件：`crates/kong-server/src/main.rs`

- [x] **8.6** 上游 TLS 完整功能 `[R8]`
  - CA 证书信任链（tls_verify + ca_certificates）+ mTLS 客户端证书
  - 文件：`crates/kong-proxy/src/lib.rs`, `crates/kong-server/src/main.rs`

- [x] **8.7** Admin API CUD 后动态刷新代理缓存 `[R1, R3]`
  - AdminState 持有 KongProxy clone，CUD 操作异步触发 refresh_proxy_cache
  - 文件：`crates/kong-admin/src/lib.rs`, `crates/kong-admin/src/handlers/mod.rs`

- [x] **8.8** 缓存刷新防抖 `[R1, R3]`
  - mpsc channel + 100ms debounce 窗口，HashSet 去重合并
  - 文件：`crates/kong-admin/src/lib.rs`

### 8b：处理链路重构 + 测试体系

- [x] **8.9** PhaseRunner 抽象层 + body_filter 接入
  - PhaseRunner 封装全阶段，body_filter 增加 end_of_stream，短路响应支持自定义 headers/body
  - 文件：`crates/kong-proxy/src/phases.rs`, `crates/kong-proxy/src/lib.rs`

- [x] **8.10** 修复 PDK 硬编码桩
  - PDK 从 __kong_req_data 读取真实数据，sync_ctx_from_lua() 同步修改回 RequestCtx
  - 文件：`crates/kong-lua-bridge/src/pdk/mod.rs`

- [x] **8.11** 补全负载均衡算法 + 健康检查集成
  - consistent-hashing（ketama 哈希环）、least-connections（加权），HealthChecker 集成到 select()
  - 文件：`crates/kong-proxy/src/balancer.rs`, `crates/kong-proxy/src/health.rs`

- [x] **8.12** 测试体系建设
  - 测试基础设施、10 个阶段链测试、20+ PDK 测试、run-cargo-test.sh 统一入口
  - 文件：`crates/kong-proxy/tests/`, `scripts/run-cargo-test.sh`

- [ ] **8.12a** 构建 busted + spec.helpers 兼容层 `[R5]`
  - 在不修改官方 Lua spec 源码前提下，运行 ai-proxy 官方测试
  - 初期范围仅覆盖 ai-proxy 官方 spec

### 8c：L4 Stream 代理

- [x] **8.13** Stream 路由引擎 `[R1]`
  - source/dest IP:Port CIDR 匹配 + SNI 通配符匹配，8 个单元测试
  - 文件：`crates/kong-router/src/stream.rs`

- [x] **8.14** TLS ClientHello SNI 解析器 `[R1]`
  - 手动解析 TLS record 提取 SNI，4 个单元测试
  - 文件：`crates/kong-proxy/src/stream_tls.rs`

- [x] **8.15** Stream 代理核心 `[R1]`
  - KongStreamProxy 实现 ServerApp trait，TCP/TLS Passthrough/TLS Termination 三种模式
  - 文件：`crates/kong-proxy/src/stream.rs`

- [x] **8.16** Stream Service 注册 + 路由热更新 `[R1]`
  - stream_listen 端口绑定，AdminState 同步更新 StreamRouter
  - 文件：`crates/kong-server/src/main.rs`, `crates/kong-admin/src/lib.rs`

### 8d：异步 DNS + 异步日志

- [x] **8.17** 异步 DNS 解析 `[R1]`
  - hickory-resolver 封装，IP 直连跳过、TTL 缓存、自定义 nameserver
  - 文件：`crates/kong-proxy/src/dns.rs`

- [x] **8.18** 异步 Access Log 写入 `[R1]`
  - bounded channel 异步写入，热路径 try_send 无锁，channel 满时丢弃不反压
  - 文件：`crates/kong-proxy/src/access_log.rs`

### 8e：Route 级别 Body Buffering

- [x] **8.19** Route 级别 request_buffering / response_buffering `[R1]`
  - RouteMatch → KongCtx 传递 buffering 标志，buffering=true 全量缓冲，false 流式
  - 文件：`crates/kong-router/src/*.rs`, `crates/kong-proxy/src/lib.rs`

## 阶段 9：Hybrid 模式和集群通信

- [-] **9.1** kong-cluster crate 基础结构和 CP/DP 角色启动 `[R9]`
  - ClusterRole 枚举，集群配置项，main.rs 角色分支启动
  - 文件：`crates/kong-cluster/src/`, `crates/kong-config/src/lib.rs`, `crates/kong-server/src/main.rs`

- [ ] **9.2** CP WebSocket 服务端 + Sync V1 配置推送 `[R9]`
  - WebSocket 服务端（cluster_listen）、配置导出 GZIP 推送、DP 客户端管理、多级配置哈希
  - 文件：`crates/kong-cluster/src/cp/`

- [ ] **9.3** DP WebSocket 客户端 + 配置接收应用 `[R9]`
  - WebSocket 连接 CP、全量配置应用、三线程模型、本地配置缓存
  - 文件：`crates/kong-cluster/src/dp/ws_client.rs`, `crates/kong-cluster/src/dp/config_apply.rs`

- [ ] **9.4** Sync V2 增量同步（JSON-RPC 2.0） `[R9]`
  - JSON-RPC 2.0 编解码、hello/get_delta/notify_new_version/notify_validation_error、x-snappy-framed
  - 文件：`crates/kong-cluster/src/sync_v2/`

- [ ] **9.5** TLS 双向认证 + 心跳/重连 `[R9]`
  - cluster_cert mTLS、30s PING 心跳（MD5 哈希负载）、45s 超时、5-10s 随机延迟重连
  - 文件：`crates/kong-cluster/src/tls.rs`, `crates/kong-cluster/src/dp/heartbeat.rs`, `crates/kong-cluster/src/dp/reconnect.rs`

- [ ] **9.6** /clustering/status Admin API 端点 `[R9, R3]`
  - GET /clustering/status 返回已连接 DP 状态，仅 control_plane 模式可用
  - 文件：`crates/kong-admin/src/handlers/clustering.rs`

- [ ] **9.7** 集成测试 — CP/DP 通信和配置同步 `[R9]`
  - Sync V1/V2 推送、心跳超时、断线重连、缓存配置降级
  - 文件：`tests/cluster_e2e.rs`

## 阶段 10：Docker 镜像构建

- [x] **10.1** docker-start 和 health CLI 子命令
  - docker-start = migrations + start，health = HTTP GET /status 检查
  - 文件：`crates/kong-server/src/main.rs`

- [x] **10.2** Dockerfile 多阶段构建
  - builder（Rust 编译）→ runtime（Debian slim），兼容 Kong 官方用户/目录布局
  - 文件：`Dockerfile`

- [x] **10.3** docker-entrypoint.sh
  - Docker Secrets（KONG_*_FILE 环境变量），兼容 Kong 官方行为
  - 文件：`docker-entrypoint.sh`

- [x] **10.4** .dockerignore
  - 排除 target/、.git/、node_modules/
  - 文件：`.dockerignore`

- [x] **10.5** Makefile docker 目标
  - docker-build/push/run/stop，支持 DOCKER_TAG/DOCKER_REGISTRY 变量
  - 文件：`Makefile`

- [x] **10.6** Docker 端口语义 + Admin API 暴露 `[R3, R6]`
  - 默认 KONG_ADMIN_LISTEN=0.0.0.0:8001，8001=Admin API，8002=Kong Manager GUI
  - 文件：`docker-entrypoint.sh`

## 阶段 11：HTTP 代理性能优化

- [x] **11.1** RouteMatch 类型优化
  - protocols → Arc<Vec<String>>，path_handling → 枚举，消除每次匹配的堆分配
  - 文件：`crates/kong-router/src/*.rs`

- [x] **11.2** 消除重复请求头解析
  - 合并为 populate_and_build_route_ctx()，单次头遍历，消除 ~20 次 String 分配
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] **11.3** 路由匹配 LRU 缓存
  - moka::sync::Cache，键=(method, host, uri)，容量 1024
  - 文件：`crates/kong-router/src/traditional.rs`, `crates/kong-router/src/expressions.rs`

- [x] **11.4** 插件链预计算 + Arc clone 消除
  - plugin_chains HashMap 预计算，resolved_plugins 改为 Arc<Vec>
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] **11.5** Service 超时应用到 HttpPeer
  - upstream_peer() 设置 connect/read/write timeout
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] **11.6** 大 body 落盘保护
  - SpillableBuffer：内存阈值 10MB，超过溢出到 tempfile
  - 文件：`crates/kong-proxy/src/spillable_buffer.rs`, `crates/kong-proxy/src/lib.rs`

- [x] **11.7** chunk 间隔超时保护
  - body chunk 间隔超 60s 返回错误终止请求
  - 文件：`crates/kong-proxy/src/lib.rs`

## 阶段 12：协议与 TLS 支持进阶

- [x] **12.1** HTTP/2 支持（ALPN）
  - Downstream add_tls_with_settings + enable_h2()，Upstream ALPN::H2H1
  - 文件：`crates/kong-server/src/main.rs`, `crates/kong-proxy/src/lib.rs`

## 阶段 13：数据库兼容性与 WebSocket 代理修复

- [x] **13.1** 添加 workspaces 表和 ws_id 列支持 `[R4]`
  - workspaces 表 + 10 个实体表 ws_id 列 + 迁移自动设置默认 workspace
  - 文件：`crates/kong-db/migrations/core/001_add_workspaces.sql`, `crates/kong-core/src/models/*.rs`, `crates/kong-db/src/dao/postgres.rs`

- [x] **13.2** 修复 WebSocket 代理握手头转发 `[R1]`
  - 透传所有 sec-websocket-* 握手头到上游
  - 文件：`crates/kong-proxy/src/lib.rs`
