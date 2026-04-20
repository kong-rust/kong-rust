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
| 8 | 集成、启动、优化 | 20 | 20 | 0 |
| 9 | Hybrid 模式 | 7 | 7 | 0 |
| 10 | Docker 镜像 | 6 | 6 | 0 |
| 11 | HTTP 代理性能优化 | 7 | 7 | 0 |
| 12 | 协议与 TLS 进阶 | 1 | 1 | 0 |
| 13 | 数据库兼容与 WebSocket | 2 | 2 | 0 |
| 14 | QA 测试与 Bug 修复 | 4 | 4 | 0 |
| 15 | AI Gateway — v1/responses | 1 | 1 | 0 |
| 16 | Admin API 补全 | 5 | 3 | 2 |
| 17 | 协议与代理进阶 | 2 | 1 | 1 |
| 18 | 安全与运维 | 3 | 0 | 3 |
| 19 | 可观测性与性能 | 2 | 0 | 2 |
| 20 | 优雅生命周期管理 | 1 | 1 | 0 |
| **合计** | | **85** | **77** | **8** |

> **2026-04-19 审计修正**（见下方任务描述中标注的 ⚠️）：
> - **阶段 8 任务数 19 → 20**：补入 8.12a（busted 兼容层）子任务，此前未计入概览表。
> - **4.3**：保留 [x]（HTTP/TCP 健康检查已完成），移除"gRPC 探测"虚报声明 —— 原声明无对应代码。
> - **6.3**：保留 [x]（7 个特殊端点已完成），移除 `/cache` 和 `/debug/node/log-level` 虚报 —— 这两项实际在阶段 16.3/16.4 为待办状态。
> - **8.15**：保留 [x]（TCP + TLS Passthrough 已完成），移除 "TLS Termination" 虚报 —— 实际存在 `// TODO`，正式交付归阶段 17.2。
> - **17.1**：测试数量 10 → 9（实际 `fn test_grpc_*` 函数 9 个）。
> - 文件组织说明（不影响完成度）：kong-admin 的 handlers 实际多数塞在 `handlers/mod.rs` 单文件（5459 行），非声明的多文件；kong-lua-bridge 的 PDK 命名空间集中在 `pdk/kong.rs`。

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
  - 主动检查（HTTP/TCP 探测）+ 被动检查（请求错误计数）
  - 文件：`crates/kong-proxy/src/health.rs`
  - ⚠️ 2026-04-19 审计修正：gRPC 健康探测尚未实现（health.rs 无 gRPC 代码路径），已移除原声明

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
  - /、/status、/config、/endpoints、/plugins/enabled、/schemas/*、/tags
  - 文件：`crates/kong-admin/src/handlers/mod.rs`（集中实现）、`crates/kong-admin/src/handlers/schemas.rs`
  - ⚠️ 2026-04-19 审计修正：`/cache` 和 `/debug/node/log-level` 未实现，已拆分至阶段 16.3 / 16.4 作为新任务

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

- [x] **8.12a** 构建 busted + spec.helpers 兼容层 `[R5]`
  - Phase 0 全部完成：375/375 (100%) — 8 个 spec 全部 0 failures, 0 errors
  - busted CLI + spec.helpers (1800+行) + mock upstream + 断言系统
  - Kong Lua shim 模块、FlexibleBody 提取器（多 Content-Type 支持）、ngx 全局 shim
  - 已复制：31 个 Kong 官方 spec 文件（admin_api 11、proxy 8、db 3、status 5、dbless 4）
  - 详见：`docs/implementation-logs/phase0-test-alignment.md`

### 8c：L4 Stream 代理

- [x] **8.13** Stream 路由引擎 `[R1]`
  - source/dest IP:Port CIDR 匹配 + SNI 通配符匹配，8 个单元测试
  - 文件：`crates/kong-router/src/stream.rs`

- [x] **8.14** TLS ClientHello SNI 解析器 `[R1]`
  - 手动解析 TLS record 提取 SNI，4 个单元测试
  - 文件：`crates/kong-proxy/src/stream_tls.rs`

- [x] **8.15** Stream 代理核心 `[R1]`
  - KongStreamProxy 实现 ServerApp trait，支持 TCP 和 TLS Passthrough 两种模式
  - 文件：`crates/kong-proxy/src/stream.rs`
  - ⚠️ 2026-04-19 审计修正：TLS Termination 尚未实现（`stream.rs:296` 处有 `// TODO`），正式交付归阶段 17.2

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

- [x] **9.1** kong-cluster crate 基础结构和 CP/DP 角色启动 `[R9]`
  - ClusterRole 枚举、SyncStatus、DataPlaneInfo、ConfigHashes、ClusterError，main.rs 角色分支启动
  - 文件：`crates/kong-cluster/src/lib.rs`, `crates/kong-config/src/lib.rs`, `crates/kong-server/src/main.rs`

- [x] **9.2** CP WebSocket 服务端 + Sync V1 配置推送 `[R9]`
  - ControlPlane（DP 注册/注销、broadcast channel 零拷贝广播、多级配置哈希、超时 DP 清理）、ClusterListenerTask（TCP listener + TLS acceptor + WS upgrade + handle_v1_connection）
  - 文件：`crates/kong-cluster/src/cp.rs`, `crates/kong-server/src/main.rs`

- [x] **9.3** DP WebSocket 客户端 + 配置接收应用 `[R9]`
  - DataPlane（WS URL 构建、basic_info、PING 心跳、重连延迟 5-10s 随机抖动、配置应用标记）、DpConnectorTask（磁盘缓存降级 → WS 连接 → V1 payload 解压解码 → config callback → 断线重连）
  - 文件：`crates/kong-cluster/src/dp.rs`, `crates/kong-server/src/main.rs`

- [x] **9.4** Sync V2 增量同步（JSON-RPC 2.0） `[R9]`
  - JsonRpcRequest/Response/Notification/Error、init/get_delta/notify_new_version/notify_validation_error、Snappy 压缩解压、encode/decode、handle_v2_connection
  - 文件：`crates/kong-cluster/src/protocol.rs`, `crates/kong-server/src/main.rs`

- [x] **9.5** TLS 双向认证 + 心跳/重连 `[R9]`
  - ClusterTlsConfig（shared/pki 两种模式、证书路径校验、SNI server_name）、build_cluster_tls_acceptor（OpenSSL SslAcceptor）、build_dp_tls_connector、30s PING interval + 5-10s 随机重连
  - 文件：`crates/kong-cluster/src/tls.rs`, `crates/kong-server/src/main.rs`

- [x] **9.6** /clustering/status Admin API 端点 `[R9, R3]`
  - GET /clustering/data-planes + GET /clustering/status，仅 control_plane 模式可用
  - 文件：`crates/kong-admin/src/handlers/clustering.rs`, `crates/kong-admin/src/lib.rs`

- [x] **9.7** 集成测试 — CP/DP 通信和配置同步 `[R9]`
  - 46 个测试：CP 单元级、DP 单元级、V1/V2 协议、Kong Lua 哈希兼容、TLS 配置、E2E WebSocket（CP↔DP V1 推送、完整连接循环、断线重连、V2 init→get_delta→notify_new_version）
  - 文件：`crates/kong-cluster/tests/integration_test.rs`

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

## 阶段 14：QA 测试与 Bug 修复

- [x] **14.1** 修复 Target weight 列类型不匹配 `[R4]`
  - target_schema() 中 weight 从 `.float()` 改为 `.integer()`，与 DB schema (INTEGER) 一致
  - 文件：`crates/kong-db/src/dao/postgres.rs`

- [x] **14.2** 修复 Docker 容器 workspace migration 冲突导致无限重启 `[R4]`
  - INSERT workspaces 从 `ON CONFLICT (id) DO NOTHING` 改为 `ON CONFLICT DO NOTHING`，兼容已有 Kong DB
  - 文件：`crates/kong-db/migrations/core/001_add_workspaces.sql`

- [x] **14.3** 修复 QA 发现的 11 个 Bug `[R1-R6]`
  - Admin API 404 空响应体、url shorthand 解析、preserve_host 端口号、timestamps 0.0、上游不可达 500 空响应、Kong 特征头、HTTPS-only 协议过滤、Service host 验证、外键冲突 400→409、Prometheus node_id/version
  - 文件：`crates/kong-admin/`, `crates/kong-proxy/`, `crates/kong-router/`, `crates/kong-db/`

- [x] **14.4** 修复最后 4 个 QA 问题 `[R1, R3, R5]`
  - ISSUE-005: 路由缓存键加入 headers，修复 header 路由条件被缓存绕过
  - ISSUE-009: 加权 round-robin 改为 GCD 交错分配，修复小请求数全命中同一 target
  - ISSUE-010: Prometheus log_serialize 补全 consumer/workspace/size 字段
  - ISSUE-013: preload 后保留 body buffer 避免重新缓冲触发超时
  - 修复 4 个路由器单元测试缺失 scheme 字段的已有 bug
  - 文件：`crates/kong-router/src/traditional.rs`, `crates/kong-proxy/src/balancer.rs`, `crates/kong-proxy/src/lib.rs`

## 阶段 15：AI Gateway — v1/responses 协议支持

- [x] **15.1** ai-proxy 支持 v1/responses API `[R1, R5]`
  - 分层架构：OpenAI pass-through 快速通道 + 跨 provider 降级/升级转换路径
  - 修复 Anthropic/Gemini provider 流式+非流式 function calling / tool_calls 支持
  - 新增 responses_format.rs 编解码器（请求降级、响应升级、流式事件状态机）
  - 支持 4 个 provider：OpenAI（pass-through）、Anthropic、Gemini、OpenAI-compat（translation）
  - Admin API schema 支持 route_type=llm/v1/responses
  - X-Kong-AI-Route-Type 响应头（调试辅助）
  - 文件：`crates/kong-ai/src/codec/responses_format.rs`（新建）、`crates/kong-ai/src/plugins/ai_proxy.rs`、`crates/kong-ai/src/plugins/context.rs`、`crates/kong-ai/src/provider/anthropic.rs`、`crates/kong-ai/src/provider/gemini.rs`

## 阶段 16：Admin API 补全

- [ ] **16.1** 实现 KeySet 实体（模型 + DAO + Admin API）`[R3, R4]`
  - 在 kong-core 定义 KeySet 模型（id, name, tags, created_at, updated_at）
  - 在 kong-db 实现 KeySetDao（schema 定义 + PgDao<KeySet>）
  - 在 kong-admin 实现 `/key_sets`、`/key_sets/{id}` CRUD 端点
  - 文件：`crates/kong-core/src/models/key_set.rs`, `crates/kong-db/src/dao/postgres.rs`, `crates/kong-admin/src/handlers/key_sets.rs`

- [ ] **16.2** 实现 Key 实体（模型 + DAO + Admin API）`[R3, R4]`
  - 在 kong-core 定义 Key 模型（id, set, name, kid, jwk, pem, tags, created_at, updated_at）
  - 在 kong-db 实现 KeyDao（schema 定义 + PgDao<Key>）
  - 在 kong-admin 实现 `/keys`、`/keys/{id}` CRUD 端点 + `/key_sets/{id}/keys` 嵌套端点
  - 文件：`crates/kong-core/src/models/key.rs`, `crates/kong-db/src/dao/postgres.rs`, `crates/kong-admin/src/handlers/keys.rs`

- [x] **16.3** 实现缓存管理端点 `[R3]`
  - `GET /cache/{key}` — 查询指定缓存条目（命中返回 JSON / 负缓存 / 未命中返回 404）
  - `DELETE /cache/{key}` — 删除指定缓存条目（幂等，始终 204）
  - `DELETE /cache` — 清空全部缓存
  - AdminState 新增共享 `Arc<KongCache>`，由 server main 统一实例化（容量来自 `mem_cache_size`）
  - 4 个集成测试：miss→404、hit→JSON、单条 DELETE、全量 DELETE
  - 文件：`crates/kong-admin/src/handlers/cache.rs`（新建）、`crates/kong-admin/src/lib.rs`、`crates/kong-server/src/main.rs`、`crates/kong-admin/tests/admin_api_compat.rs`

- [x] **16.4** 实现动态日志级别端点 `[R3]`
  - `GET /debug/node/log-level` — 获取当前日志级别（Kong 风格消息 `log level: info`）
  - `PUT /debug/node/log-level/{level}` — 运行时切换，支持 debug/info/notice/warn/error/crit/alert/emerg
  - `init_logging` 用 `tracing_subscriber::reload::Layer` 包裹 EnvFilter，闭包暴露为 `LogLevelUpdater`（类型擦除，admin crate 不依赖 tracing-subscriber）
  - 未知级别 → 400，未注入 updater → 503
  - 3 个集成测试：GET 返回当前级别、PUT 未知级别 400、PUT 无 updater 503
  - 文件：`crates/kong-admin/src/handlers/debug.rs`（新建）、`crates/kong-admin/src/lib.rs`、`crates/kong-server/src/main.rs`、`crates/kong-admin/tests/admin_api_compat.rs`

- [x] **16.5** 实现 Timers 端点 `[R3]`
  - `GET /timers` — 返回 Kong 兼容的计时器统计结构：`{ worker: {id, count}, stats: { sys: {total, runs, running, pending, waiting}, timers: {}, flamegraph: {running, pending, elapsed_time} } }`
  - Pingora + tokio 无 `resty-timer-ng` 等价物，故返回零值占位（后续可对接 `tokio::runtime::Handle::metrics()`）
  - 1 个集成测试：验证 Kong 形态 schema 完整性（worker/stats/sys 所有字段）
  - 文件：`crates/kong-admin/src/handlers/timers.rs`（新建）、`crates/kong-admin/src/lib.rs`、`crates/kong-admin/tests/admin_api_compat.rs`

## 阶段 17：协议与代理进阶

- [x] **17.1** 完整 gRPC 代理支持 `[R1]`
  - 新建 `grpc.rs` 模块：HTTP→gRPC 状态码映射（Kong 兼容）、gRPC 请求检测（content-type: application/grpc）、gRPC Trailers-Only 错误响应
  - 代理层集成：gRPC 请求自动检测、框架级错误返回 gRPC 格式（HTTP 200 + grpc-status/grpc-message）、不剥离 trailer 逐跳头、强制禁用 request/response body buffering（流式支持）
  - 已有基础设施：h2c 先验知识（明文 gRPC）、H2H1 ALPN（TLS gRPC）、server 端 h2c 启用、路由匹配 grpc→http 透明映射
  - 9 个新增测试：路由匹配（host/path/HTTPS/SNI）、路由共存、strip_path 约束、upstream 协议检测、gRPC 状态码映射
  - 文件：`crates/kong-proxy/src/grpc.rs`（新建）、`crates/kong-proxy/src/lib.rs`、`crates/kong-proxy/tests/proxy_e2e.rs`

- [ ] **17.2** Stream TLS Termination `[R8]`
  - 实现 L4 Stream 代理的 TLS 终止模式（当前仅支持 TLS Passthrough 和 TCP）
  - 使用 CertificateManager 按 SNI 选择证书，SslAcceptor 终止 TLS 后转发明文到上游
  - 补充 `stream.rs:296` 处的 TODO
  - 文件：`crates/kong-proxy/src/stream.rs`, `crates/kong-proxy/src/stream_tls.rs`

## 阶段 18：安全与运维

- [ ] **18.1** Admin API RBAC 支持 `[NFR]`
  - 实现基于角色的访问控制（与 Kong Enterprise RBAC 兼容）
  - 支持 admin 用户认证（basic-auth / token）
  - 端点级别权限检查中间件
  - 文件：`crates/kong-admin/src/rbac.rs`（新建）, `crates/kong-admin/src/app.rs`

- [ ] **18.2** Lua 沙箱隔离加固 `[NFR]`
  - 限制 Lua 插件可访问的系统 API（文件系统、网络、os.execute 等）
  - 内存和执行时间限制
  - 文件：`crates/kong-lua-bridge/src/vm.rs`

- [ ] **18.3** Proxy Cache 插件实现 `[R5]`
  - 集成 pingora-cache 实现 HTTP 响应缓存
  - 支持 memory 存储策略
  - 缓存命中/未命中响应头（X-Cache-Status）
  - 与 proxy-cache 插件 schema 对接
  - 文件：`crates/kong-proxy/src/cache.rs`（新建）, `crates/kong-plugin-system/`

## 阶段 19：可观测性与性能

- [ ] **19.1** OpenTelemetry 集成 `[NFR]`
  - 集成 opentelemetry-rust SDK，实现分布式追踪（span 上下文传播、导出器）
  - 支持 OTLP gRPC/HTTP 导出
  - 对接 config 中已有的 `tracing_instrumentations` 和 `tracing_sampling_rate` 配置
  - 文件：`crates/kong-proxy/src/telemetry.rs`（新建）, `crates/kong-config/src/config.rs`

- [ ] **19.2** 性能基准测试框架 `[NFR]`
  - 使用 criterion.rs 建立基准测试套件
  - 覆盖关键路径：路由匹配、插件链执行、代理转发全链路
  - 与 Kong 原版对比吞吐量和 P99 延迟的基线数据
  - 文件：`benches/`（新建目录）, `Cargo.toml`

## 阶段 20：优雅生命周期管理

- [x] **20.1** Graceful Shutdown 和连接排空 `[NFR]`
  - Pingora `Server::run_forever()` 内置 SIGINT/SIGTERM 处理，通过 `ShutdownWatch` 广播；Admin / CP / DP 等后台服务已响应 `shutdown.changed()`
  - 新增配置 `nginx_main_worker_shutdown_timeout`（Kong 同名参数，默认 10s，兼容 `10s` 带单位写法）
  - 映射到 Pingora `ServerConf.graceful_shutdown_timeout_seconds`（存量请求完成期）+ `grace_period_seconds=0`（立即停止接受新连接）
  - 用 `Server::new_with_opt_and_conf(None, conf)` 替换 `Server::new(None)`
  - 1 个单元测试：默认值 / 数字 / 带单位 / 非法值回落
  - 文件：`crates/kong-config/src/config.rs`（新增字段 + 解析 + 测试）、`crates/kong-server/src/main.rs`

### 已知问题（QA 发现，全部已修复 ✅）

以下 16 个问题由 QA 测试（2026-03-20）发现，已全部修复。完整原始报告未入库（原路径 `.gstack/qa-reports/qa-report-kong-rust-2026-03-20.md` 不存在于工作树），关键发现摘要见阶段 14.3 / 14.4 的任务描述。
