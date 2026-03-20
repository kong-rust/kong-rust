# 需求文档：Kong-Rust — 使用 Rust 重写 Kong API 网关

## 简介

Kong-Rust 是一个使用 Rust 语言和 Cloudflare Pingora 框架完全重写 Kong API 网关的项目。最终目标是**完全替换 Kong**——无需修改任何现有 Kong 配置和 Lua 插件，所有数据模型、API 接口和使用习惯与 Kong 保持完全一致，实现零成本迁移。

**核心价值：**
- **无缝替换**：与 Kong 100% 兼容的数据模型、Admin API、配置格式和 Lua 插件接口，可直接替换现有 Kong 部署
- 更高的性能（Rust 原生 + Pingora 的多线程共享连接池）
- 更好的内存安全性（Rust 所有权系统）

## 需求

### R1：核心代理引擎

**用户故事：** 作为 Kong 开发工程师，我希望 Kong-Rust 能完全替换现有的 Kong 部署，所有代理行为（路由转发、负载均衡、重试、超时等）与 Kong 保持完全一致，无需修改任何现有配置。

#### 验收标准

1. 当接收到 HTTP/HTTPS 请求时，系统应根据路由规则匹配对应的 Service 并转发到上游
2. 当配置了多个上游 Target 时，系统应按照负载均衡算法（round-robin、least-conn、consistent-hashing）分发流量
3. 当上游连接失败时，系统应按 retries 配置进行重试，并在所有重试失败后返回 502
4. 当请求超时时，系统应按 connect_timeout/read_timeout/write_timeout 配置执行超时策略
5. 当配置了 strip_path=true 时，系统应在转发前去除匹配的路径前缀
6. 当配置了 preserve_host=true 时，系统应保留原始 Host 头发送到上游
7. 当接收到 gRPC/WebSocket 请求时，系统应正确代理这些协议

### R2：路由引擎

**用户故事：** 作为 Kong 开发工程师，我希望路由引擎的配置方式和匹配效果与 Kong 完全一致，这样迁移到 Kong-Rust 后路由行为不会发生任何变化。

#### 验收标准

1. 当定义路由时指定 hosts 条件，系统应根据请求 Host 头进行匹配（支持通配符如 *.example.com），匹配规则与 Kong 完全一致
2. 当定义路由时指定 paths 条件，系统应根据请求路径进行前缀匹配或正则匹配，匹配优先级与 Kong 完全一致
3. 当定义路由时指定 methods 条件，系统应仅匹配指定的 HTTP 方法
4. 当定义路由时指定 headers 条件，系统应根据请求头进行匹配
5. 当多条路由可能匹配同一请求时，系统应按与 Kong 完全一致的优先级规则选择最具体的路由
6. 当使用 expressions 路由风格时，系统应支持与 Kong 完全一致的表达式路由语法
7. 当路由配置了 SNI 条件时，系统应在 TLS 握手时根据 SNI 匹配路由
8. Route 的所有字段定义（protocols、methods、hosts、paths、headers、snis、sources、destinations、strip_path、preserve_host、regex_priority、path_handling、expression、priority 等）与 Kong 完全一致

### R3：Admin API 完全兼容

**用户故事：** 作为 Kong 开发工程师，我希望 Kong-Rust 的 Admin API 与 Kong 完全兼容，这样现有的自动化脚本和工具（如 decK）无需修改即可使用。

#### 验收标准

**核心实体 CRUD API：**

1. 当调用 `/services` 端点时，系统应支持 GET（列表/分页）、POST（创建）操作
2. 当调用 `/services/{id|name}` 端点时，系统应支持 GET、PUT、PATCH、DELETE 操作
3. 当调用 `/routes`、`/consumers`、`/upstreams`、`/plugins`、`/certificates`、`/ca_certificates`、`/key_sets`、`/keys`、`/vaults` 端点时，系统应提供完整的 CRUD 操作
4. 当调用 `/upstreams/{id}/targets` 端点时，系统应支持 Target 的 CRUD 操作
5. 当调用 `/snis` 端点时，系统应支持 SNI 的关联管理

**嵌套资源 API：**

6. 当调用 `/services/{id}/routes` 时，系统应返回该 Service 下所有 Routes
7. 当调用 `/services/{id}/plugins` 时，系统应管理 Service 级别的插件
8. 当调用 `/routes/{id}/plugins` 时，系统应管理 Route 级别的插件
9. 当调用 `/consumers/{id}/plugins` 时，系统应管理 Consumer 级别的插件

**特殊端点：**

10. 当调用 `/` 时，系统应返回节点信息（版本、已加载插件、配置摘要）
11. 当调用 `/status` 时，系统应返回健康状态和数据库连接信息
12. 当调用 `/config` 时，系统应支持声明式配置的导入导出
13. 当调用 `/plugins/enabled` 时，系统应列出所有启用的插件
14. 当调用 `/schemas/{entity}` 时，系统应返回实体的 JSON Schema
15. 当调用 `/schemas/plugins/{name}` 时，系统应返回插件的配置 Schema
16. 当调用 `/schemas/plugins/validate` 时，系统应验证插件配置
17. 当调用 `/cache` 或 `/cache/{key}` 时，系统应支持缓存管理
18. 当调用 `/endpoints` 时，系统应列出所有可用的 API 端点
19. 当调用 `/debug/node/log-level` 时，系统应支持动态日志级别调整
20. 当调用 `/tags/{tag}` 时，系统应返回带有指定标签的所有实体
21. 当调用 `/upstreams/{id}/health` 时，系统应返回上游健康检查状态
22. 当调用 `/upstreams/{id}/targets/{id}/healthy` 或 `/unhealthy` 时，系统应手动设置 Target 健康状态
23. 当调用 `/clustering/status` 时，系统应返回集群状态信息
24. 当调用 `/timers` 时，系统应返回计时器统计信息

**API 行为兼容性：**

25. 当请求包含分页参数 (size, offset) 时，系统应返回兼容的分页响应格式
26. 当创建实体缺少必填字段时，系统应返回与 Kong 一致的错误响应格式
27. 当使用 PUT 方法更新不存在的实体时，系统应创建该实体（upsert 语义）
28. 当实体包含 tags 字段时，系统应支持基于标签的过滤查询

### R4：数据库兼容

**用户故事：** 作为 Kong 开发工程师，我希望 Kong-Rust 能直接使用现有的 PostgreSQL 数据库，以实现从 Kong 的平滑迁移。

#### 验收标准

1. 当配置 PostgreSQL 连接时，系统应连接并使用与 Kong 相同的 Schema
2. 当启动时，系统应自动执行数据库迁移（如有需要）
3. 当读写实体数据时，系统应与 Kong 的数据格式完全兼容
4. 当配置为 db-less 模式时，系统应从声明式配置文件加载数据
5. 如果需要缓存实体，系统应实现多级缓存（内存缓存 + 数据库查询缓存）

### R5：Lua 插件兼容层

**用户故事：** 作为 Kong 开发工程师，我希望能直接在 Kong-Rust 中运行现有的 Kong Lua 插件，无需修改任何插件代码。

#### 验收标准

1. 当加载 Lua 插件时，系统应通过内嵌 LuaJIT（via mlua crate）执行插件代码
2. 当 Lua 插件调用 PDK 接口（kong.request、kong.response 等）时，系统应提供与 Kong PDK 行为一致的 Rust 实现
3. 当 Lua 插件定义 handler 生命周期方法（init_worker、certificate、rewrite、access、header_filter、body_filter、log）时，系统应在对应的 Pingora 阶段调用这些方法
4. 当 Lua 插件定义 schema.lua 时，系统应解析并验证插件配置
5. 当 Lua 插件使用 DAO 接口访问数据库时，系统应通过桥接层提供数据库访问能力
6. 如果 Lua 插件使用 OpenResty 特有 API（ngx.* 系列），系统应提供兼容层或明确标注不支持的 API 列表

**内置插件兼容清单（47 个）：**

7. 当启用认证类插件（basic-auth、key-auth、jwt、hmac-auth、oauth2、ldap-auth、acl）时，系统应正确执行认证逻辑
8. 当启用日志类插件（file-log、http-log、tcp-log、udp-log、syslog、loggly、datadog、statsd）时，系统应正确发送日志
9. 当启用转换类插件（request-transformer、response-transformer、correlation-id、request-termination、redirect、request-size-limiting）时，系统应正确转换请求/响应
10. 当启用速率限制类插件（rate-limiting、response-ratelimiting）时，系统应正确执行限流
11. 当启用安全类插件（cors、ip-restriction、bot-detection）时，系统应正确实施安全策略
12. 当启用 Serverless 类插件（aws-lambda、azure-functions、pre-function、post-function）时，系统应正确集成外部服务
13. 当启用监控类插件（prometheus、opentelemetry、zipkin）时，系统应正确采集和暴露指标
14. 当启用 AI 类插件（ai-proxy、ai-prompt-decorator 等 6 个）时，系统应正确处理 AI 请求代理
15. 当启用其他插件（proxy-cache、session、acme、grpc-gateway、grpc-web、standard-webhooks）时，系统应正确执行对应逻辑

### R6：配置管理

**用户故事：** 作为 Kong 开发工程师，我希望 Kong-Rust 的配置方式与 Kong 完全兼容，直接使用现有的 kong.conf 配置文件即可启动。

#### 验收标准

1. 当提供 kong.conf 格式的配置文件时，系统应正确解析所有配置项
2. 当设置 KONG_* 环境变量时，系统应用环境变量覆盖配置文件中的值
3. 当配置监听地址时，系统应支持 proxy_listen、admin_listen、status_listen
4. 当配置数据库连接时，系统应支持 pg_host、pg_port 等 PostgreSQL 连接参数
5. 当配置 plugins=bundled 时，系统应加载所有内置插件
6. 当配置 plugins=custom1,custom2 时，系统应只加载指定的插件

### R7：健康检查

**用户故事：** 作为 Kong 开发工程师，我希望系统能主动和被动地检测上游服务健康状态，健康检查的配置和行为与 Kong 完全一致。

#### 验收标准

1. 当配置了主动健康检查时，系统应按配置的间隔周期性地探测上游 Target
2. 当主动探测失败次数达到阈值时，系统应将 Target 标记为不健康
3. 当被动健康检查检测到连续错误时，系统应自动将 Target 标记为不健康
4. 当不健康的 Target 通过健康检查时，系统应自动恢复其为健康状态
5. 当 Target 被标记为不健康时，系统应不再将流量路由到��� Target

### R8：TLS/证书管理

**用户故事：** 作为 Kong 开发工程师，我希望系统支持与 Kong 完全一致的 TLS 证书管理，包括 SNI 匹配和客户端证书验证。

#### 验收标准

1. 当配置了 SSL 证书和 SNI 时，系统应在 TLS 握手时选择正确的证书
2. 当 Service 配置了 client_certificate 时，系统应使用客户端证书连接上游
3. 当 Service 配置了 tls_verify=true 时，系统应验证上游证书
4. 当 Service 配置了 ca_certificates 时，系统应使用指定的 CA 证书链验证

### R9：Hybrid 模式/集群

**用户故事：** 作为 Kong 运维，我希望支持 Control Plane / Data Plane 分离部署（Hybrid 模式），CP 集中管理配置，DP 只负责代理流量，这样可以实现管理平面和数据平面的独立扩缩容和故障隔离。

#### 验收标准

**角色和启动模式：**

1. 当配置 `role=traditional`（默认）时，系统应同时运行 Admin API + Proxy，行为与单节点 Kong 完全一致
2. 当配置 `role=control_plane` 时，系统应只运行 Admin API 和 WebSocket 配置推送服务，不处理代理流量
3. 当配置 `role=data_plane` 时，系统应只运行 Proxy，从 CP 接收配置，不暴露 Admin API

**配置同步（Sync V1 — 全量推送）：**

4. 当 DP 连接到 CP 时，CP 应通过 WebSocket 全量推送当前配置（GZIP 压缩）
5. 当 CP 配置发生变化时，CP 应主动将新配置推送给所有已连接的 DP
6. 当 DP 发送 PING 帧时，帧负载应携带当前配置的 MD5 哈希（32 字符十六进制），CP 通过对比哈希判断 DP 配置是否最新

**增量同步（Sync V2 — JSON-RPC 2.0）：**

7. 当使用 Sync V2 时，CP 和 DP 应通过 JSON-RPC 2.0 协议通信，WebSocket 子协议为 `kong.meta.v1`
8. 当 DP 需要同步配置时，应调用 `kong.sync.v2.get_delta` 方法，参数为 `{ default: { version: "<current_version>" } }`，CP 返回增量 delta 数据
9. 当 CP 配置更新时，CP 应通过 `kong.sync.v2.notify_new_version` 方法通知所有 DP，参数为 `{ default: { new_version: "<version>" } }`
10. 当 DP 应用配置失败时，DP 应通过 `kong.sync.v2.notify_validation_error` 方法通知 CP
11. 当连接建立时，双方应通过 `kong.meta.v1.hello` 方法交换元信息（kong_version、kong_node_id、kong_hostname、rpc_capabilities 等）

**TLS 和连接管理：**

12. 当 CP 和 DP 通信时，应使用 TLS 双向认证（mTLS），通过 `cluster_cert` 和 `cluster_cert_key` 配置
13. 当 DP 与 CP 断开连接时，DP 应使用缓存的配置继续提供代理服务
14. 当 DP 断连后，应在 5-10 秒随机延迟后自动重连（避免雷鸣羊群效应）
15. 当 DP 连接 CP 时，WebSocket 端点路径为 `/v1/outlet`（V1）或 `/v2/outlet`（V2）

**心跳和健康检测：**

16. 当 DP 已连接时，应每 30 秒（`CLUSTERING_PING_INTERVAL`）发送 PING 帧
17. 当 CP 超过 45 秒（PING_INTERVAL × 1.5）未收到 DP 的 PING 时，应将该 DP 标记为离线
18. 当 DP 超过 45 秒未收到 CP 的 PONG 时，应标记连接断开并触发重连

**配置哈希（多级哈希）：**

19. 当计算配置哈希时，系统应分别计算 routes、services、plugins、upstreams、targets 和 rest（其余配置）的子哈希，然后将所有子哈希拼接后再计算总 MD5
20. 当计算子哈希时，系统应使用确定性排序算法：对象按键名排序、数组元素用 `;` 分隔、null 值序列化为 `/null/`

**Admin API：**

21. 当调用 `/clustering/status` 时，系统应返回所有已连接 DP 的状态信息（节点 ID、连接时间、最后通信时间、配置哈希等）

**配置项：**

22. 系统应支持以下集群相关配置项：`role`、`cluster_listen`（默认 `0.0.0.0:8005`）、`cluster_control_plane`（DP 指向的 CP 地址）、`cluster_cert`、`cluster_cert_key`、`cluster_data_plane_purge_delay`、`cluster_max_payload`

## 非功能性需求

### 代码架构和模块化

- **单一职责原则**：每个模块（代理、路由、Admin API、插件系统、数据库层）独立组织
- **模块化设计**：采用 Rust workspace 多 crate 结构，核心组件可独立编译和测试
- **清晰接口**：通过 Rust trait 定义组件间契约，特别是插件系统的 trait 接口
- **分层架构**：
  - `kong-core`：核心数据结构和 trait 定义
  - `kong-proxy`：基于 Pingora 的代理引擎
  - `kong-admin`：Admin API 实现
  - `kong-db`：数据库 DAO 层
  - `kong-plugin-system`：插件加载和执行框架
  - `kong-lua-bridge`：Lua 兼容层（通过 mlua）
  - `kong-config`：配置解析

### 性能

- 单节点吞吐量不低于原版 Kong（合理估计：Rust + Pingora 应显著优于 LuaJIT + OpenResty）
- P99 延迟不高于原版 Kong
- 内存占用不高于原版 Kong
- 利用 Pingora 的多线程共享连接池优化上游连接复用
- Lua 插件通过 LuaJIT 执行，性能应与原版 Kong 接近

### 安全

- 无 unsafe Rust 代码（除 FFI 边界），利用 Rust 类型系统保证内存安全
- Lua 沙箱隔离，防止恶意插件代码影响主进程
- 支持 TLS 1.2/1.3，可配置密码套件
- Admin API 支持 RBAC（与 Kong 兼容）

### 可靠性

- 支持优雅重启（graceful reload），利用 Pingora 内置能力
- Worker panic 不应导致整个进程崩溃
- 数据库断连时应支持降级运行（使用缓存数据）

### 可观测性

- 兼容 Kong 的 Prometheus 指标格式
- 支持结构化日志输出
- 支持 OpenTelemetry 追踪集成

### 兼容性

- 配置文件格式与 Kong 兼容
- Admin API 响应格式与 Kong 兼容
- 数据库 Schema 与 Kong 兼容
- Lua 插件接口（Handler + Schema + PDK）与 Kong 兼容
