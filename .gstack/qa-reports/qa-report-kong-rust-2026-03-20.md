# Kong-Rust QA 报告

**日期**: 2026-03-20
**分支**: dawxy/qa-test-site
**模式**: PostgreSQL
**测试范围**: Admin API (port 8001) + 代理流量 (port 8000)
**测试时长**: ~20 分钟

---

## 健康评分

| 类别 | 得分 | 权重 | 加权得分 |
|------|------|------|----------|
| Console (启动日志) | 85 | 15% | 12.75 |
| Links/Routes | 100 | 10% | 10.00 |
| Visual (API 响应格式) | 85 | 10% | 8.50 |
| Functional | 50 | 20% | 10.00 |
| UX (API 兼容性) | 60 | 15% | 9.00 |
| Performance | 80 | 10% | 8.00 |
| Content (错误消息) | 65 | 5% | 3.25 |
| Accessibility | 90 | 15% | 13.50 |
| **总计** | | | **75.0** |

---

## 问题汇总

| ID | 严重度 | 类别 | 标题 | 状态 |
|----|--------|------|------|------|
| ISSUE-001 | Low | Content | Admin API 404 响应体为空 | deferred |
| ISSUE-003 | Medium | Functional | PUT upsert 不解析 `url` shorthand | deferred |
| ISSUE-004 | Critical | Functional | Target weight 列类型不匹配 (float→integer) | **verified** ✅ |
| ISSUE-005 | High | Functional | Header 路由匹配不严格 | deferred |
| ISSUE-006 | Medium | Functional | preserve_host 丢失端口号 | deferred |
| ISSUE-007 | Medium | Functional | Targets created_at/updated_at 返回 0.0 | deferred |
| ISSUE-008 | Low | Functional | 失败请求仍部分写入数据库 | deferred |
| ISSUE-009 | High | Functional | 负载均衡不分发请求到多个 targets | deferred |
| ISSUE-010 | Medium | Functional | Prometheus 缺少请求级别指标 | deferred |
| ISSUE-011 | Low | Content | 指标中 node_id 全零，version 显示 3.0.0 | deferred |
| ISSUE-012 | Medium | Content | 上游不可达返回 500 空响应体 | deferred |
| ISSUE-013 | Medium | Performance | 1MB 请求体导致 502 超时 | deferred |
| ISSUE-014 | Medium | UX | 代理响应缺少 Kong 特征头 | deferred |
| ISSUE-015 | High | Functional | HTTPS-only 路由匹配 HTTP 请求 | deferred |
| ISSUE-016 | Medium | Functional | Service 缺少 host 字段验证 | deferred |
| ISSUE-017 | Low | Content | 外键冲突返回 400 而非 409 | deferred |

**统计**: 16 个问题发现，1 个已修复，15 个 deferred
**按严重度**: Critical: 1 (已修复), High: 3, Medium: 8, Low: 4

---

## 详细问题描述

### ISSUE-001 [Low] — Admin API 404 响应体为空
- **位置**: `GET /nonexistent` on port 8001
- **现象**: HTTP 404 返回，但响应体完全为空
- **预期**: 返回 JSON 格式错误消息 `{"message":"Not found"}`

### ISSUE-003 [Medium] — PUT upsert 不解析 url shorthand
- **位置**: `PUT /services/{name}`
- **现象**: 使用 `url` 字段创建 service 时，`host` 为空字符串，`created_at`/`updated_at` 为 0
- **复现**: `curl -X PUT /services/test -d '{"name":"test","url":"http://mockbin.org"}'`
- **预期**: `url` 应被解析为 `protocol` + `host` + `port` + `path`

### ISSUE-004 [Critical] — Target weight 列类型不匹配 ✅ 已修复
- **位置**: `crates/kong-db/src/dao/postgres.rs:1160`
- **现象**: `.float("weight")` 导致 Rust 期望 FLOAT8，但 DB schema 定义为 INTEGER
- **修复**: 改为 `.integer("weight")`
- **Commit**: `243b2a6`

### ISSUE-005 [High] — Header 路由匹配不严格
- **位置**: 路由匹配引擎 (kong-router)
- **现象**: Route 配置了 `headers: {"X-Custom": ["value1"]}`，但不带该 header 的请求也被成功匹配
- **复现**: `curl http://localhost:8000/header-test/get`（无 X-Custom header，仍返回 200）
- **预期**: 缺少必要 header 的请求应不匹配此路由

### ISSUE-006 [Medium] — preserve_host 丢失端口号
- **位置**: 代理引擎 (kong-proxy)
- **现象**: `preserve_host=true` 时，上游收到 `Host: 127.0.0.1`，丢失了 `:8000`
- **预期**: 应保留完整的 `Host: 127.0.0.1:8000`

### ISSUE-007 [Medium] — Targets created_at/updated_at 返回 0.0
- **位置**: Targets DAO
- **现象**: 新建 Target 的 `created_at` 和 `updated_at` 字段返回 `0.0`
- **预期**: 应返回实际的 Unix 时间戳

### ISSUE-008 [Low] — 失败请求仍部分写入数据库
- **位置**: Targets DAO 事务处理
- **现象**: Target 创建返回类型错误，但数据实际已写入 DB（重启后可见）
- **预期**: 写入失败应回滚事务，不应有脏数据

### ISSUE-009 [High] — 负载均衡不分发请求到多个 targets
- **位置**: kong-proxy 负载均衡器
- **现象**: Upstream 有 2 个 targets（weight 100 和 50），15 个请求全部发送到第一个 target
- **预期**: Round-robin 算法应按权重比例分发请求

### ISSUE-010 [Medium] — Prometheus 缺少请求级别指标
- **位置**: Prometheus 插件
- **现象**: `/metrics` 仅显示基础连接指标，缺少 `kong_http_requests_total`、`kong_latency`、`kong_bandwidth_bytes`
- **预期**: 每个请求应产生按 service/route/status_code 分组的指标

### ISSUE-011 [Low] — 指标中 node_id 全零，version 不正确
- **位置**: Prometheus 指标输出
- **现象**: `node_id="00000000-0000-0000-0000-000000000001"`，`version="3.0.0"`
- **预期**: node_id 应为实际 node UUID，version 应为 `0.1.0`

### ISSUE-012 [Medium] — 上游不可达返回 500 空响应体
- **位置**: 代理错误处理
- **现象**: 上游 DNS 解析失败时返回 HTTP 500 但响应体为空
- **预期**: 应返回 JSON 格式 `{"message":"An invalid response was received from the upstream server"}`

### ISSUE-013 [Medium] — 1MB 请求体导致 502 超时
- **位置**: 代理请求转发
- **现象**: 发送 1MB 请求体到 /test/post，等待 60 秒后返回 502
- **预期**: 应能处理大请求体的转发

### ISSUE-014 [Medium] — 代理响应缺少 Kong 特征头
- **位置**: 代理响应头注入
- **现象**: 响应中缺少 `Server`、`X-Kong-Upstream-Latency`、`X-Kong-Proxy-Latency`、`X-Kong-Request-Id`
- **预期**: Kong 标准响应头应被注入到所有代理响应中

### ISSUE-015 [High] — HTTPS-only 路由匹配 HTTP 请求
- **位置**: 路由匹配引擎协议过滤
- **现象**: Route 配置 `protocols: ["https"]`，但 HTTP 请求仍被匹配并转发到上游
- **预期**: HTTP 请求不应匹配 HTTPS-only 路由，应返回 426 Upgrade Required

### ISSUE-016 [Medium] — Service 缺少 host 字段验证
- **位置**: Admin API Service 创建
- **现象**: `host` 为空字符串的 Service 创建成功（201）
- **预期**: 应返回 400，host 是 Service 的必填字段

### ISSUE-017 [Low] — 外键冲突返回 400 而非 409
- **位置**: Admin API 错误码映射
- **现象**: 删除有关联路由的 Service 返回 HTTP 400
- **预期**: Kong 标准返回 409 Conflict

---

## 通过的测试

| 测试项 | 结果 |
|--------|------|
| Admin API GET / 根信息 | ✅ 正确返回配置、版本、插件信息 |
| Admin API GET /status | ✅ 数据库可达，连接信息正确 |
| Admin API GET /plugins/enabled | ✅ 返回 prometheus, ai-proxy |
| Admin API GET /schemas/plugins/prometheus | ✅ 返回完整 schema |
| Services POST/GET/PATCH/DELETE | ✅ CRUD 全部正常 |
| Services 重复名称检测 | ✅ 返回 409 Conflict |
| Services 不存在资源 | ✅ 返回 404 |
| Routes POST (path/host/method/header) | ✅ 所有匹配条件创建正常 |
| Routes 嵌套路由 /services/{id}/routes | ✅ 正确返回关联路由 |
| Routes PATCH 更新 | ✅ 字段更新生效 |
| Consumers CRUD | ✅ 基本操作正常 |
| Plugins 全局创建 | ✅ prometheus 插件启用成功 |
| Upstreams CRUD | ✅ 创建成功，健康检查配置完整 |
| 分页查询 (size + offset) | ✅ 分页工作正常 |
| 代理基础 HTTP 转发 | ✅ 请求正确转发到上游 |
| strip_path=true | ✅ 路径前缀正确剥离 |
| strip_path=false | ✅ 路径完整保留 |
| X-Forwarded-* 头注入 | ✅ Host/Path/Prefix 正确注入 |
| Host 路由匹配 | ✅ 精确匹配和通配符均工作 |
| Method 路由限制 | ✅ 不允许的方法返回 404 |
| 正则路径匹配 | ✅ 正则匹配和不匹配均正确 |
| 无匹配路由 404 | ✅ 返回 JSON 错误消息 |
| POST 请求体转发 | ✅ JSON body 正确透传 |
| Query 参数保留 | ✅ 查询参数完整保留 |
| URL 编码/特殊字符 | ✅ 中文和 emoji 正确处理 |
| 长 URL 处理 | ✅ 2000+ 字符 URL 正常 |
| 并发请求 (10 并发) | ✅ 全部返回 200 |
| Admin API JSON 解析错误 | ✅ 返回 400 |

---

## Top 3 Things to Fix

1. **ISSUE-009 [High]: 负载均衡不工作** — 这是核心代理功能，影响所有使用 Upstream 的生产部署
2. **ISSUE-005 [High]: Header 路由匹配不严格** — 安全隐患，可能导致请求绕过 header 条件限制
3. **ISSUE-015 [High]: HTTPS-only 路由匹配 HTTP 请求** — 安全隐患，可能导致敏感路由通过非加密通道访问

---

## PR 摘要

> QA 发现 16 个问题（3 High, 8 Medium, 4 Low, 1 Critical 已修复），修复 1 个，健康评分 75/100。
