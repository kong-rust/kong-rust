# 任务文档：Kong-Rust

## 阶段 1：项目基础和核心模型

- [x] 1.1 初始化 Rust Workspace 项目结构
  - 创建 Cargo.toml workspace 配置和所有 crate 目录
  - 文件：`Cargo.toml`, `crates/kong-core/Cargo.toml`, `crates/kong-config/Cargo.toml`, `crates/kong-db/Cargo.toml`, `crates/kong-router/Cargo.toml`, `crates/kong-proxy/Cargo.toml`, `crates/kong-plugin-system/Cargo.toml`, `crates/kong-lua-bridge/Cargo.toml`, `crates/kong-admin/Cargo.toml`, `crates/kong-server/Cargo.toml`
  - 目的：建立项目骨架，确保所有 crate 可编译
  - _Requirements: NFR-代码架构_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 workspace 项目结构设计 | Task: 创建 Kong-Rust 的 Rust workspace 项目结构，包含 10 个 crate（kong-core, kong-config, kong-db, kong-router, kong-proxy, kong-plugin-system, kong-lua-bridge, kong-admin, kong-server）。参考设计文档 .spec-workflow/specs/kong-rust/design.md 中的 Workspace 结构部分。每个 crate 创建基本的 Cargo.toml（含必要依赖）和 src/lib.rs（或 src/main.rs）。根 Cargo.toml 定义 workspace members。 | Restrictions: 不要添加任何业务逻辑代码，只搭建项目骨架。确保 cargo build 能通过。 | Success: cargo build --workspace 编译成功，所有 crate 被正确识别。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 1.2 定义核心数据模型（kong-core）
  - 文件：`crates/kong-core/src/models/*.rs`
  - 定义 Service、Route、Consumer、Upstream、Target、Plugin、Certificate、Sni、CaCertificate、KeySet、Key、Vault 等所有核心模型
  - 所有字段定义必须与 Kong 源码（/Users/dawxy/proj/kong/kong/db/schema/entities/*.lua）完全一致
  - 实现 serde 序列化/反序列化，JSON 格式与 Kong API 响应一致
  - _Leverage: /Users/dawxy/proj/kong/kong/db/schema/entities/*.lua_
  - _Requirements: R1, R3_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，精通数据建模和 serde | Task: 在 kong-core crate 中定义所有核心数据模型，参考 Kong 源码 /Users/dawxy/proj/kong/kong/db/schema/entities/ 目录下的 Lua schema 定义。需要创建的模型包括：Service、Route、Consumer、Upstream、Target、Plugin、Certificate、Sni、CaCertificate、KeySet、Key、Vault、Workspace。参考设计文档 .spec-workflow/specs/kong-rust/design.md 中的数据模型部分。每个模型都需要 derive Serialize, Deserialize, Clone, Debug。所有字段名和类型必须与 Kong schema 完全一致。同时定义辅助类型如 Protocol、LbAlgorithm、HashOn、PathHandling、ForeignKey、HealthcheckConfig 等枚举和结构体。 | Restrictions: 不要遗漏任何字段。UUID 使用 uuid crate，时间戳使用 i64（Unix 秒）。JSON 序列化时字段名使用 snake_case（与 Kong 一致）。不要添加 Kong 中不存在的字段。 | _Leverage: /Users/dawxy/proj/kong/kong/db/schema/entities/services.lua, routes.lua, consumers.lua, upstreams.lua, targets.lua, plugins.lua, certificates.lua, snis.lua, ca_certificates.lua, keys.lua, key_sets.lua, vaults.lua | Success: 所有模型编译通过，字段与 Kong schema 100% 一致。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 1.3 定义核心 Trait 接口（kong-core）
  - 文件：`crates/kong-core/src/traits/*.rs`, `crates/kong-core/src/error.rs`
  - 定义 PluginHandler trait（含所有生命周期方法）、Dao trait（泛型 CRUD）、Entity trait
  - 定义统一错误类型 KongError
  - _Leverage: 设计文档中的 Trait 定义_
  - _Requirements: R1, R5_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 trait 设计和错误处理 | Task: 在 kong-core crate 中定义核心 trait 接口。参考设计文档 .spec-workflow/specs/kong-rust/design.md。1) PluginHandler trait：包含 priority(), version(), init_worker(), certificate(), rewrite(), access(), response(), header_filter(), body_filter(), log() 等方法，大部分方法有默认空实现。2) `Dao<T>` trait：泛型数据访问接口，包含 insert, select, select_by_key, page, update, upsert, delete 方法。3) Entity trait：标记 trait，提供 table_name(), primary_key() 等方法。4) KongError 统一错误类型：包含 DatabaseError, ValidationError, NotFound, PluginError, ConfigError 等变体。 | Restrictions: 所有 trait 方法使用 async fn（使用 async-trait crate 或 Rust 原生 async trait）。PluginHandler 的阶段方法签名需要包含 PluginConfig 和 RequestCtx 参数。 | Success: 所有 trait 编译通过，trait 定义清晰完整。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 2：配置和数据库

- [x] 2.1 实现配置解析器（kong-config）
  - 文件：`crates/kong-config/src/*.rs`
  - 解析 kong.conf 格式配置文件（key=value 格式，# 注释）
  - 支持 KONG_* 环境变量覆盖
  - 支持所有 Kong 配置项（proxy_listen, admin_listen, database, pg_*, plugins, router_flavor 等）
  - _Leverage: /Users/dawxy/proj/kong/kong.conf.default, /Users/dawxy/proj/kong/kong/conf_loader/_
  - _Requirements: R6_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长配置系统设计 | Task: 实现 kong-config crate，解析与 Kong 完全兼容的配置文件。参考 /Users/dawxy/proj/kong/kong.conf.default 获取所有配置项及默认值，参考 /Users/dawxy/proj/kong/kong/conf_loader/ 了解解析逻辑。1) KongConfig 结构体包含所有配置字段及默认值。2) parser.rs 实现 kong.conf 文件解析（key = value 格式，支持 # 注释）。3) env.rs 实现 KONG_* 环境变量覆盖（KONG_PG_HOST -> pg_host）。4) ListenAddr 类型支持解析如 '0.0.0.0:8000 ssl' 格式。 | Restrictions: 配置项名称和默认值必须与 Kong 完全一致。不要硬编码，从文件和环境变量动态加载。 | Success: 能正确解析 Kong 的 kong.conf.default 文件，环境变量覆盖生效。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 2.2 实现 PostgreSQL DAO 层（kong-db）
  - 文件：`crates/kong-db/src/dao/*.rs`, `crates/kong-db/src/lib.rs`
  - 使用 sqlx 实现所有核心实体的 CRUD 操作
  - SQL 查询直接操作 Kong 的数据库表（不引入 ORM）
  - 实现分页查询（与 Kong 分页格式一致）
  - _Leverage: /Users/dawxy/proj/kong/kong/db/strategies/postgres/, kong-core 模型定义_
  - _Requirements: R4_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 数据库工程师，精通 sqlx 和 PostgreSQL | Task: 实现 kong-db crate 的 PostgreSQL DAO 层。参考 /Users/dawxy/proj/kong/kong/db/strategies/postgres/ 了解 Kong 的 SQL 查询模式。1) Database 结构体：管理 PgPool 连接池，提供各实体 DAO 访问方法。2) 为每个核心实体（Service, Route, Consumer, Upstream, Target, Plugin, Certificate, Sni, CaCertificate, KeySet, Key, Vault）实现 Dao trait。3) SQL 查询使用 sqlx::query_as! 宏，表名和字段名与 Kong 数据库完全一致。4) 分页实现：支持 size 和 offset 参数，返回 `Page&lt;T&gt;` 结构体（含 data 和 next offset）。5) 支持按外键查询（如 Service 下的所有 Routes）。 | Restrictions: 不要使用 ORM，直接写 SQL。表名和列名必须与 Kong 数据库一致。UUID 主键、created_at/updated_at 时间戳。 | Success: 所有 DAO 的 CRUD 操作可正确执行，分页格式与 Kong 一致。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 2.3 实现缓存层（kong-db）
  - 文件：`crates/kong-db/src/cache.rs`
  - 使用 moka 实现内存缓存，模拟 Kong 的 kong.cache 行为
  - 支持 cache_key 生成规则与 Kong 一致
  - 支持 TTL 和容量配置
  - _Leverage: /Users/dawxy/proj/kong/kong/cache/_
  - _Requirements: R4_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长缓存系统设计 | Task: 在 kong-db crate 中实现内存缓存层。参考 /Users/dawxy/proj/kong/kong/cache/ 了解 Kong 的缓存机制。1) KongCache 结构体：基于 moka::sync::Cache 实现。2) 支持 get/set/delete/purge 操作。3) cache_key 生成规则与 Kong 一致（实体类型:主键 或 实体类型:唯一键名:值）。4) 支持 neg_ttl（负缓存 TTL）。5) 支持 mem_cache_size 配置。 | Restrictions: 缓存 key 格式必须与 Kong 一致。线程安全。 | Success: 缓存读写正确，TTL 过期生效。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 2.4 实现 db-less 模式（kong-db）
  - 文件：`crates/kong-db/src/dbless.rs`
  - 支持从 YAML/JSON 声明式配置文件加载所有实体数据到内存
  - 格式与 Kong 的 declarative config 完全兼容
  - _Leverage: /Users/dawxy/proj/kong/kong/db/declarative/_
  - _Requirements: R4_

- [x] 2.5 实现 Database Migration 机制（kong-db）
  - 文件：`crates/kong-db/migrations/core/000_base.sql`（新建）, `crates/kong-db/src/migrations.rs`（新建）, `crates/kong-db/src/database.rs`（修改）, `crates/kong-db/src/lib.rs`（修改）, `crates/kong-server/src/main.rs`（修改）
  - 实现 SQL migration 引擎：schema_meta 版本追踪表 + run_migrations() 执行逻辑
  - 000_base.sql 创建 10 个核心实体表（按外键依赖排序）+ 索引
  - Database::connect() 成功后自动执行 migration
  - SQL 通过 include_str! 编译期嵌入，与 Kong 的 schema_meta 表结构兼容
  - _Requirements: R4_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长数据序列化 | Task: 在 kong-db crate 中实现 db-less 模式。参考 /Users/dawxy/proj/kong/kong/db/declarative/ 了解 Kong 的声明式配置格式。1) DeclarativeConfig 结构体：解析 YAML/JSON 格式的声明式配置。2) 支持 _format_version 字段。3) 将所有实体加载到内存中的 HashMap 结构。4) 实现只读的 Dao trait（insert/update/delete 返回错误）。5) 通过 /config POST 端点支持运行时更新。 | Restrictions: 配置格式必须与 Kong 的声明式配置完全兼容。db-less 模式下写操作应返回明确错误。 | Success: 能正确加载 Kong 导出的声明式配置文件。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 2.6 实现完整的 migrations 命令集（kong-db + kong-server）
  - 重构 migrations.rs：删除 run_migrations()，新增 schema_state/bootstrap/up/finish/reset 公开 API
  - 移除 Database::connect() 中的自动 migration
  - 扩展 kong-server 的 db 子命令：Bootstrap/Up/Finish/List/Reset/Status
  - start 启动时增加 schema 状态检查，未 bootstrap 或有新 migration 时报错
  - 文件：`crates/kong-db/src/migrations.rs`, `crates/kong-db/src/database.rs`, `crates/kong-db/src/lib.rs`, `crates/kong-server/src/main.rs`
  - _Requirements: R4_

## 阶段 3：路由引擎

- [x] 3.1 实现传统路由匹配（kong-router）
  - 文件：`crates/kong-router/src/traditional.rs`, `crates/kong-router/src/priority.rs`
  - 实现基于 hosts/paths/methods/headers/snis 的路由匹配
  - 优先级排序规则必须与 Kong 完全一致
  - 支持通配符 host 匹配（*.example.com）和路径正则匹配
  - _Leverage: /Users/dawxy/proj/kong/kong/router/traditional.lua, /Users/dawxy/proj/kong/kong/router/utils.lua_
  - _Requirements: R2_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长路由算法和模式匹配 | Task: 实现 kong-router crate 的传统路由匹配引擎。仔细参考 /Users/dawxy/proj/kong/kong/router/traditional.lua 和 /Users/dawxy/proj/kong/kong/router/utils.lua 确保匹配行为完全一致。1) TraditionalRouter 结构体：维护路由表。2) 匹配优先级：hosts > paths > methods > headers > snis（与 Kong 排序规则完全一致）。3) Host 匹配：支持精确匹配和通配符匹配（*.example.com）。4) Path 匹配：支持前缀匹配和正则匹配，regex_priority 控制正则优先级。5) path_handling 支持 v0 和 v1 两种模式。6) 路由表支持增量重建。 | Restrictions: 匹配优先级和结果必须与 Kong 完全一致，这是兼容性的核心。需要编写充分的单元测试。 | Success: 路由匹配结果与 Kong 完全一致，所有边界情况都正确处理。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 3.2 实现表达式路由（kong-router）
  - 文件：`crates/kong-router/src/expressions.rs`
  - 实现 Kong 的 ATC 表达式路由语法解析和匹配
  - _Leverage: /Users/dawxy/proj/kong/kong/router/expressions.lua, /Users/dawxy/proj/kong/kong/router/atc.lua_
  - _Requirements: R2_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长表达式解析和编译器前端 | Task: 实现 kong-router crate 的表达式路由引擎。参考 /Users/dawxy/proj/kong/kong/router/expressions.lua 和 /Users/dawxy/proj/kong/kong/router/atc.lua。1) 表达式语法解析器：支持 Kong 的 ATC（Adaptive Trie Collection）表达式语法。2) 支持的运算符：==, !=, ~, in, not in, &&, ||。3) 支持的字段：http.method, http.host, http.path, http.headers.*, net.protocol, tls.sni 等。4) ExpressionsRouter 结构体：编译表达式并执行匹配。5) priority 字段控制表达式路由的优先级。 | Restrictions: 表达式语法必须与 Kong 完全兼容。 | Success: 能正确解析和匹配 Kong 格式的路由表达式。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 3.3 实现路由器统一入口和路由表管理（kong-router）
  - 文件：`crates/kong-router/src/lib.rs`
  - Router 结构体统一管理 traditional 和 expressions 两种路由风格
  - 根据 router_flavor 配置选择路由引擎
  - 支持从数据库加载路由表和增量更新
  - _Requirements: R2_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师 | Task: 实现 kong-router crate 的统一路由入口。1) Router 结构体：根据 router_flavor 配置（traditional, expressions, traditional_compatible）选择使用 TraditionalRouter 或 ExpressionsRouter。2) match_route() 方法：接收 RequestContext 返回 RouteMatch（包含匹配的 Route、Service、matched_path 等）。3) rebuild() 方法：从 Route 和 Service 列表重建路由表。4) traditional_compatible 模式下两种引擎的协调逻辑。 | Restrictions: 线程安全，使用 `Arc&lt;RwLock&gt;` 包装路由表以支持热更新。 | Success: Router 能根据配置选择正确的引擎并完成路由匹配。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 4：代理引擎

- [x] 4.1 实现 Pingora 代理服务（kong-proxy）
  - 文件：`crates/kong-proxy/src/server.rs`, `crates/kong-proxy/src/service.rs`
  - 实现 Pingora 的 ProxyHttp trait
  - 将 Pingora 请求生命周期映射到 Kong 插件阶段
  - 实现请求上下文 KongRequestCtx
  - _Leverage: Pingora 文档和示例_
  - _Requirements: R1_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，精通 Pingora 框架 | Task: 实现 kong-proxy crate 的核心代理服务。参考设计文档 .spec-workflow/specs/kong-rust/design.md 中的 Pingora 生命周期映射表。1) KongProxy 结构体：持有 Router、Database、PluginSystem 的 Arc 引用。2) 实现 ProxyHttp trait：new_ctx() 创建 KongRequestCtx，early_request_filter() 执行路由匹配和 rewrite 阶段插件，request_filter() 执行 access 阶段插件，upstream_peer() 通过负载均衡选择上游，response_filter() 执行 header_filter 阶段，logging() 执行 log 阶段。3) KongRequestCtx：存储匹配的 Route/Service、插件链、请求/响应修改队列等。 | Restrictions: 必须正确映射 Pingora 和 Kong 的阶段对应关系。ctx 必须线程安全。 | Success: 代理服务能接收请求、路由匹配、转发到上游并返回响应。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 4.2 实现负载均衡器（kong-proxy）
  - 文件：`crates/kong-proxy/src/balancer.rs`
  - 实现 round-robin、least-conn、consistent-hashing、latency 算法
  - 支持基于 consumer、ip、header、cookie、path、query_arg、uri_capture 的一致性哈希
  - 支持 Target 权重
  - _Leverage: /Users/dawxy/proj/kong/kong/runloop/balancer/, Pingora load-balancing_
  - _Requirements: R1_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长负载均衡算法 | Task: 实现 kong-proxy crate 的负载均衡器。参考 /Users/dawxy/proj/kong/kong/runloop/balancer/ 了解 Kong 的负载均衡实现。1) Balancer 结构体：每个 Upstream 对应一个 Balancer 实例。2) 实现负载均衡算法：round-robin（加权轮询）、least-conn（最少连接）、consistent-hashing（一致性哈希，使用 ketama 算法，slots 配置）、latency（延迟感知）。3) 一致性哈希支持多种 hash_on 来源：consumer、ip、header、cookie、path、query_arg、uri_capture，以及 hash_fallback。4) 支持 Target 权重（weight 字段）。5) 排除不健康的 Target。 | Restrictions: 一致性哈希的 slots 数量和算法必须与 Kong 一致。权重分配必须正确。 | Success: 负载均衡器能按配置的算法正确分发流量。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 4.3 实现健康检查器（kong-proxy）
  - 文件：`crates/kong-proxy/src/health_check.rs`
  - 实现主动健康检查（HTTP/TCP/gRPC 探测）和被动健康检查（请求错误计数）
  - 健康/不健康状态转换逻辑与 Kong 一致
  - _Leverage: /Users/dawxy/proj/kong/kong/runloop/balancer/healthcheckers.lua_
  - _Requirements: R7_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长网络编程和健康检查系统 | Task: 实现 kong-proxy crate 的健康检查器。参考 /Users/dawxy/proj/kong/kong/runloop/balancer/healthcheckers.lua。1) 主动健康检查：按 interval 周期探测 Target（支持 HTTP/HTTPS/TCP/gRPC 类型），successes 次成功标记健康，http_failures/tcp_failures 次失败标记不健康。2) 被动健康检查：统计请求的成功/失败，successes 次成功恢复健康，http_failures/tcp_failures/timeouts 次失败标记不健康。3) 健康状态管理：维护每个 Target 的健康状态，通知 Balancer 排除不健康节点。4) 支持通过 Admin API 手动设置健康状态（/healthy, /unhealthy）。 | Restrictions: 健康检查的阈值和状态转换逻辑必须与 Kong 完全一致。主动检查使用独立的 tokio task。 | Success: 健康检查正确检测并标记不健康节点，自动恢复正常节点。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 5：插件系统

- [x] 5.1 实现插件注册表和执行框架（kong-plugin-system）
  - 文件：`crates/kong-plugin-system/src/*.rs`
  - 实现插件注册、配置验证、优先级排序、链式执行
  - 支持全局/Service/Route/Consumer 四级插件配置
  - 插件迭代器按 PRIORITY 值排序执行
  - _Leverage: /Users/dawxy/proj/kong/kong/runloop/plugins_iterator.lua_
  - _Requirements: R5_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长插件架构设计 | Task: 实现 kong-plugin-system crate。参考 /Users/dawxy/proj/kong/kong/runloop/plugins_iterator.lua 了解 Kong 的插件迭代执行逻辑。1) PluginRegistry：注册插件工厂，按名称查找插件。2) PluginSystem：管理插件生命周期。get_plugin_chain() 根据当前请求的 Route/Service/Consumer 筛选匹配的插件实例，按 PRIORITY 降序排序。3) execute_phase()：按顺序执行插件链中每个插件的指定阶段方法。4) 插件配置验证：根据插件 schema 验证 config JSON。5) Phase 枚举：Certificate, Rewrite, Access, Response, HeaderFilter, BodyFilter, Log。6) 插件优先级合并逻辑：Consumer 级 > Route 级 > Service 级 > 全局级（与 Kong 一致）。 | Restrictions: 插件执行顺序和合并逻辑必须与 Kong 完全一致。某些插件（如认证插件）可能短路后续插件执行。 | Success: 插件系统能正确加载、排序和执行插件链。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 5.2 实现 LuaJIT VM 管理（kong-lua-bridge）
  - 文件：`crates/kong-lua-bridge/src/vm.rs`, `crates/kong-lua-bridge/src/lib.rs`
  - 使用 mlua crate 创建 LuaJIT VM 池（per-worker）
  - 实现 VM 的获取、归还和资源清理
  - _Requirements: R5_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，精通 mlua 和 LuaJIT 集成 | Task: 实现 kong-lua-bridge crate 的 LuaJIT VM 管理。1) LuaVmPool：per-worker 的 Lua VM 对象池，避免频繁创建/销毁。2) 使用 mlua 的 Lua::new() 创建 LuaJIT VM（启用 mlua 的 luajit feature）。3) VM 初始化：预加载 Kong 的基础 Lua 模块路径（package.path/package.cpath）。4) acquire()/release() 方法：从池中获取和归还 VM。5) 每次归还时清理请求相关的全局状态，保留预加载的模块。 | Restrictions: Lua VM 不是 Send 的，需要确保 VM 在同一线程使用。使用 thread_local 或 per-worker 分配。 | Success: VM 池能正确管理 Lua 状态，无内存泄漏。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 5.3 实现 PDK 兼容层（kong-lua-bridge）
  - 文件：`crates/kong-lua-bridge/src/pdk/*.rs`
  - 实现所有 PDK 命名空间：kong.request、kong.response、kong.service、kong.service.request、kong.service.response、kong.client、kong.ctx、kong.log、kong.cache、kong.router、kong.node、kong.ip
  - 每个 PDK 方法通过 Rust 回调实现，操作 Pingora Session 或请求上下文
  - _Leverage: /Users/dawxy/proj/kong/kong/pdk/*.lua_
  - _Requirements: R5_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，精通 mlua FFI 和 Kong PDK | Task: 实现 kong-lua-bridge crate 的 PDK 兼容层。逐一参考 /Users/dawxy/proj/kong/kong/pdk/ 目录下的每个 Lua 文件，确保 API 行为一致。参考设计文档中的 PDK 接口映射表。需要实现的命名空间：1) kong.request：get_method, get_scheme, get_host, get_port, get_path, get_raw_query, get_query, get_header, get_headers, get_raw_body, get_body, get_uri_captures。2) kong.response：exit, set_header, add_header, clear_header, set_headers, get_status, get_header, get_headers, get_source。3) kong.service：set_upstream, set_target。4) kong.service.request：set_scheme, set_path, set_raw_query, set_query, set_header, add_header, clear_header, set_headers, set_body, set_raw_body。5) kong.service.response：get_status, get_header, get_headers, get_raw_body, get_body。6) kong.client：get_ip, get_forwarded_ip, get_port, get_forwarded_port, get_credential, authenticate, get_consumer, load_consumer。7) kong.ctx：shared（请求级共享数据表）。8) kong.log：debug, info, notice, warn, err, crit, alert, emerg, serialize。9) kong.cache：get, invalidate, purge。10) kong.router：get_route, get_service。11) kong.node：get_id, get_hostname, get_memory_stats。12) kong.ip：is_trusted。每个方法通过 mlua 的 UserData 或 Function 注册到 Lua 全局 kong 表中。 | Restrictions: API 签名和行为必须与 Kong PDK 完全一致。kong.response.exit() 需要能中断插件链执行（通过 Lua error 或特殊返回值）。 | Success: Lua 插件调用 PDK 方法能得到与 Kong 一致的结果。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 5.4 实现 Lua 插件加载器和 ngx.* 兼容层（kong-lua-bridge）
  - 文件：`crates/kong-lua-bridge/src/loader.rs`, `crates/kong-lua-bridge/src/schema.rs`, `crates/kong-lua-bridge/src/ngx_compat.rs`
  - 加载 Lua 插件的 handler.lua 和 schema.lua
  - 解析插件 schema 定义，提取 PRIORITY、VERSION
  - 实现 LuaPluginHandler（实现 PluginHandler trait）
  - 提供常用 ngx.* API 兼容实现
  - _Leverage: /Users/dawxy/proj/kong/kong/plugins/*/handler.lua, /Users/dawxy/proj/kong/kong/plugins/*/schema.lua_
  - _Requirements: R5_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，精通 Lua 模块系统和 Kong 插件机制 | Task: 实现 Lua 插件加载器和 ngx 兼容层。1) loader.rs：从插件目录加载 handler.lua 和 schema.lua，解析 PRIORITY 和 VERSION 字段，创建 LuaPluginHandler 实例。2) schema.rs：解析 Lua schema 定义，提取插件配置的字段定义、类型约束、默认值，用于 Admin API 的配置验证。3) LuaPluginHandler 实现 PluginHandler trait：每个阶段方法从 VM 池获取 Lua VM，注入 PDK context，调用对应的 handler 方法（如 handler:access(config)），然后归还 VM。4) ngx_compat.rs：实现常用 ngx.* API —— ngx.say, ngx.print, ngx.exit, ngx.status, ngx.var, ngx.req（get_headers, get_body_data, get_uri_args 等）, ngx.resp（get_headers）, ngx.log, ngx.ERR/WARN/INFO 等常量, ngx.HTTP_OK/HTTP_NOT_FOUND 等状态码常量, ngx.now, ngx.time, ngx.re.match/find/gmatch, ngx.encode_base64/decode_base64, ngx.md5, ngx.sha1_bin。 | Restrictions: 加载器必须能加载 Kong 的所有 47 个内置插件。ngx 兼容层不需要 100% 覆盖，但必须覆盖内置插件使用到的 API。 | Success: 能成功加载并执行 Kong 的内置 Lua 插件（如 key-auth, rate-limiting）。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 6：Admin API

- [x] 6.1 实现 Admin API 基础框架（kong-admin）
  - 文件：`crates/kong-admin/src/app.rs`, `crates/kong-admin/src/pagination.rs`, `crates/kong-admin/src/error.rs`, `crates/kong-admin/src/validation.rs`
  - 使用 axum 搭建 Admin API HTTP 服务
  - 实现与 Kong 兼容的分页响应格式、错误响应格式
  - 实现泛型 CRUD handler 宏/函数
  - _Requirements: R3_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust Web 工程师，精通 axum 框架 | Task: 实现 kong-admin crate 的基础框架。1) app.rs：使用 axum 创建 Admin API 应用，配置路由、中间件（JSON content-type、CORS、请求日志）。AppState 包含 Database 和 PluginSystem 的 Arc 引用。2) pagination.rs：实现 `PageResponse&lt;T&gt;` 结构体（data、next、offset 字段），与 Kong 的分页响应格式完全一致。3) error.rs：实现 KongApiError 类型，序列化为 Kong 格式 `{ message, name, code, fields }`。实现 IntoResponse trait。4) validation.rs：实现请求体验证，字段缺失/类型错误/唯一约束冲突等场景的错误消息与 Kong 一致。5) 泛型 CRUD：创建 `register_entity_routes&lt;T&gt;()` 函数或宏，自动为实体注册 GET(list)/POST(create)/GET(read)/PUT(upsert)/PATCH(update)/DELETE(delete) 路由。 | Restrictions: 错误响应格式必须与 Kong 完全一致（同样的字段名和 HTTP 状态码）。 | Success: Admin API 框架能启动并响应请求，错误格式与 Kong 一致。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 6.2 实现核心实体 CRUD 端点（kong-admin）
  - 文件：`crates/kong-admin/src/handlers/*.rs`
  - 实现 Services、Routes、Consumers、Upstreams、Targets、Plugins、Certificates、SNIs、CaCertificates、KeySets、Keys、Vaults 的 CRUD 端点
  - 实现嵌套资源端点（/services/{id}/routes 等）
  - _Requirements: R3_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust Web 工程师 | Task: 实现所有核心实体的 CRUD API 端点。参考 /Users/dawxy/proj/kong/kong/api/routes/ 和 /Users/dawxy/proj/kong/kong/api/endpoints.lua 了解 Kong 的 API 定义。为每个实体创建 handler 文件：1) services.rs：/services, /services/{id_or_name}。2) routes.rs：/routes, /routes/{id_or_name}。3) consumers.rs：/consumers, /consumers/{id_or_name}。4) upstreams.rs：/upstreams, /upstreams/{id_or_name}。5) targets.rs：/upstreams/{upstream}/targets, /upstreams/{upstream}/targets/{target}。6) plugins.rs：/plugins, /plugins/{id}。7) certificates.rs：/certificates, /certificates/{id}。8) snis.rs：通过 certificates 嵌套管理。9) 其他实体类似。10) 嵌套资源：/services/{id}/routes, /services/{id}/plugins, /routes/{id}/plugins, /consumers/{id}/plugins。所有端点支持按 id 或 name（如果有 name 字段）查找。PUT 方法实现 upsert 语义。 | Restrictions: 响应 JSON 格式必须与 Kong 完全一致。支持按 id 和 name 查找。 | Success: 所有 CRUD 端点正常工作，响应格式与 Kong 一致。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 6.3 实现特殊 Admin API 端点（kong-admin）
  - 文件：`crates/kong-admin/src/handlers/info.rs`, `schemas.rs`, `tags.rs`, `cache.rs`, `debug.rs`
  - 实现 /（根信息）、/status、/config、/endpoints、/plugins/enabled、/schemas/*、/tags/{tag}、/cache、/debug/node/log-level、/clustering/status、/timers
  - _Leverage: /Users/dawxy/proj/kong/kong/api/routes/*.lua_
  - _Requirements: R3_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust Web 工程师 | Task: 实现所有特殊 Admin API 端点。参考 /Users/dawxy/proj/kong/kong/api/routes/ 了解各端点的响应格式。1) info.rs：GET / 返回节点信息（version、hostname、plugins.available_on_server 等），GET /status 返回 `{ database: { reachable: true }, memory: {...}, server: {...} }`。2) schemas.rs：GET /schemas/{entity} 返回实体 schema，GET /schemas/plugins/{name} 返回插件配置 schema，POST /schemas/plugins/validate 验证插件配置。3) tags.rs：GET /tags/{tag} 返回带指定标签的所有实体。4) cache.rs：GET /cache/{key} 获取缓存，DELETE /cache/{key} 删除缓存，DELETE /cache 清空所有缓存。5) debug.rs：GET/PUT /debug/node/log-level 获取/设置日志级别。6) GET /endpoints 列出所有可用端点。7) GET/POST /config 声明式配置导入导出。8) GET /plugins/enabled 列出启用的插件。9) GET /timers 计时器统计。10) GET /clustering/status 集群状态。 | Restrictions: 每个端点的响应格式必须与 Kong 完全一致。 | Success: 所有特殊端点正常工作。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 7：TLS 和证书管理

- [x] 7.1 实现 TLS 证书管理和 SNI 匹配
  - 文件：`crates/kong-proxy/src/tls.rs`
  - 在 Pingora 的 TLS 回调中实现基于 SNI 的证书选择
  - 支持 Service 级别的客户端证书和上游 TLS 验证
  - _Leverage: /Users/dawxy/proj/kong/kong/runloop/certificate.lua_
  - _Requirements: R8_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 TLS 和网络安全 | Task: 实现 TLS 证书管理。参考 /Users/dawxy/proj/kong/kong/runloop/certificate.lua。1) CertificateManager：从数据库加载 Certificate 和 SNI 映射，在 TLS 握手时根据 SNI 选择正确的证书。2) 集成 Pingora 的 TLS 配置：通过 ssl_certificate 回调实现动态证书选择。3) 上游 TLS：支持 Service 的 client_certificate（mTLS）、tls_verify、tls_verify_depth、ca_certificates 配置。4) 证书热更新：监听数据库变更，更新证书缓存。 | Restrictions: 证书选择逻辑必须与 Kong 一致（精确匹配优先于通配符匹配）。 | Success: TLS 握手能根据 SNI 选择正确的证书，上游 mTLS 正常工作。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 8：集成和启动

- [x] 8.1 实现主入口和启动流程（kong-server）
  - 文件：`crates/kong-server/src/main.rs`
  - 整合所有组件：配置加载 → 数据库连接 → 路由表构建 → 插件加载 → 启动 Proxy Listener + Admin API Listener
  - 支持命令行参数（-c 指定配置文件，-p 指定工作目录）
  - 支持优雅关闭
  - _Requirements: R1, R6_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师 | Task: 实现 kong-server crate 的主入口。1) main.rs：解析命令行参数（-c config_file, -p prefix）。2) 启动流程：a) 加载 KongConfig（配置文件 + 环境变量）。b) 初始化数据库连接（PostgreSQL 或 db-less）。c) 构建路由表。d) 加载所有 Lua 插件（从 plugins 配置和插件目录）。e) 创建 KongProxy 实例并注册到 Pingora Server。f) 启动 Admin API（axum）在 admin_listen 地址。g) 启动 Pingora Proxy 在 proxy_listen 地址。3) 优雅关闭：监听 SIGTERM/SIGINT，等待在途请求完成后退出。4) 后台任务：路由表定时刷新、健康检查、缓存清理。 | Restrictions: 启动流程中任何步骤失败应输出清晰的错误信息并退出。 | Success: kong-server 能从 kong.conf 启动，同时监听 proxy 和 admin 端口。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 8.2 集成测试 — Admin API 兼容性验证
  - 文件：`tests/admin_api_compat.rs`
  - 针对所有 Admin API 端点编写集成测试
  - 验证请求/响应格式与 Kong 完全一致
  - _Requirements: R3_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 测试工程师 | Task: 编写 Admin API 集成测试。1) 启动测试 Kong-Rust 实例（使用测试数据库或 db-less 模式）。2) 对所有 CRUD 端点编写测试：创建、读取、更新（PUT/PATCH）、删除、列表、分页。3) 验证响应 JSON 格式：字段名、类型、默认值与 Kong 一致。4) 验证错误响应：缺少必填字段、唯一约束冲突、不存在的资源等。5) 验证嵌套资源端点。6) 验证特殊端点（/, /status, /schemas/*, /tags/*）。 | Restrictions: 测试应能独立运行，使用测试数据库或 db-less 模式。 | Success: 所有 Admin API 测试通过，响应格式与 Kong 一致。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 8.3 集成测试 — Lua 插件兼容性验证
  - 文件：`tests/lua_plugin_compat.rs`
  - 加载 Kong 的内置 Lua 插件，验证基本功能
  - 重点测试 key-auth、rate-limiting、cors、request-transformer 等常用插件
  - _Requirements: R5_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 测试工程师 | Task: 编写 Lua 插件兼容性集成测试。1) 配置 Kong-Rust 加载 Kong 的内置 Lua 插件目录（/Users/dawxy/proj/kong/kong/plugins/）。2) 测试 key-auth 插件：配置 key-auth → 创建 Consumer 和 key → 验证无 key 请求返回 401 → 验证有 key 请求通过。3) 测试 rate-limiting 插件：配置限流规则 → 发送请求 → 验证限流生效（429）。4) 测试 cors 插件：配置 CORS → 发送 OPTIONS 请求 → 验证 CORS 响应头。5) 测试 request-transformer 插件：配置头部添加 → 验证上游收到修改后的请求。6) 验证插件优先级执行顺序正确。 | Restrictions: 使用 Kong 原始的 Lua 插件代码，不做任何修改。 | Success: 常用 Lua 插件能在 Kong-Rust 中正确运行。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 8.4 端到端测试 — 代理功能验证
  - 文件：`tests/proxy_e2e.rs`
  - 测试完整的代理流程：请求 → 路由匹配 → 插件执行 → 负载均衡 → 上游转发 → 响应
  - 测试各种路由匹配场景、重试、超时、健康检查
  - _Requirements: R1, R2, R7_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 测试工程师 | Task: 编写端到端代理测试。1) 启动测试上游 HTTP 服务。2) 通过 Admin API 配置 Service + Route。3) 发送代理请求验证转发正确。4) 测试路由匹配：host 匹配、path 匹配、method 匹配、header 匹配、通配符匹配。5) 测试 strip_path 和 preserve_host 行为。6) 测试负载均衡：多 Target 轮询。7) 测试重试：上游失败后自动重试其他 Target。8) 测试超时：connect_timeout、read_timeout。9) 测试健康检查：标记 Target 不健康后不再转发。 | Restrictions: 测试应模拟真实场景，使用真实的 HTTP 请求。 | Success: 所有代理功能正确工作。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [x] 8.5 日志输出到文件，通过 kong.conf 配置
  - 文件：`Cargo.toml`, `crates/kong-server/Cargo.toml`, `crates/kong-server/src/main.rs`, `kong.conf.default`
  - 重构 tracing 初始化，支持从 kong.conf 读取 `log_level` 和 `proxy_error_log` 配置
  - 支持文件 + stderr 双写，`proxy_error_log = off` 时仅 stderr
  - 使用 tracing-appender 的 rolling::never（不轮转，与 Kong/Nginx 行为一致）
  - 自动创建日志目录，RUST_LOG 环境变量优先于配置文件
  - _Requirements: R6_

- [x] 8.6 实现上游 TLS 完整功能（CA 验证 + mTLS 客户端证书）
  - 文件：`crates/kong-proxy/src/lib.rs`, `crates/kong-server/src/main.rs`
  - KongProxy 添加 cert_manager 和 ca_certificates 字段
  - upstream_peer() 实现 CA 证书信任链构建（tls_verify + ca_certificates）
  - upstream_peer() 实现 mTLS 客户端证书（client_certificate）
  - kong-server DB 模式加载 certificates/snis/ca_certificates 传给 KongProxy
  - _Requirements: R8_

- [x] 8.7 Admin API 写操作后动态刷新 KongProxy 内存缓存
  - 文件：`crates/kong-proxy/src/lib.rs`, `crates/kong-admin/Cargo.toml`, `crates/kong-admin/src/lib.rs`, `crates/kong-admin/src/handlers/mod.rs`, `crates/kong-server/src/main.rs`
  - KongProxy 实现 Clone（所有字段 Arc 包装，clone 后共享底层数据）
  - AdminState 添加 proxy 字段，持有 KongProxy 的 clone
  - 实现 refresh_proxy_cache 方法，根据实体类型从 DAO 全量读取并刷新代理缓存
  - 修改 entity_handlers! 宏，CUD 操作成功后 tokio::spawn 异步刷新
  - 嵌套 target 端点同样触发缓存刷新
  - _Requirements: R1, R3_

- [x] 8.8 缓存刷新防抖（Debounce）— 合并批量 CUD 操作的刷新请求
  - 文件：`crates/kong-admin/src/lib.rs`, `crates/kong-admin/src/handlers/mod.rs`, `crates/kong-server/src/main.rs`
  - 使用 mpsc unbounded channel + debounce 循环替代 tokio::spawn 直接刷新
  - handler 发送实体类型信号（纳秒级），后台任务收到第一个信号后等 100ms，HashSet 去重后一次性执行
  - AdminBgService 在 Pingora runtime 的 start() 中 spawn refresher，避免临时 runtime 生命周期问题
  - _Requirements: R1, R3_

## 阶段 8b：处理链路重构 + 功能补全 + 测试体系

- [x] 8.9 PhaseRunner 抽象层 + body_filter 接入
  - 新建 `crates/kong-proxy/src/phases.rs` — PhaseRunner 封装所有 Kong 阶段
  - 修改 `crates/kong-core/src/traits/plugin.rs` — body_filter 增加 end_of_stream 参数、RequestCtx 增加请求/响应快照字段
  - 修改 `crates/kong-plugin-system/src/lib.rs` — 新增 execute_body_filter 方法
  - 重构 `crates/kong-proxy/src/lib.rs` — 使用 PhaseRunner、新增 response_body_filter()、修复短路响应（支持自定义 headers/body）、填充 RequestCtx 请求快照

- [x] 8.10 修复 PDK 硬编码桩
  - 重写 `crates/kong-lua-bridge/src/pdk/mod.rs` — 从 __kong_req_data 全局表读取真实请求数据
  - kong.response.exit() 通过 __kong_short_circuited 等全局变量实现短路
  - kong.service.request.set_header/clear_header 通过 __kong_upstream_headers_set/remove 传递
  - sync_ctx_from_lua() 同步短路标志、上游请求头修改、响应头修改回 RequestCtx
  - ngx.req/ngx.var 从 __kong_req_data 读取真实数据

- [x] 8.11 补全负载均衡算法 + 健康检查集成
  - 重写 `crates/kong-proxy/src/balancer.rs` — 实现 consistent-hashing（ketama 风格哈希环）、least-connections（加权最少连接）
  - LoadBalancer 集成 HealthChecker — select() 跳过不健康目标
  - 实现 increment_connections/decrement_connections 连接计数
  - 实现 extract_hash_key 从请求上下文提取哈希 key
  - 修复 `crates/kong-proxy/src/health.rs` — do_http_check 发送真正的 HTTP GET 请求

- [x] 8.12 测试体系建设
  - 新建 `crates/kong-proxy/tests/helpers/mod.rs` — TestPlugin、make_resolved_plugin 测试基础设施
  - 新建 `crates/kong-proxy/tests/phase_chain.rs` — 10 个阶段链测试（短路、优先级、ctx.shared 传递等）
  - 扩展 `crates/kong-lua-bridge/tests/lua_plugin_compat.rs` — 新增 20+ PDK 真实数据测试（验证不再硬编码）
  - 修复 `crates/kong-admin/tests/admin_api_compat.rs` — 补全缺失的 proxy 和 refresh_tx 字段

## 阶段 8c：L4 Stream 代理

- [x] 8.13 实现 Stream 路由引擎（kong-router/src/stream.rs）
  - 新建 `crates/kong-router/src/stream.rs` — StreamRouter、StreamRequestContext、StreamRouteMatch
  - 支持 source/dest IP:Port 匹配（CIDR 范围）、SNI 匹配（含通配符 *.example.com）
  - 优先级：匹配维度越多越优先，同级按 created_at 排序
  - 仅索引 protocols 包含 tcp/tls/tls_passthrough 的路由
  - 8 个单元测试覆盖各种匹配场景
  - _Requirements: R1_

- [x] 8.14 实现 TLS ClientHello SNI 解析器（kong-proxy/src/stream_tls.rs）
  - 新建 `crates/kong-proxy/src/stream_tls.rs` — 手动解析 TLS record 提取 SNI
  - 解析路径：TLS Record Header → Handshake Header → ClientHello → Extensions → SNI Extension
  - CLIENT_HELLO_PEEK_SIZE = 1024 字节推荐 peek 缓冲区
  - 4 个单元测试（含构造 ClientHello 辅助函数）
  - _Requirements: R1_

- [x] 8.15 实现 Stream 代理核心（kong-proxy/src/stream.rs）
  - 新建 `crates/kong-proxy/src/stream.rs` — KongStreamProxy 实现 Pingora ServerApp trait
  - 支持 TCP 明文转发、TLS Passthrough（peek SNI 不终止 TLS）、TLS Termination（暂作 TCP 透传）
  - 共享 HTTP 代理的 balancers/services/cert_manager（Arc<RwLock<...>>）
  - peek 首字节判断 TLS → 解析 SNI → 路由匹配 → 解析上游 → 双向转发
  - Stream access log 输出
  - _Requirements: R1_

- [x] 8.16 注册 Stream Service + 路由热更新
  - 修改 `crates/kong-server/src/main.rs` — 创建 Stream Proxy Service，绑定 stream_listen 端口
  - 用实际 DB 路由数据初始化 StreamRouter（非空 Vec）
  - AdminState 添加 stream_router 字段，路由 CUD 操作同步更新 Stream 路由表
  - 修改 `crates/kong-admin/src/lib.rs` + `handlers/mod.rs` — 路由刷新时同步 rebuild StreamRouter
  - _Requirements: R1_

## 阶段 8d：性能优化 — 异步 DNS + 异步日志

- [x] 8.17 异步 DNS 解析（hickory-resolver）
  - 新建 `crates/kong-proxy/src/dns.rs` — DnsResolver 封装 hickory-resolver，支持 IP 直连跳过、TTL 缓存、自定义 nameserver
  - 修改 `crates/kong-proxy/src/lib.rs` — KongProxy 新增 dns_resolver 字段，upstream_peer() 使用异步 DNS
  - 修改 `crates/kong-proxy/src/stream.rs` — KongStreamProxy 新增 dns_resolver 字段，connect_upstream() 使用异步 DNS
  - 修改 `crates/kong-server/src/main.rs` — 创建共享 DnsResolver 注入两个代理
  - 消除了 `std::net::ToSocketAddrs` 同步阻塞 tokio 工作线程的问题
  - _Requirements: R1_

- [x] 8.18 异步 Access Log 写入（mpsc channel + 后台 flush）
  - 新建 `crates/kong-proxy/src/access_log.rs` — AccessLogWriter 通过 bounded channel 异步写入
  - 热路径仅 try_send（无锁纳秒级），后台任务批量 recv + flush
  - channel 满时丢弃日志（不反压请求处理）
  - 删除 KongProxy::init_access_log() 和 KongStreamProxy::init_access_log()
  - 修改 logging()/handle_connection() 使用异步写入
  - _Requirements: R1_

## 阶段 8e：Route 级别 Body Buffering

- [x] 8.19 实现 Route 级别 request_buffering / response_buffering
  - 修改 `crates/kong-router/src/lib.rs` — RouteMatch 添加 request_buffering / response_buffering 字段
  - 修改 `crates/kong-router/src/traditional.rs` — ProcessedRoute 添加字段，process_route() 和 find_route() 传递字段值
  - 修改 `crates/kong-router/src/expressions.rs` — ExpressionRoute 添加字段，构造和匹配时传递字段值
  - 修改 `crates/kong-proxy/src/lib.rs` — KongCtx 添加 request_body_buf / response_body_buf 缓冲区；实现 request_body_filter()；修改 response_body_filter() 在插件阶段前执行 response buffering
  - Kong 默认 buffering=true（全量缓冲后转发），false=流式转发（Pingora 原生行为）
  - _Requirements: R1_

## 阶段 9：Hybrid 模式和集群通信

- [ ] 9.1 实现 kong-cluster crate 基础结构和 CP/DP 角色启动
  - 创建 kong-cluster crate，定义 ClusterRole 枚举（Traditional/ControlPlane/DataPlane）
  - 修改 kong-config 添加集群相关配置项（role、cluster_listen、cluster_control_plane、cluster_cert/key 等）
  - 修改 kong-server/main.rs 根据 role 配置分支启动流程
  - 文件：`crates/kong-cluster/Cargo.toml`, `crates/kong-cluster/src/lib.rs`, `crates/kong-cluster/src/role.rs`, `crates/kong-config/src/lib.rs`, `crates/kong-server/src/main.rs`
  - _Requirements: R9_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师 | Task: 创建 kong-cluster crate 并实现角色启动差异。1) 创建 kong-cluster crate 基础结构（参考设计文档 .spec-workflow/specs/kong-rust/design.md 组件 9 的 Workspace 结构）。2) 定义 ClusterRole 枚举：Traditional、ControlPlane、DataPlane。3) 在 kong-config 中添加集群配置项：role（默认 traditional）、cluster_listen（默认 0.0.0.0:8005）、cluster_control_plane、cluster_cert、cluster_cert_key、cluster_data_plane_purge_delay（默认 1209600）、cluster_max_payload（默认 16777216）。4) 修改 kong-server/main.rs 根据 role 分支启动：Traditional → Admin + Proxy + DB；ControlPlane → Admin + DB + WebSocket 服务端；DataPlane → WebSocket 客户端 + Proxy（不启动 Admin API，不连接 DB）。 | Restrictions: 确保 Traditional 模式行为不受影响（向后兼容）。CP 和 DP 模式的启动流程参考设计文档中的角色启动流程变更。 | Success: cargo build 通过，Traditional 模式正常工作，CP/DP 模式能正确识别角色并启动对应组件。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [ ] 9.2 实现 Control Plane WebSocket 服务端和配置推送（Sync V1）
  - 在 CP 上启动 WebSocket 服务端（cluster_listen），处理 DP 连接
  - 实现配置导出（从 DB 读取所有实体 → JSON → GZIP 压缩）和全量推送
  - 实现 DP 客户端管理（注册、状态追踪、断连清理）
  - 实现多级配置哈希计算（routes/services/plugins/upstreams/targets/rest 分别 MD5，拼接后再 MD5）
  - 文件：`crates/kong-cluster/src/cp/ws_server.rs`, `crates/kong-cluster/src/cp/config_push.rs`, `crates/kong-cluster/src/cp/client_manager.rs`, `crates/kong-cluster/src/cp/hash.rs`
  - _Leverage: /Users/dawxy/proj/kong/kong/clustering/control_plane.lua, /Users/dawxy/proj/kong/kong/clustering/config_helper.lua_
  - _Requirements: R9_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 WebSocket 和网络编程 | Task: 实现 CP WebSocket 服务端和 Sync V1 配置推送。参考 /Users/dawxy/proj/kong/kong/clustering/control_plane.lua 和 config_helper.lua。1) ws_server.rs：使用 tokio-tungstenite 在 cluster_listen 地址启动 WebSocket 服务端，支持 /v1/outlet 和 /v2/outlet 端点，mTLS 认证。2) config_push.rs：从 Database 导出所有实体配置为 JSON，GZIP 压缩后通过 WebSocket Binary 帧发送。DP 连接时立即推送全量配置。配置变更时广播给所有 DP。3) client_manager.rs：维护 HashMap<String, DpClientInfo> 追踪所有 DP（node_id、hostname、connected_at、last_seen、config_hash、sync_status）。4) hash.rs：多级哈希计算 — 按 Kong 的 to_sorted_string 规则序列化（对象键排序、数组 ; 分隔、null → /null/、缓冲超 1MB 截断为 MD5），分别计算 routes/services/plugins/upstreams/targets/rest 子哈希，拼接后 MD5 得总哈希。5) 处理 PING 帧：解析 32 字符 MD5 哈希，与当前配置哈希对比，不匹配则重新推送。 | Restrictions: 哈希计算的确定性排序算法必须与 Kong 完全一致。 | Success: CP 能接收 DP 连接，推送配置，正确计算和对比哈希。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [ ] 9.3 实现 Data Plane WebSocket 客户端和配置接收/应用
  - DP 通过 WebSocket 连接 CP（wss://<cluster_control_plane>/v1/outlet）
  - 接收全量配置（解压 GZIP → 解析 JSON → 应用到路由表和插件系统）
  - 实现三线程模型：config_thread（配置应用）、read_thread（帧读取）、write_thread（帧写入）
  - 缓存配置到本地文件，断连后使用缓存继续服务
  - 文件：`crates/kong-cluster/src/dp/ws_client.rs`, `crates/kong-cluster/src/dp/config_apply.rs`
  - _Leverage: /Users/dawxy/proj/kong/kong/clustering/data_plane.lua_
  - _Requirements: R9_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 WebSocket 和异步编程 | Task: 实现 DP WebSocket 客户端和配置接收。参考 /Users/dawxy/proj/kong/kong/clustering/data_plane.lua。1) ws_client.rs：使用 tokio-tungstenite 连接 CP（wss://<cluster_control_plane>/v1/outlet），mTLS 认证。实现三线程模型 — 使用 tokio::select! 或 spawn 并行运行 read_thread（接收帧）、write_thread（发送 PING）和 config_thread（处理配置）。2) config_apply.rs：实现 ConfigApplier trait — apply_full_config 接收 DeclarativeConfig 后重建路由表、重新加载插件配置、更新当前配置哈希。3) 配置缓存：将最新配置写入本地文件（JSON），启动时若无法连接 CP 则从缓存加载。4) 连接状态追踪：维护 control_plane_connected 标志，断连时设为 false。 | Restrictions: 配置应用必须是原子的（全部成功或全部回滚），避免部分应用导致不一致。 | Success: DP 能连接 CP、接收配置、正确应用到代理引擎。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [ ] 9.4 实现增量同步（Sync V2，JSON-RPC 2.0）
  - 实现 JSON-RPC 2.0 协议编解码（请求/响应/通知）
  - 实现 RPC 方法：kong.meta.v1.hello（握手）、kong.sync.v2.get_delta（请求 delta）、kong.sync.v2.notify_new_version（通知新版本）、kong.sync.v2.notify_validation_error（报告错误）
  - 实现 delta 计算（CP 端）和 delta 应用（DP 端）
  - 支持 x-snappy-framed 帧编码
  - WebSocket 子协议：kong.meta.v1
  - 文件：`crates/kong-cluster/src/sync_v2/rpc.rs`, `crates/kong-cluster/src/sync_v2/delta.rs`, `crates/kong-cluster/src/sync_v2/version.rs`
  - _Leverage: /Users/dawxy/proj/kong/kong/clustering/services/sync/rpc.lua, /Users/dawxy/proj/kong/kong/clustering/rpc/manager.lua_
  - _Requirements: R9_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 RPC 协议实现 | Task: 实现 Sync V2 增量同步。参考 /Users/dawxy/proj/kong/kong/clustering/services/sync/rpc.lua 和 /Users/dawxy/proj/kong/kong/clustering/rpc/manager.lua。1) rpc.rs：实现 JSON-RPC 2.0 编解码 — JsonRpcRequest { jsonrpc: '2.0', method, params, id }，JsonRpcResponse { jsonrpc: '2.0', result/error, id }。注册 RPC 方法处理器。支持 x-snappy-framed 帧编码（使用 snap crate）。2) 握手：DP 发送 kong.meta.v1.hello { rpc_capabilities, rpc_frame_encodings: ['x-snappy-framed'], kong_version, kong_node_id, kong_hostname, kong_conf }，CP 响应 { rpc_capabilities, rpc_frame_encoding: 'x-snappy-framed' }。WebSocket Sec-WebSocket-Protocol: kong.meta.v1。3) delta.rs：CP 端 — 根据 DP 提供的版本号计算 delta（新增/修改/删除的实体）。DP 端 — 接收 delta 列表并逐条应用。4) version.rs：版本号管理，版本比较。5) 同步流程：CP 通知 notify_new_version → DP 调用 get_delta → CP 返回 delta → DP 应用。单次同步最多重试 5 次，间隔 0.1 秒。 | Restrictions: JSON-RPC 2.0 格式必须严格遵守规范。RPC 方法名必须与 Kong 完全一致。 | Success: CP 和 DP 能通过 Sync V2 协议完成增量配置同步。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [ ] 9.5 实现 TLS 双向认证和心跳/重连机制
  - 实现 cluster_cert/cluster_cert_key 的 TLS 配置加载
  - CP 端：服务端 TLS + 客户端证书验证
  - DP 端：客户端 TLS + 服务端证书验证
  - 实现 30 秒 PING 心跳（负载为 32 字符 MD5 哈希）
  - 实现 45 秒超时检测（PING_INTERVAL × 1.5）
  - 实现断线重连（5-10 秒随机延迟）
  - 文件：`crates/kong-cluster/src/tls.rs`, `crates/kong-cluster/src/dp/heartbeat.rs`, `crates/kong-cluster/src/dp/reconnect.rs`
  - _Requirements: R9_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 工程师，擅长 TLS 和网络安全 | Task: 实现集群 TLS 双向认证和心跳重连。1) tls.rs：加载 cluster_cert 和 cluster_cert_key PEM 文件，构建 rustls ServerConfig（CP 端，要求客户端证书）和 ClientConfig（DP 端，验证服务端证书）。支持可选的 CA 证书配置。2) heartbeat.rs：DP 端每 30 秒（CLUSTERING_PING_INTERVAL）发送 WebSocket PING 帧，负载为当前配置的 32 字符十六进制 MD5 哈希。CP 端接收 PING 后回复 PONG。CP 端验证 PING 负载必须是 32 字符（否则报错）。超时检测：若 45 秒（PING_INTERVAL × 1.5）内未收到对端消息，标记连接断开。3) reconnect.rs：DP 断连后使用 5-10 秒随机延迟（避免雷鸣羊群效应）后重连。连接失败也触发重连。重连成功后重新执行握手和配置同步流程。维护 control_plane_connected 状态标志。 | Restrictions: TLS 配置必须是 mTLS（双向认证），不能只做单向。心跳参数必须与 Kong 一致。 | Success: CP/DP 间 mTLS 正常工作，心跳保活生效，断线能自动重连。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [ ] 9.6 实现 /clustering/status Admin API 端点
  - 在 kong-admin 中实现 GET /clustering/status 端点
  - 返回所有已连接 DP 的状态（node_id、ip、hostname、version、sync_status、config_hash、last_seen）
  - 仅在 role=control_plane 时可用
  - 文件：`crates/kong-admin/src/handlers/clustering.rs`
  - _Requirements: R9, R3_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust Web 工程师 | Task: 实现 /clustering/status Admin API 端点。参考 /Users/dawxy/proj/kong/kong/api/routes/clustering.lua 了解响应格式。1) 在 kong-admin 中添加 GET /clustering/status 路由。2) 从 ControlPlaneServer 的 client_manager 读取所有已连接 DP 信息。3) 响应格式与 Kong 一致：{ data: [{ id: node_id, ip: addr, hostname, version, sync_status, config_hash, last_seen }] }。4) 仅在 role=control_plane 时返回数据；role=traditional 或 data_plane 时返回空列表或适当提示。 | Restrictions: 响应格式必须与 Kong 完全一致。 | Success: /clustering/status 能正确返回集群状态信息。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

- [ ] 9.7 集成测试 — CP/DP 通信和配置同步验证
  - 测试 CP/DP 角色启动和基本通信
  - 测试 Sync V1 全量配置推送和应用
  - 测试 Sync V2 增量同步
  - 测试心跳超时和断线重连
  - 测试配置变更后自动推送到 DP
  - 测试 DP 使用缓存配置在断连后继续服务
  - 文件：`tests/cluster_e2e.rs`
  - _Requirements: R9_
  - _Prompt: "Implement the task for spec kong-rust, first run spec-workflow-guide to get the workflow guide then implement the task: Role: Rust 测试工程师 | Task: 编写 CP/DP 集群通信集成测试。1) 测试角色启动：启动 CP 实例（role=control_plane）和 DP 实例（role=data_plane），验证 DP 能连接 CP。2) 测试 Sync V1 全量推送：CP 通过 Admin API 创建 Service + Route → 验证 DP 收到配置 → DP 代理请求能路由到正确上游。3) 测试配置变更推送：CP 更新 Service → 验证 DP 自动收到新配置。4) 测试 Sync V2 增量同步：CP 添加新 Route → 验证 DP 只收到 delta 而非全量。5) 测试心跳：验证 30 秒心跳正常发送，PING 负载包含配置哈希。6) 测试断线重连：断开 CP/DP 连接 → 验证 DP 使用缓存配置继续代理 → 恢复连接后 DP 自动重连并重新同步。7) 测试 /clustering/status：验证 CP 的 /clustering/status 端点返回正确的 DP 信息。 | Restrictions: 测试应使用真实的 WebSocket 通信（非 mock），可使用自签名证书进行 mTLS。 | Success: 所有集群通信场景测试通过。在 tasks.md 中将 [ ] 改为 [-] 标记开始，完成后用 log-implementation 记录，然后改为 [x]。"_

## 阶段 10：Docker 镜像构建

- [x] 10.1 添加 docker-start 和 health CLI 子命令
  - kong-server 新增 `docker-start` 子命令（顺序执行 migrations bootstrap/up/finish → start）
  - 新增 `health` 子命令（HTTP GET Admin API /status，进程退出码表示健康状态）
  - 用于 Docker 容器内一键启动和健康检查
  - 文件：`crates/kong-server/src/main.rs`

- [x] 10.2 创建 Dockerfile（多阶段构建，兼容 Kong 官方镜像）
  - 多阶段构建：builder（Rust 编译）→ runtime（Debian slim 最小镜像）
  - 兼容 Kong 官方镜像的用户/目录布局（kong 用户、/usr/local/kong 等）
  - 暴露标准端口（8000/8443/8001/8444/8005）
  - 文件：`Dockerfile`

- [x] 10.3 创建 docker-entrypoint.sh（Docker Secrets 支持）
  - 支持 Docker Secrets（`KONG_*_FILE` 环境变量自动读取文件内容）
  - 与 Kong 官方 docker-entrypoint.sh 行为兼容
  - 文件：`docker-entrypoint.sh`

- [x] 10.4 创建 .dockerignore
  - 排除 target/、.git/、kong-manager/node_modules/ 等不必要文件
  - 文件：`.dockerignore`

- [x] 10.5 更新 Makefile（docker-build/push/run/stop 目标）
  - 新增 docker-build、docker-push、docker-run、docker-stop Make 目标
  - 支持 DOCKER_TAG 和 DOCKER_REGISTRY 变量
  - 文件：`Makefile`

## 阶段 11：HTTP 代理性能优化

- [x] 11.1 RouteMatch 类型优化（Opt-4）
  - protocols 改为 Arc<Vec<String>>，path_handling 改为 PathHandling 枚举，route_name 改为 Arc<str>
  - 消除每次路由匹配时的堆分配
  - 文件：`crates/kong-router/src/lib.rs`, `crates/kong-router/src/traditional.rs`, `crates/kong-router/src/expressions.rs`

- [x] 11.2 消除重复请求头解析（Opt-1）
  - 合并 build_request_context() 和 populate_request_ctx() 为 populate_and_build_route_ctx()
  - 单次头遍历同时填充 RequestCtx 和 RequestContext，消除约 20 次重复 String 分配
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] 11.3 路由匹配 LRU 缓存（Opt-2）
  - TraditionalRouter 和 ExpressionsRouter 添加 moka::sync::Cache LRU 缓存
  - 缓存键：(method, host_no_port, uri)，容量 1024
  - 路由表 rebuild 时新实例自动创建新缓存（旧缓存随旧实例释放）
  - 文件：`crates/kong-router/Cargo.toml`, `crates/kong-router/src/traditional.rs`, `crates/kong-router/src/expressions.rs`

- [x] 11.4 插件链预计算 + Arc clone 消除（Opt-3 + Opt-5）
  - KongProxy 添加 plugin_chains HashMap，在 update_plugins/update_routes 时预计算
  - KongCtx.resolved_plugins 改为 Arc<Vec<ResolvedPlugin>>，clone 变为原子计数增加
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] 11.5 Service 超时应用到 HttpPeer（Opt-7）
  - upstream_peer() 中设置 connect/read/write timeout
  - 文件：`crates/kong-proxy/src/lib.rs`

- [x] 11.6 大 body 落盘保护（Opt-6）
  - 新建 SpillableBuffer：内存阈值 10MB，超过自动溢出到 tempfile
  - KongCtx 的 request/response_body_buf 改为 Option<SpillableBuffer>
  - 文件：`crates/kong-proxy/src/spillable_buffer.rs`, `crates/kong-proxy/src/lib.rs`

- [x] 11.7 chunk 间隔超时保护（Opt-8）
  - request_body_filter 中检查 body chunk 间隔，超过 60s 返回错误终止请求
  - KongCtx 添加 last_body_chunk_at: Option<Instant>
  - 文件：`crates/kong-proxy/src/lib.rs`

## 阶段 12：协议与 TLS 支持进阶

- [x] 12.1 实现 HTTP/2 支持 (ALPN)
  - 为客户端 (Downstream) 代理开启 HTTP/2 支持：使用 `add_tls_with_settings` 和 `enable_h2()`。
  - 为上游 (Upstream) 代理配置 `ALPN::H2H1`，优先协商 HTTP/2。
  - 检查并兼容配置文件中的 `http2` 标志。
  - 文件：`crates/kong-server/src/main.rs`, `crates/kong-proxy/src/lib.rs`
