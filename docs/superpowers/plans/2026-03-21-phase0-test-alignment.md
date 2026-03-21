# Phase 0: Kong 官方测试对齐计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 使 Kong-Rust 能够直接运行 Kong 官方 spec 文件，通过 busted 测试框架 + spec.helpers 兼容层验证 100% 行为兼容性

**前提条件:** Phase 0 Step 1 已完成 — busted CLI 安装、spec/helpers.lua 基础骨架（363 行）、luasocket HTTP 客户端、Blueprint、烟雾测试

---

## 全局架构

```
┌──────────────────────────────────────────────────────────────────┐
│                    Kong Spec Test Architecture                    │
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌───────────────────┐  │
│  │ busted CLI   │───▶│ helpers.lua  │───▶│ kong-rust binary  │  │
│  │ (test runner)│    │ (兼容层)      │    │ (被测目标)         │  │
│  └──────────────┘    └──────┬───────┘    └──────┬────────────┘  │
│                             │                    │               │
│                    ┌────────▼────────┐   ┌──────▼──────┐       │
│                    │ http_client.lua │   │ :9001 Admin │       │
│                    │ (luasocket)     │──▶│ :9000 Proxy │       │
│                    └─────────────────┘   │ :9443 SSL   │       │
│                                          └─────────────┘       │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ mock_upstream (Rust axum server on :15555/:15556)        │   │
│  │  /             — 回显请求信息（JSON）                      │   │
│  │  /get          — 回显 GET 请求详情                         │   │
│  │  /post         — 回显 POST 请求详情                        │   │
│  │  /request      — 回显任何请求（含 headers/body/uri）       │   │
│  │  /anything     — 同 /request                              │   │
│  │  /status/{code}— 返回指定状态码                            │   │
│  │  /delay/{sec}  — 延迟响应                                  │   │
│  │  /response-headers — 设置自定义响应头                      │   │
│  │  /cache/{n}    — 缓存控制                                  │   │
│  │  /basic-auth/{user}/{pass} — 基础认证                     │   │
│  │  /stream/{n}   — 分块流式响应                              │   │
│  │  /ws           — WebSocket echo                           │   │
│  │  /post_log/{name} — 日志收集                              │   │
│  │  /read_log/{name} — 日志读取                              │   │
│  └──────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────┘
```

## 测试规模概览

| 目录 | Kong 文件数 | Kong 代码行 | Kong-Rust 可运行 | 优先级 |
|------|------------|------------|-----------------|--------|
| 02-integration/04-admin_api | 25 | 15,448 | ~20 (80%) | P0 |
| 02-integration/05-proxy | 34 | 15,115 | ~25 (74%) | P0 |
| 02-integration/03-db | 21 | 7,542 | ~15 (71%) | P1 |
| 02-integration/08-status_api | 5 | 1,156 | 5 (100%) | P1 |
| 02-integration/11-dbless | 4 | 593 | 4 (100%) | P1 |
| 03-plugins (核心插件) | ~70 | ~40,000 | ~50 (71%) | P2 |
| 01-unit (相关部分) | ~20 | ~8,000 | ~10 (50%) | P3 |

**总计约 170 个可运行 spec 文件，约 88,000 行 Lua 测试代码。**

---

## Task 1: Mock Upstream Server（Rust 实现）

**Files:**
- Create: `crates/kong-server/src/mock_upstream.rs`
- Modify: `crates/kong-server/src/main.rs` (添加 `--mock-upstream` 子命令)
- Modify: `spec/helpers.lua` (添加 mock_upstream_* 常量和自动启停)

Kong 的大量 spec 依赖 mock upstream server（端口 15555/15556）。原版 Kong 在 nginx 内嵌 Lua 实现，Kong-Rust 需要独立的 mock server。

**用 Rust axum 实现，因为：**
1. 项目已经依赖 axum（Admin API 用）
2. 比 Lua 实现更可靠，不需要 openresty
3. 可以作为 kong-rust 二进制的子命令启动

- [ ] **Step 1: 创建 mock_upstream.rs**

使用 axum 实现以下端点（参考 `/Users/dawxy/proj/kong/spec/fixtures/mock_upstream.lua` 和 `1.2_custom_nginx.template`）：

```
核心端点：
GET/POST/...  /                    → JSON 回显请求信息
GET           /get                  → 回显 GET 请求详情（headers, args, url）
POST          /post                 → 回显 POST 请求详情（含 body）
GET/POST/...  /request              → 同 /anything
GET/POST/...  /anything             → 完整回显（method, uri, headers, body, args）
GET           /status/{code}        → 返回指定 HTTP 状态码
GET           /delay/{seconds}      → 延迟指定秒数后响应
GET           /response-headers     → 从 query 参数设置响应头
GET           /cache/{n}            → 设置 Cache-Control: public, max-age={n}
GET           /basic-auth/{u}/{p}   → 验证 Basic Auth 凭证
GET           /stream/{n}           → 返回 n 行分块响应
POST          /post_log/{name}      → 存储请求 body 到内存（日志收集）
GET           /read_log/{name}      → 读取存储的日志
GET           /count_log/{name}     → 返回日志条数
DELETE        /reset_log/{name}     → 清除存储的日志
```

关键设计：
- 所有回显端点统一使用 `EchoResponse` 结构体
- 响应头固定 `X-Powered-By: mock_upstream` 和 `Server: mock-upstream/1.0.0`
- 日志存储使用 `Arc<RwLock<HashMap<String, Vec<String>>>>` 共享状态
- 监听端口默认 15555（HTTP）和 15556（HTTPS）

```rust
// 核心结构体参考
#[derive(Serialize)]
struct EchoResponse {
    method: String,
    url: String,
    uri: String,
    headers: HashMap<String, String>,
    data: String,           // raw body
    json: Option<Value>,    // parsed JSON body (if applicable)
    args: HashMap<String, String>, // query parameters
}
```

- [ ] **Step 2: 在 main.rs 添加 mock-upstream 子命令**

```rust
// 在 CLI 解析中添加
Command::MockUpstream { port, ssl_port } => {
    mock_upstream::run(port.unwrap_or(15555), ssl_port).await
}
```

- [ ] **Step 3: 更新 helpers.lua 添加 mock upstream 支持**

在 `spec/helpers.lua` 中添加：

```lua
-- Mock upstream constants — Mock 上游服务常量
_M.mock_upstream_protocol = "http"
_M.mock_upstream_host = "127.0.0.1"
_M.mock_upstream_hostname = "localhost"
_M.mock_upstream_port = tonumber(os.getenv("KONG_SPEC_MOCK_UPSTREAM_PORT")) or 15555
_M.mock_upstream_ssl_port = tonumber(os.getenv("KONG_SPEC_MOCK_UPSTREAM_SSL_PORT")) or 15556
_M.mock_upstream_url = string.format("http://127.0.0.1:%d", _M.mock_upstream_port)
_M.mock_upstream_ssl_url = string.format("https://127.0.0.1:%d", _M.mock_upstream_ssl_port)
_M.mock_upstream_stream_port = 15557
_M.mock_upstream_stream_ssl_port = 15558

-- 在 start_kong() 中自动启动 mock upstream
-- 在 stop_kong() 中自动停止 mock upstream
```

- [ ] **Step 4: 编写测试验证 mock upstream**

创建 `spec/00-smoke/02-mock_upstream_spec.lua`，验证所有端点正常工作。

- [ ] **Step 5: Commit**

```
feat: 添加 mock upstream server（Rust axum 实现）
add mock upstream server for Kong spec compatibility (Rust axum)
```

---

## Task 2: 补全 spec.helpers 兼容层

**Files:**
- Modify: `spec/helpers.lua`
- Modify: `spec/fixtures/http_client.lua`

当前 helpers.lua 只有 363 行，缺少大量 Kong spec 依赖的 API。需要按 Kong 原版 helpers.lua 导出列表逐一补全。

- [ ] **Step 1: 补全 helpers 缺失的 API**

对比 Kong 原版 helpers.lua（229 行导出表）和当前实现（363 行），以下 API 需要补充：

```lua
-- 1. Penlight 工具（如果有 spec 用到 helpers.path/file/dir）
_M.dir = require("pl.dir")    -- 需要 luarocks install penlight
_M.path = require("pl.path")
_M.file = require("pl.file")
_M.utils = require("pl.utils")

-- 2. Mock upstream 常量（Task 1 已覆盖）

-- 3. HTTP 客户端增强
_M.http_client = http_client.new  -- 通用 HTTP 客户端构造器

-- 4. 进程控制增强
_M.restart_kong = function(conf) ... end
_M.reload_kong = function(conf) ... end
_M.cleanup_kong = function(prefix) ... end

-- 5. 缺失的 wait 工具
_M.pwait_until = function(fn, timeout) ... end  -- protected wait
_M.wait_for_file = function(path, timeout) ... end
_M.wait_for_file_contents = function(path, timeout) ... end

-- 6. 缺失的 shell 工具
_M.kong_exec = function(cmd, env) ... end  -- 执行 kong CLI 命令

-- 7. 缺失的 misc 工具
_M.unindent = function(str) ... end
_M.make_yaml_file = function(content) ... end
_M.setenv = function(name, value) ... end
_M.unsetenv = function(name) ... end
_M.deep_sort = function(tbl) ... end

-- 8. TCP/UDP 服务器（插件日志测试需要）
_M.tcp_server = function(port, opts) ... end
_M.udp_server = function(port, opts) ... end

-- 9. get_db_utils 增强 — 支持 strategy 参数和 clean 选项
_M.get_db_utils = function(strategy, tables, plugins) ... end

-- 10. test_conf 补全 — 添加更多配置字段
_M.test_conf.proxy_listen = ...
_M.test_conf.admin_listen = ...
_M.test_conf.prefix = ...
```

- [ ] **Step 2: 增强 http_client.lua**

当前 HTTP 客户端缺少一些 Kong spec 依赖的特性：

```lua
-- 1. 支持 Host header 透传（代理测试核心）
-- 当前实现不会自动设置 Host header，但代理测试需要通过 Host 头路由

-- 2. 支持 chunked transfer encoding 检测

-- 3. 支持原始 socket 模式（某些测试需要发送畸形请求）

-- 4. 响应对象增加 read_body() 方法的正确实现
```

- [ ] **Step 3: 安装 penlight 依赖**

在 `scripts/setup-busted.sh` 中添加：
```bash
luarocks install --local --lua-version=5.1 penlight
```

- [ ] **Step 4: Commit**

```
feat: 补全 spec.helpers 兼容层 API
complete spec.helpers compatibility layer API coverage
```

---

## Task 3: 断言系统对齐

**Files:**
- Modify: `spec/helpers.lua`

Kong 的 spec 大量使用自定义断言（在 `spec/internal/asserts.lua` 中定义）。

- [ ] **Step 1: 实现 Kong 自定义断言**

参考 `/Users/dawxy/proj/kong/spec/internal/asserts.lua`，实现以下断言：

```lua
-- 核心断言（几乎所有 spec 都用）
assert.response(res).has.status(200)
assert.response(res).has.header("X-Custom")
assert.response(res).has.no.header("X-Should-Not-Exist")
assert.response(res).has.jsonbody()

-- 请求断言
assert.request(res).has.header("X-Forwarded-For")
assert.request(res).has.queryparam("foo")
assert.request(res).has.formparam("bar")

-- 高级断言
assert.cn(cert_pem, "expected-cn")  -- 证书 CN 检查
assert.matches("pattern", actual)   -- 正则匹配
assert.near(expected, actual, tolerance)  -- 近似相等

-- 错误断言
assert.error_matches(fn, "expected error pattern")
```

关键实现点：
- `assert.response(res)` 需要返回一个 proxy 对象，支持链式调用 `.has.status()`
- `assert.request(res)` 需要从 mock upstream 回显的 JSON 中提取请求信息
- 当前 helpers.lua 中的 `_M.assert_response` 不符合 Kong 原版 API（Kong 用 `assert.response()`，不是 `helpers.assert_response()`）

- [ ] **Step 2: 注册断言到 luassert**

```lua
local say = require("say")
local luassert = require("luassert")

-- 使用 luassert 的 register 机制，确保 assert.response() 语法正确
```

- [ ] **Step 3: 编写断言测试**

创建 `spec/00-smoke/03-assertions_spec.lua`，验证所有自定义断言。

- [ ] **Step 4: Commit**

```
feat: 对齐 Kong 自定义断言系统
align custom assertion system with Kong spec/internal/asserts.lua
```

---

## Task 4: 运行 Admin API spec 并修复兼容性问题

**Files:**
- Copy from Kong: `spec/02-integration/04-admin_api/*.lua` (选择性复制)
- Modify: 多个 Rust crate 文件（根据测试失败修复）

这是第一个真正对齐 Kong 官方 spec 的 task。

- [ ] **Step 1: 复制核心 Admin API spec 文件**

从 `/Users/dawxy/proj/kong/spec/02-integration/04-admin_api/` 复制以下文件到 `spec/02-integration/04-admin_api/`：

```
优先级 P0（必须通过）：
01-admin_api_spec.lua          — Admin API 基础（监听、根路由、/status）
09-routes_routes_spec.lua      — Route CRUD + 嵌套路由
10-services_routes_spec.lua    — Service CRUD
03-consumers_routes_spec.lua   — Consumer CRUD
04-plugins_routes_spec.lua     — Plugin CRUD
14-tags_spec.lua               — Tags 过滤
02-kong_routes_spec.lua        — / 和 /status 路由

优先级 P1（应该通过）：
06-certificates_routes_spec.lua — Certificate CRUD
07-upstreams_routes_spec.lua    — Upstream CRUD
08-targets_routes_spec.lua      — Target CRUD
16-ca_certificates_routes_spec.lua — CA Certificate CRUD
```

- [ ] **Step 2: 运行第一个 spec 文件，收集失败**

```bash
make spec-file FILE=spec/02-integration/04-admin_api/10-services_routes_spec.lua
```

预期会有大量失败，主要原因：
1. helpers.lua API 不完整
2. Admin API 响应格式差异
3. 分页、排序行为差异
4. 错误响应格式差异

- [ ] **Step 3: 逐个修复 Admin API 兼容性问题**

常见修复类型：
- 响应 JSON 字段缺失/多余
- 分页 offset/next 格式
- 错误消息文本差异
- 排序行为差异
- Content-Type 处理差异

- [ ] **Step 4: 每修复一批问题后 commit**

```
fix: 修复 Admin API 兼容性问题（services routes）
fix Admin API compatibility issues (services routes spec alignment)
```

---

## Task 5: 运行 Proxy spec 并修复兼容性问题

**Files:**
- Copy from Kong: `spec/02-integration/05-proxy/*.lua` (选择性复制)
- Modify: `crates/kong-proxy/`, `crates/kong-router/` 等

- [ ] **Step 1: 复制核心 Proxy spec 文件**

```
优先级 P0（必须通过）：
01-proxy_spec.lua              — 代理基础（监听、连接）
02-router_spec.lua             — 路由匹配（2869 行，67 个测试）

优先级 P1（应该通过）：
03-upstream_headers_spec.lua   — 上游请求头
06-ssl_spec.lua                — TLS/SNI 匹配
08-uri_encoding_spec.lua       — URI 编码处理
14-server_tokens_spec.lua      — Server/Via 头
24-buffered_spec.lua           — Body buffering
33-request-id-header_spec.lua  — X-Kong-Request-Id

优先级 P2（可以延后）：
04-plugins_triggering_spec.lua — 插件触发顺序
09-websockets_spec.lua         — WebSocket 代理
12-error_default_type_spec.lua — 错误响应类型
```

- [ ] **Step 2: 从最简单的 spec 开始运行**

先跑 `01-proxy_spec.lua`（306 行，8 个测试），这是最简单的代理测试，验证：
- 代理端口是否正确监听
- 基本请求转发是否工作

- [ ] **Step 3: 运行路由 spec**

`02-router_spec.lua` 是最关键的测试（2869 行），覆盖：
- hosts 匹配（精确、通配符、多值）
- paths 匹配（前缀、正则、strip_path）
- methods 匹配
- headers 匹配
- 优先级排序
- expressions 路由

- [ ] **Step 4: 逐个修复兼容性问题**

- [ ] **Step 5: 每修复一批问题后 commit**

---

## Task 6: 运行 DB spec 并修复兼容性问题

**Files:**
- Copy from Kong: `spec/02-integration/03-db/*.lua` (选择性复制)

- [ ] **Step 1: 复制核心 DB spec 文件**

```
01-db_spec.lua                 — DB 基础操作
02-db_core_entities_spec.lua   — 核心实体 CRUD
14-dao_spec.lua                — DAO 接口
```

- [ ] **Step 2: 运行并修复**

DB spec 主要验证：
- 实体 CRUD 操作
- 唯一约束
- 外键关系
- 分页查询
- 缓存行为

---

## Task 7: 运行 Status API 和 DB-less spec

**Files:**
- Copy from Kong: `spec/02-integration/08-status_api/*.lua`
- Copy from Kong: `spec/02-integration/11-dbless/*.lua`

这两组测试相对简单，应该能较快通过。

- [ ] **Step 1: 复制并运行 Status API spec**
- [ ] **Step 2: 复制并运行 DB-less spec**
- [ ] **Step 3: 修复发现的问题**

---

## Task 8: 核心插件 spec 对齐

**Files:**
- Copy from Kong: `spec/03-plugins/` (选择性复制)

插件测试依赖 Lua 插件兼容层。优先运行 Kong-Rust 已支持的插件。

- [ ] **Step 1: 按优先级复制插件 spec**

```
P0 认证插件：
09-key-auth/           — key-auth 认证
10-basic-auth/         — basic-auth 认证
18-acl/                — ACL 访问控制

P0 流量控制：
23-rate-limiting/      — 限流
12-request-size-limiting/ — 请求大小限制
14-request-termination/   — 请求终止

P1 日志插件：
03-http-log/           — HTTP 日志
04-file-log/           — 文件日志

P1 变换插件：
36-request-transformer/ — 请求变换
15-response-transformer/ — 响应变换
13-cors/               — CORS

P2 高级插件：
16-jwt/                — JWT 认证
25-oauth2/             — OAuth2
26-prometheus/         — Prometheus 指标
11-correlation-id/     — 关联 ID
```

- [ ] **Step 2: 先跑 key-auth spec（最简单的认证插件）**

```bash
make spec-file FILE=spec/03-plugins/09-key-auth/01-api_spec.lua
make spec-file FILE=spec/03-plugins/09-key-auth/02-access_spec.lua
```

- [ ] **Step 3: 逐步扩展到更多插件**

- [ ] **Step 4: 记录每个插件的通过率**

---

## Task 9: CI 集成

**Files:**
- Create: `.github/workflows/spec-tests.yml`
- Modify: `Makefile`

- [ ] **Step 1: 创建 GitHub Actions workflow**

```yaml
name: Kong Spec Tests
on: [push, pull_request]
jobs:
  spec-tests:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:15
        env:
          POSTGRES_USER: kong
          POSTGRES_PASSWORD: kong
          POSTGRES_DB: kong_tests
        ports:
          - 5432:5432
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Setup busted
        run: bash scripts/setup-busted.sh
      - name: Build kong-rust
        run: cargo build
      - name: Run spec tests
        run: make spec
```

- [ ] **Step 2: 添加 spec 测试报告**

将 busted TAP 输出转换为 GitHub Actions 可读格式。

- [ ] **Step 3: Commit**

---

## Task 10: 更新项目文档

**Files:**
- Modify: `docs/tasks.md`
- Create: `docs/implementation-logs/phase0-test-alignment.md`

- [ ] **Step 1: 更新 tasks.md**

将 8.12a 标记为已完成，添加新的测试对齐任务。

- [ ] **Step 2: 创建实现日志**

---

## 依赖关系

```
Task 1 (Mock Upstream) ──┐
                         ├──▶ Task 4 (Admin API spec)
Task 2 (helpers 补全) ───┤
                         ├──▶ Task 5 (Proxy spec)      ──┐
Task 3 (断言系统) ───────┘                                ├──▶ Task 8 (插件 spec)
                         Task 6 (DB spec) ────────────────┘
                         Task 7 (Status/DBless spec) ─────┘

Task 9 (CI) — 独立，可随时开始
Task 10 (文档) — 最后执行
```

**建议执行顺序：**
1. Task 1 + Task 2 + Task 3（并行，基础设施）
2. Task 4（Admin API spec — 最容易验证进展）
3. Task 5（Proxy spec — 核心代理功能验证）
4. Task 6 + Task 7（DB + Status/DBless — 补充覆盖）
5. Task 8（插件 spec — 最复杂，依赖前面所有 task）
6. Task 9（CI 集成）
7. Task 10（文档更新）

---

## 成功标准

| 里程碑 | 标准 | 预估工作量 (CC) |
|--------|------|----------------|
| M1: 基础设施就绪 | Mock upstream + helpers 补全 + 断言系统 | ~1h |
| M2: Admin API spec 通过 | 10 个核心 Admin API spec 文件 ≥90% 通过 | ~2h |
| M3: Proxy spec 通过 | 5 个核心 Proxy spec 文件 ≥80% 通过 | ~3h |
| M4: 核心插件 spec 通过 | key-auth + rate-limiting + cors ≥80% 通过 | ~2h |
| M5: CI 绿色 | GitHub Actions spec 测试全部通过 | ~30min |

## NOT in scope

- **01-unit spec**：这些是 Kong 内部 Lua 实现的单测（如 router 内部算法），Kong-Rust 用 Rust 重写了这些模块，有自己的 Rust 单测
- **09-hybrid_mode spec**：Hybrid 模式尚未实现（阶段 9）
- **grpc 相关 spec**：Kong-Rust 目前不支持 gRPC 代理
- **04-perf spec**：性能测试需要独立框架
- **05-migration spec**：迁移测试与 Kong-Rust 无关
- **06-third-party spec**：第三方集成测试
- **外部插件（10-external-plugins）**：非核心功能
- **AI 插件 spec**：待 AI Gateway 战略落地后再考虑

## What already exists

| 已有组件 | 位置 | 复用情况 |
|----------|------|---------|
| helpers.lua 基础骨架 | spec/helpers.lua (363行) | 需要扩展到 ~800 行 |
| HTTP 客户端 | spec/fixtures/http_client.lua | 需要增强 |
| 烟雾测试 | spec/00-smoke/01-admin_api_spec.lua | 保留作为回归测试 |
| Rust spec runner | crates/kong-server/tests/spec_runner.rs | 保留但非主要入口 |
| busted 安装脚本 | scripts/setup-busted.sh | 需要添加 penlight |
| run-specs.sh | scripts/run-specs.sh | 保留 |
| Makefile spec targets | Makefile | 保留 |
| Rust 单测 (138个) | crates/*/tests/*.rs | 保留，与 spec 互补 |
