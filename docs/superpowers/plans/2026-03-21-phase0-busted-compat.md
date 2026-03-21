# Phase 0: busted + spec.helpers 兼容层 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 搭建 busted 测试框架 + spec.helpers 兼容层，能直接运行 Kong 官方 spec 文件验证 Kong-Rust 兼容性

**Architecture:** 进程级集成测试架构。Rust 编译 kong-rust 二进制 → 启动为子进程 → busted CLI 执行 Kong spec 文件 → spec/helpers.lua 通过 luasocket HTTP 与 Kong-Rust Admin API 通信创建 fixture、通过 HTTP 客户端访问代理端口验证行为。不依赖 openresty/ngx.socket，纯 luasocket + busted 生态。

**Tech Stack:** busted (Lua test framework via luarocks), luasocket (HTTP client), luassert (assertions), Kong-Rust binary (test target)

---

## 架构概览

```
┌─────────────────────────────────────────────────┐
│ Rust: cargo test (integration test entry point) │
│                                                 │
│  1. Build kong-rust binary                      │
│  2. Start PostgreSQL (if needed)                │
│  3. Run migrations                              │
│  4. Start kong-rust as subprocess               │
│  5. Invoke busted CLI on spec files             │
│  6. Collect exit code → pass/fail               │
│  7. Stop kong-rust                              │
└────────┬──────────────────────────┬─────────────┘
         │                          │
         ▼                          ▼
┌─────────────────┐    ┌─────────────────────────┐
│ kong-rust binary │    │ busted CLI              │
│ (subprocess)     │    │                         │
│                  │    │ loads spec/helpers.lua   │
│ :9000 proxy      │◄───│ helpers.admin_client()  │
│ :9001 admin      │    │ helpers.proxy_client()  │
│                  │    │ helpers.get_db_utils()   │
└─────────────────┘    └─────────────────────────┘
```

**关键设计决策：**

| 决策 | 选择 | 理由 |
|------|------|------|
| 测试框架 | 原生 busted CLI (luarocks) | 100% 兼容，无需重新实现 |
| HTTP 客户端 | luasocket | busted 环境无 openresty，luasocket 是标准选择 |
| Fixture 创建 | Admin API (HTTP) | 复用已有 Admin API，无需 Lua 直连 DB |
| 进程管理 | Rust integration test 启动/停止 | cargo test 自动管理生命周期 |
| 端口 | 代理 9000/9443，Admin 9001 | 与 Kong 官方测试端口一致 |

---

## 文件结构

```
spec/                                  # Kong-compatible spec directory
├── helpers.lua                        # spec.helpers 兼容 shim（核心）
├── kong_tests.conf                    # 测试用 kong.conf
├── fixtures/                          # 测试辅助工具
│   └── http_client.lua                # luasocket HTTP 客户端封装
│
crates/kong-server/
├── tests/
│   └── spec_runner.rs                 # Rust integration test: 启动 kong-rust + 运行 busted
│
scripts/
├── setup-busted.sh                    # 安装 busted + luarocks 依赖
└── run-specs.sh                       # 运行 Kong spec 测试的便捷脚本
│
Makefile                               # 新增 spec 相关 target
```

---

### Task 1: 安装 busted 工具链

**Files:**
- Create: `scripts/setup-busted.sh`
- Modify: `Makefile`

- [ ] **Step 1: 创建 busted 安装脚本**

```bash
#!/usr/bin/env bash
# scripts/setup-busted.sh
# 安装 busted 测试框架及其依赖 — Install busted test framework and dependencies

set -euo pipefail

echo "=== 安装 luarocks (如果未安装) ==="
if ! command -v luarocks &>/dev/null; then
    if command -v brew &>/dev/null; then
        brew install luarocks
    else
        echo "错误: 请先安装 luarocks (brew install luarocks 或从 https://luarocks.org 下载)"
        exit 1
    fi
fi

echo "=== 安装 busted 测试框架 ==="
luarocks install --local --lua-version=5.1 busted 2.2.0-1

echo "=== 安装 luasocket (HTTP 客户端) ==="
luarocks install --local --lua-version=5.1 luasocket 3.1.0-1

echo "=== 安装 luasec (HTTPS 支持) ==="
luarocks install --local --lua-version=5.1 luasec

echo "=== 安装 lua-cjson (JSON 编解码) ==="
luarocks install --local --lua-version=5.1 lua-cjson 2.1.0.14-1

echo "=== 验证安装 ==="
eval "$(luarocks path --bin)"
busted --version

echo "=== 安装完成 ==="
echo "运行 'eval \"\$(luarocks path --bin)\"' 或将其加入 shell profile"
```

- [ ] **Step 2: 运行安装脚本验证**

Run: `chmod +x scripts/setup-busted.sh && bash scripts/setup-busted.sh`
Expected: busted --version 输出版本号

- [ ] **Step 3: 在 Makefile 中添加 spec 相关 target**

在 Makefile 末尾添加：

```makefile
# ========================
# Kong Spec 测试 (busted)
# ========================

.PHONY: setup-busted spec spec-verbose

setup-busted:
	@bash scripts/setup-busted.sh

spec: ## 运行 Kong 官方 spec 测试
	@eval "$$(luarocks path --bin)" && \
	cargo build && \
	busted --helper=spec/helpers.lua spec/ -o TAP --no-auto-insulate

spec-verbose: ## 运行 Kong spec 测试 (详细输出)
	@eval "$$(luarocks path --bin)" && \
	cargo build && \
	busted --helper=spec/helpers.lua spec/ -o utfTerminal --no-auto-insulate -v
```

- [ ] **Step 4: Commit**

```bash
git add scripts/setup-busted.sh Makefile
git commit -m "chore: 添加 busted 测试框架安装脚本和 Makefile target
add busted test framework setup script and Makefile spec targets"
```

---

### Task 2: luasocket HTTP 客户端封装

**Files:**
- Create: `spec/fixtures/http_client.lua`

Kong 的 spec.helpers 需要一个 HTTP 客户端来与 Kong-Rust 通信。原版 Kong 用 `resty.http`（需要 openresty），我们用 `luasocket` 替代。

- [ ] **Step 1: 创建 HTTP 客户端模块**

```lua
-- spec/fixtures/http_client.lua
-- HTTP client wrapper using luasocket — 基于 luasocket 的 HTTP 客户端封装
-- Compatible with Kong spec.helpers proxy_client/admin_client API

local socket = require("socket")
local http_socket = require("socket.http")
local ltn12 = require("ltn12")
local url_mod = require("socket.url")

local _M = {}
local Client = {}
Client.__index = Client

-- Create a new HTTP client — 创建新的 HTTP 客户端
-- @param host string 主机地址
-- @param port number 端口号
-- @param opts table 可选参数 {timeout, scheme}
function _M.new(host, port, opts)
    opts = opts or {}
    local self = setmetatable({}, Client)
    self.host = host or "127.0.0.1"
    self.port = port
    self.scheme = opts.scheme or "http"
    self.timeout = (opts.timeout or 10) * 1000  -- luasocket uses ms
    return self
end

-- Build full URL from path — 从路径构建完整 URL
function Client:_url(path)
    return string.format("%s://%s:%d%s", self.scheme, self.host, self.port, path or "/")
end

-- Send HTTP request — 发送 HTTP 请求
-- @param opts table {method, path, headers, body, query}
-- @return response table {status, headers, body}
function Client:send(opts)
    opts = opts or {}
    local method = (opts.method or "GET"):upper()
    local path = opts.path or "/"

    -- Append query string — 附加查询字符串
    if opts.query then
        local parts = {}
        for k, v in pairs(opts.query) do
            parts[#parts + 1] = url_mod.escape(tostring(k)) .. "=" .. url_mod.escape(tostring(v))
        end
        if #parts > 0 then
            path = path .. "?" .. table.concat(parts, "&")
        end
    end

    local full_url = self:_url(path)
    local response_body = {}
    local request_body = opts.body

    -- Handle JSON body — 处理 JSON body
    if type(request_body) == "table" then
        local cjson = require("cjson")
        request_body = cjson.encode(request_body)
        opts.headers = opts.headers or {}
        if not opts.headers["Content-Type"] and not opts.headers["content-type"] then
            opts.headers["Content-Type"] = "application/json"
        end
    end

    local req_headers = {}
    if opts.headers then
        for k, v in pairs(opts.headers) do
            req_headers[k] = v
        end
    end

    if request_body then
        req_headers["Content-Length"] = tostring(#request_body)
    end

    -- Set Host header — 设置 Host 头
    if not req_headers["Host"] and not req_headers["host"] then
        if opts.headers and opts.headers["Host"] then
            req_headers["Host"] = opts.headers["Host"]
        end
    end

    local ok, status_code, response_headers = http_socket.request({
        url = full_url,
        method = method,
        headers = req_headers,
        source = request_body and ltn12.source.string(request_body) or nil,
        sink = ltn12.sink.table(response_body),
        redirect = false,
    })

    if not ok then
        return nil, status_code  -- status_code is error message here
    end

    -- Normalize response headers to lowercase keys — 响应头 key 统一小写
    local norm_headers = {}
    if response_headers then
        for k, v in pairs(response_headers) do
            norm_headers[k:lower()] = v
        end
    end

    return {
        status = status_code,
        headers = norm_headers,
        body = table.concat(response_body),
        -- Helper: read body as string — 辅助方法：读取 body 为字符串
        read_body = function(self)
            return self.body
        end,
    }
end

-- Convenience methods — 便捷方法
function Client:get(path, opts)
    opts = opts or {}
    opts.method = "GET"
    opts.path = path
    return self:send(opts)
end

function Client:post(path, opts)
    opts = opts or {}
    opts.method = "POST"
    opts.path = path
    return self:send(opts)
end

function Client:put(path, opts)
    opts = opts or {}
    opts.method = "PUT"
    opts.path = path
    return self:send(opts)
end

function Client:patch(path, opts)
    opts = opts or {}
    opts.method = "PATCH"
    opts.path = path
    return self:send(opts)
end

function Client:delete(path, opts)
    opts = opts or {}
    opts.method = "DELETE"
    opts.path = path
    return self:send(opts)
end

-- Close client (no-op for luasocket, kept for API compatibility) — 关闭客户端（luasocket 无需操作，保持 API 兼容）
function Client:close()
    -- no-op
end

return _M
```

- [ ] **Step 2: Commit**

```bash
git add spec/fixtures/http_client.lua
git commit -m "feat: 添加基于 luasocket 的 HTTP 客户端封装
add luasocket-based HTTP client wrapper for spec tests"
```

---

### Task 3: spec.helpers 核心兼容层

**Files:**
- Create: `spec/helpers.lua`
- Create: `spec/kong_tests.conf`

这是最核心的文件——提供与 Kong `spec.helpers` API 兼容的接口。

- [ ] **Step 1: 创建测试配置文件**

```
# spec/kong_tests.conf
# Kong-Rust test configuration — Kong-Rust 测试配置

# Database — 数据库
database = postgres
pg_host = 127.0.0.1
pg_port = 5432
pg_user = kong
pg_password = kong
pg_database = kong_tests

# Proxy listen — 代理监听
proxy_listen = 0.0.0.0:9000, 0.0.0.0:9443 ssl
admin_listen = 0.0.0.0:9001

# Logging — 日志
log_level = info
```

- [ ] **Step 2: 创建 spec/helpers.lua 核心兼容层**

```lua
-- spec/helpers.lua
-- Kong spec.helpers compatibility layer for Kong-Rust — Kong-Rust 的 spec.helpers 兼容层
--
-- Provides the same API as Kong's spec/helpers.lua so that official
-- Kong spec files can run against Kong-Rust without modification.

local http_client = require("spec.fixtures.http_client")
local cjson = require("cjson")
local socket = require("socket")

math.randomseed(socket.gettime() * 1000)

local _M = {}

---------------------------------------------------------------------------
-- Configuration — 配置
---------------------------------------------------------------------------

-- Test ports matching Kong's spec conventions — 与 Kong spec 约定一致的测试端口
_M.test_conf = {
    proxy_port       = tonumber(os.getenv("KONG_SPEC_TEST_PROXY_PORT")) or 9000,
    proxy_ssl_port   = tonumber(os.getenv("KONG_SPEC_TEST_PROXY_SSL_PORT")) or 9443,
    admin_port       = tonumber(os.getenv("KONG_SPEC_TEST_ADMIN_PORT")) or 9001,
    proxy_host       = os.getenv("KONG_SPEC_TEST_PROXY_HOST") or "127.0.0.1",
    admin_host       = os.getenv("KONG_SPEC_TEST_ADMIN_HOST") or "127.0.0.1",
    pg_host          = os.getenv("KONG_SPEC_TEST_PG_HOST") or "127.0.0.1",
    pg_port          = tonumber(os.getenv("KONG_SPEC_TEST_PG_PORT")) or 5432,
    pg_user          = os.getenv("KONG_SPEC_TEST_PG_USER") or "kong",
    pg_password      = os.getenv("KONG_SPEC_TEST_PG_PASSWORD") or "kong",
    pg_database      = os.getenv("KONG_SPEC_TEST_PG_DATABASE") or "kong_tests",
    database         = os.getenv("KONG_SPEC_TEST_DATABASE") or "postgres",
}

-- Kong-Rust binary path — Kong-Rust 二进制路径
local KONG_RUST_BIN = os.getenv("KONG_RUST_BIN")
    or "./target/debug/kong"

-- PID file for process management — 进程管理用的 PID 文件
local PID_FILE = os.getenv("KONG_SPEC_PID_FILE")
    or "/tmp/kong-rust-spec.pid"

---------------------------------------------------------------------------
-- Kong lifecycle — Kong 生命周期管理
---------------------------------------------------------------------------

--- Start Kong-Rust instance — 启动 Kong-Rust 实例
-- @param conf table 配置覆盖（可选）
-- @return true on success
function _M.start_kong(conf)
    conf = conf or {}

    -- Build environment — 构建环境变量
    local env = {}
    env.KONG_DATABASE = conf.database or _M.test_conf.database
    env.KONG_PG_HOST = _M.test_conf.pg_host
    env.KONG_PG_PORT = tostring(_M.test_conf.pg_port)
    env.KONG_PG_USER = _M.test_conf.pg_user
    env.KONG_PG_PASSWORD = _M.test_conf.pg_password
    env.KONG_PG_DATABASE = _M.test_conf.pg_database
    env.KONG_PROXY_LISTEN = string.format("0.0.0.0:%d", _M.test_conf.proxy_port)
    env.KONG_ADMIN_LISTEN = string.format("0.0.0.0:%d", _M.test_conf.admin_port)
    env.KONG_LOG_LEVEL = conf.log_level or "warn"

    if conf.plugins then
        env.KONG_PLUGINS = type(conf.plugins) == "table"
            and table.concat(conf.plugins, ",")
            or tostring(conf.plugins)
    end

    -- Build env prefix string — 构建环境变量前缀字符串
    local env_parts = {}
    for k, v in pairs(env) do
        env_parts[#env_parts + 1] = string.format("%s=%s", k, v)
    end
    local env_str = table.concat(env_parts, " ")

    -- Run migrations first — 先运行数据库迁移
    if env.KONG_DATABASE ~= "off" then
        os.execute(string.format("%s %s db bootstrap 2>/dev/null || true",
            env_str, KONG_RUST_BIN))
    end

    -- Start kong-rust, capture PID — 启动 kong-rust，捕获 PID
    local cmd = string.format(
        "%s nohup %s start > /tmp/kong-rust-spec.log 2>&1 & echo $! > %s",
        env_str, KONG_RUST_BIN, PID_FILE)
    os.execute(cmd)

    -- Wait for it to be ready — 等待就绪
    local ok = _M.wait_until(function()
        local client = _M.admin_client()
        if not client then return false end
        local res = client:get("/status")
        return res and res.status == 200
    end, 15)

    if not ok then
        _M.stop_kong()
        error("Kong-Rust failed to start within 15 seconds. Check /tmp/kong-rust-spec.log")
    end

    return true
end

--- Stop Kong-Rust instance — 停止 Kong-Rust 实例
function _M.stop_kong()
    -- Read PID from file — 从 PID 文件读取进程号
    local f = io.open(PID_FILE, "r")
    if f then
        local pid = f:read("*l")
        f:close()
        if pid and pid ~= "" then
            os.execute(string.format("kill -TERM %s 2>/dev/null || true", pid))
            -- Wait for process to exit — 等待进程退出
            _M.wait_until(function()
                local ret = os.execute(string.format("kill -0 %s 2>/dev/null", pid))
                return ret ~= 0 and ret ~= true  -- process no longer exists
            end, 10)
        end
        os.remove(PID_FILE)
    end

    return true
end

---------------------------------------------------------------------------
-- HTTP Clients — HTTP 客户端
---------------------------------------------------------------------------

--- Create a proxy HTTP client — 创建代理 HTTP 客户端
function _M.proxy_client(timeout)
    return http_client.new(
        _M.test_conf.proxy_host,
        _M.test_conf.proxy_port,
        { timeout = timeout or 10 }
    )
end

--- Create a proxy HTTPS client — 创建代理 HTTPS 客户端
function _M.proxy_ssl_client(timeout)
    return http_client.new(
        _M.test_conf.proxy_host,
        _M.test_conf.proxy_ssl_port,
        { timeout = timeout or 10, scheme = "https" }
    )
end

--- Create an admin API client — 创建 Admin API 客户端
function _M.admin_client(timeout)
    return http_client.new(
        _M.test_conf.admin_host,
        _M.test_conf.admin_port,
        { timeout = timeout or 10 }
    )
end

---------------------------------------------------------------------------
-- Database Utilities & Blueprint — 数据库工具和 Blueprint
---------------------------------------------------------------------------

-- Blueprint: creates test fixtures via Admin API — 通过 Admin API 创建测试数据
local Blueprint = {}
Blueprint.__index = Blueprint

function Blueprint:new(admin_client)
    local bp = { admin = admin_client }

    -- Entity name → Admin API endpoint mapping — 实体名 → Admin API 端点映射
    local entity_endpoints = {
        services     = "/services",
        routes       = "/routes",
        consumers    = "/consumers",
        plugins      = "/plugins",
        upstreams    = "/upstreams",
        targets      = "/upstreams/%s/targets",  -- needs upstream id
        certificates = "/certificates",
        snis         = "/snis",
        ca_certificates = "/ca-certificates",
    }

    -- __index: bp.services returns a table with :insert/:truncate methods
    -- 元表魔法：bp.services 返回带 :insert/:truncate 方法的表
    setmetatable(bp, {
        __index = function(_, key)
            local endpoint = entity_endpoints[key]
            if not endpoint then return nil end

            return {
                insert = function(_, data)
                    local actual_endpoint = endpoint
                    if key == "targets" and data and data.upstream then
                        local uid = type(data.upstream) == "table"
                            and (data.upstream.id or data.upstream.name)
                            or data.upstream
                        actual_endpoint = string.format(endpoint, uid)
                        data.upstream = nil
                    end
                    -- Use bp.admin (captured via closure) — 通过闭包捕获 bp.admin
                    local res, err = bp.admin:post(actual_endpoint, {
                        body = data,
                        headers = { ["Content-Type"] = "application/json" },
                    })
                    if not res then
                        error("Failed to create entity at " .. actual_endpoint
                            .. ": " .. tostring(err))
                    end
                    if res.status < 200 or res.status >= 300 then
                        error(string.format(
                            "Failed to create entity at %s: HTTP %d - %s",
                            actual_endpoint, res.status, res.body))
                    end
                    return cjson.decode(res.body)
                end,

                truncate = function(_)
                    local res = bp.admin:get(endpoint)
                    if res and res.status == 200 then
                        local body = cjson.decode(res.body)
                        if body and body.data then
                            for _, entity in ipairs(body.data) do
                                bp.admin:delete(endpoint .. "/" .. entity.id)
                            end
                        end
                    end
                end,
            }
        end,
    })

    return bp
end

--- Get database utilities — 获取数据库工具
-- @param strategy string 数据库策略 ("postgres" or "off")
-- @param tables table 需要的表列表（可选，用于兼容，实际不过滤）
-- @param plugins table 需要的插件列表（可选）
-- @return Blueprint, db_proxy
function _M.get_db_utils(strategy, tables, plugins)
    -- For Kong-Rust, strategy is informational — 对于 Kong-Rust，strategy 只是信息性的
    local admin = _M.admin_client()
    local bp = Blueprint:new(admin)
    return bp, nil  -- second return is db object, not needed for Admin API approach
end

---------------------------------------------------------------------------
-- Assertions — 断言扩展
---------------------------------------------------------------------------

-- Extend busted's assert with Kong-compatible helpers — 扩展 busted 的 assert
local say = require("say")
local luassert = require("luassert")

-- assert.res_status(expected_status, response) — 断言响应状态码
local function res_status(state, arguments)
    local expected = arguments[1]
    local response = arguments[2]
    if not response then
        return false
    end
    return response.status == expected
end

say:set("assertion.res_status.positive", "Expected status %s, got %s")
say:set("assertion.res_status.negative", "Expected status to not be %s")
luassert:register("assertion", "res_status", res_status,
    "assertion.res_status.positive", "assertion.res_status.negative")

-- assert.response(res).has.status(code) — 链式断言
function _M.assert_response(response)
    return {
        has = {
            status = function(expected)
                assert.are.equal(expected, response.status,
                    string.format("Expected status %d, got %d. Body: %s",
                        expected, response.status, response.body or ""))
                return response
            end,
            header = function(name)
                local val = response.headers[name:lower()]
                assert.is_not_nil(val,
                    string.format("Expected header '%s' to be present", name))
                return val
            end,
            no = {
                header = function(name)
                    local val = response.headers[name:lower()]
                    assert.is_nil(val,
                        string.format("Expected header '%s' to not be present", name))
                end,
            },
            jsonbody = function()
                assert.is_not_nil(response.body, "Expected response to have a body")
                local ok, json = pcall(cjson.decode, response.body)
                assert(ok, "Expected response body to be valid JSON, got: "
                    .. tostring(response.body):sub(1, 200))
                return json
            end,
        },
    }
end

---------------------------------------------------------------------------
-- Wait utilities — 等待工具
---------------------------------------------------------------------------

--- Wait until a function returns truthy, or timeout — 等待函数返回 truthy，或超时
-- @param fn function 要轮询的函数
-- @param timeout number 超时秒数（默认 10）
-- @return boolean
function _M.wait_until(fn, timeout)
    timeout = timeout or 10
    local deadline = socket.gettime() + timeout
    while socket.gettime() < deadline do
        local ok, res = pcall(fn)
        if ok and res then
            return true
        end
        -- Sleep 100ms — 休眠 100ms
        socket.sleep(0.1)
    end
    return false
end

--- Sleep for N seconds — 休眠 N 秒
function _M.sleep(seconds)
    socket.sleep(seconds)
end

---------------------------------------------------------------------------
-- Iterators — 迭代器
---------------------------------------------------------------------------

-- strategies to iterate over — 可迭代的数据库策略
local STRATEGIES = { "postgres" }

--- Iterate over database strategies — 迭代数据库策略
-- Kong specs use: for _, strategy in helpers.each_strategy() do ... end
function _M.each_strategy()
    local i = 0
    return function()
        i = i + 1
        if STRATEGIES[i] then
            return i, STRATEGIES[i]
        end
    end
end

--- Iterate over all strategies (alias) — 所有策略迭代（别名）
_M.all_strategies = _M.each_strategy

---------------------------------------------------------------------------
-- Cleanup — 清理
---------------------------------------------------------------------------

--- Clean database — 清空数据库
function _M.clean_db()
    local admin = _M.admin_client()
    -- Delete in dependency order — 按依赖顺序删除
    -- Delete in reverse dependency order — 按反向依赖顺序删除
    local entities = { "plugins", "snis", "routes", "services", "consumers",
                       "targets", "upstreams", "certificates", "ca_certificates" }
    for _, entity in ipairs(entities) do
        local res = admin:get("/" .. entity:gsub("_", "-"))
        if res and res.status == 200 then
            local body = cjson.decode(res.body)
            if body and body.data then
                for _, item in ipairs(body.data) do
                    admin:delete("/" .. entity:gsub("_", "-") .. "/" .. item.id)
                end
            end
        end
    end
end

---------------------------------------------------------------------------
-- Misc utilities — 杂项工具
---------------------------------------------------------------------------

--- Execute a shell command — 执行 shell 命令
function _M.execute(cmd)
    local handle = io.popen(cmd .. " 2>&1")
    local result = handle:read("*a")
    local ok, _, code = handle:close()
    return result, "", code or (ok and 0 or 1)
end

--- Generate a random UUID — 生成随机 UUID
function _M.uuid()
    -- Simple UUID v4 generation — 简单的 UUID v4 生成
    local template = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
    return string.gsub(template, "[xy]", function(c)
        local v = (c == "x") and math.random(0, 0xf) or math.random(8, 0xb)
        return string.format("%x", v)
    end)
end

return _M
```

- [ ] **Step 3: Commit**

```bash
git add spec/helpers.lua spec/kong_tests.conf
git commit -m "feat: 添加 spec.helpers 核心兼容层和测试配置
add spec.helpers core compatibility layer and test configuration"
```

---

### Task 4: Rust spec runner (集成测试入口)

**Files:**
- Create: `crates/kong-server/tests/spec_runner.rs`
- Modify: `scripts/run-cargo-test.sh`（可选）

Rust 侧的 integration test，负责：编译二进制 → 启动 PostgreSQL → 运行 busted。

- [ ] **Step 1: 创建 spec_runner.rs**

```rust
//! Kong spec runner — runs Kong official spec files via busted against Kong-Rust
//! Kong spec 运行器 — 通过 busted 对 Kong-Rust 运行 Kong 官方 spec 文件

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Find workspace root by looking for Cargo.toml with [workspace]
/// 通过查找包含 [workspace] 的 Cargo.toml 来定位 workspace 根目录
fn workspace_root() -> PathBuf {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1"])
        .output()
        .expect("Failed to run cargo metadata");
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Failed to parse cargo metadata");
    PathBuf::from(metadata["workspace_root"].as_str().unwrap())
}

/// Run busted on a spec file or directory — 在 spec 文件或目录上运行 busted
fn run_busted(spec_path: &str) -> bool {
    let root = workspace_root();

    // Get luarocks paths — 获取 luarocks 路径
    let luarocks_path = Command::new("luarocks")
        .args(["path", "--bin"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Parse LUA_PATH and LUA_CPATH from luarocks output — 解析 luarocks 输出的路径
    let mut lua_path = String::new();
    let mut lua_cpath = String::new();
    let mut path_env = env::var("PATH").unwrap_or_default();

    for line in luarocks_path.lines() {
        if line.starts_with("export LUA_PATH=") {
            lua_path = line
                .trim_start_matches("export LUA_PATH='")
                .trim_end_matches('\'')
                .to_string();
        } else if line.starts_with("export LUA_CPATH=") {
            lua_cpath = line
                .trim_start_matches("export LUA_CPATH='")
                .trim_end_matches('\'')
                .to_string();
        } else if line.starts_with("export PATH=") {
            let p = line
                .trim_start_matches("export PATH='")
                .trim_end_matches('\'');
            path_env = format!("{}:{}", p, path_env);
        }
    }

    // Add spec/ to Lua path — 将 spec/ 添加到 Lua 路径
    let spec_lua_path = format!(
        "{root}/spec/?.lua;{root}/spec/?/init.lua;{root}/?.lua;{lua_path}",
        root = root.display(),
        lua_path = lua_path,
    );

    let status = Command::new("busted")
        .args([
            "--helper=spec/helpers.lua",
            "-o", "TAP",
            "--no-auto-insulate",
            spec_path,
        ])
        .current_dir(&root)
        .env("LUA_PATH", &spec_lua_path)
        .env("LUA_CPATH", &lua_cpath)
        .env("PATH", &path_env)
        .env("KONG_RUST_BIN", root.join("target/debug/kong").to_str().unwrap())
        .status()
        .expect("Failed to execute busted — is it installed? Run: make setup-busted");

    status.success()
}

#[test]
#[ignore] // Run with: cargo test --test spec_runner -- --ignored
fn test_run_kong_specs() {
    assert!(
        run_busted("spec/"),
        "Kong spec tests failed — Kong spec 测试失败"
    );
}
```

- [ ] **Step 2: 在 Makefile 添加 spec-rust target**

在 Makefile 的 spec 相关部分追加：

```makefile
spec-rust: build ## 通过 Rust integration test 运行 Kong spec
	@cargo test --test spec_runner -- --ignored --nocapture
```

- [ ] **Step 3: Commit**

```bash
git add crates/kong-server/tests/spec_runner.rs Makefile
git commit -m "feat: 添加 Rust spec runner 集成测试入口
add Rust spec runner integration test entry point"
```

---

### Task 5: 烟雾测试 — 验证框架端到端

**Files:**
- Create: `spec/00-smoke/01-admin_api_spec.lua`

创建一个最简单的 spec 文件，验证整个 busted → helpers → Kong-Rust 链路工作。

- [ ] **Step 1: 创建烟雾测试 spec**

```lua
-- spec/00-smoke/01-admin_api_spec.lua
-- Smoke test: verify busted + spec.helpers + Kong-Rust work end-to-end
-- 烟雾测试：验证 busted + spec.helpers + Kong-Rust 端到端工作

local helpers = require "spec.helpers"
local cjson = require "cjson"

describe("Kong-Rust smoke test", function()
    local admin_client

    lazy_setup(function()
        -- Start Kong-Rust — 启动 Kong-Rust
        assert(helpers.start_kong({
            database = helpers.test_conf.database,
        }))

        admin_client = helpers.admin_client()
    end)

    lazy_teardown(function()
        if admin_client then
            admin_client:close()
        end
        helpers.stop_kong()
    end)

    describe("Admin API", function()
        it("GET / returns node information", function()
            local res = admin_client:get("/")
            assert.are.equal(200, res.status)
            local body = cjson.decode(res.body)
            assert.is_not_nil(body.version)
            assert.is_not_nil(body.node_id)
        end)

        it("GET /status returns status information", function()
            local res = admin_client:get("/status")
            assert.are.equal(200, res.status)
            local body = cjson.decode(res.body)
            assert.is_not_nil(body.database)
        end)

        it("can CRUD a service", function()
            -- Create — 创建
            local res = admin_client:post("/services", {
                body = {
                    name = "smoke-test-service",
                    url = "http://httpbin.org:80",
                },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(201, res.status)
            local service = cjson.decode(res.body)
            assert.are.equal("smoke-test-service", service.name)
            assert.is_not_nil(service.id)

            -- Read — 读取
            res = admin_client:get("/services/" .. service.id)
            assert.are.equal(200, res.status)

            -- Update — 更新
            res = admin_client:patch("/services/" .. service.id, {
                body = { name = "smoke-test-updated" },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(200, res.status)
            local updated = cjson.decode(res.body)
            assert.are.equal("smoke-test-updated", updated.name)

            -- Delete — 删除
            res = admin_client:delete("/services/" .. service.id)
            assert.are.equal(204, res.status)
        end)

        it("can create a route with a service", function()
            -- Create service — 创建 service
            local res = admin_client:post("/services", {
                body = {
                    name = "route-test-service",
                    url = "http://httpbin.org:80",
                },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(201, res.status)
            local service = cjson.decode(res.body)

            -- Create route — 创建 route
            res = admin_client:post("/services/" .. service.id .. "/routes", {
                body = {
                    paths = { "/smoke-test" },
                },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(201, res.status)
            local route = cjson.decode(res.body)
            assert.is_not_nil(route.id)

            -- Cleanup — 清理
            admin_client:delete("/routes/" .. route.id)
            admin_client:delete("/services/" .. service.id)
        end)
    end)

    describe("Blueprint", function()
        it("can create fixtures via Blueprint", function()
            local bp = helpers.get_db_utils("postgres", {
                "services", "routes",
            })

            -- Use Blueprint to create fixtures — 用 Blueprint 创建 fixture
            local service = bp.services:insert({
                name = "bp-test-service",
                url = "http://httpbin.org:80",
            })
            assert.is_not_nil(service.id)

            local route = bp.routes:insert({
                service = { id = service.id },
                paths = { "/bp-test" },
            })
            assert.is_not_nil(route.id)

            -- Cleanup — 清理
            local admin = helpers.admin_client()
            admin:delete("/routes/" .. route.id)
            admin:delete("/services/" .. service.id)
        end)
    end)
end)
```

- [ ] **Step 2: 运行烟雾测试验证整个链路**

先确保 PostgreSQL 运行：
```bash
make services-up
```

然后运行 busted（手动方式，验证 Lua 部分工作）：
```bash
eval "$(luarocks path --bin)"
cargo build
busted --helper=spec/helpers.lua spec/00-smoke/ -o utfTerminal -v
```

Expected: 全部测试通过

- [ ] **Step 3: 修复发现的问题**

根据烟雾测试结果调试和修复 helpers.lua / http_client.lua 中的问题。这一步可能需要多次迭代。

- [ ] **Step 4: Commit**

```bash
git add spec/00-smoke/01-admin_api_spec.lua
git commit -m "test: 添加 Kong spec 烟雾测试验证端到端框架
add Kong spec smoke test to verify end-to-end framework"
```

---

### Task 6: 便捷脚本和 CI 集成

**Files:**
- Create: `scripts/run-specs.sh`
- Modify: `Makefile`

- [ ] **Step 1: 创建运行脚本**

```bash
#!/usr/bin/env bash
# scripts/run-specs.sh
# Run Kong spec tests — 运行 Kong spec 测试
#
# Usage:
#   ./scripts/run-specs.sh                    # run all specs
#   ./scripts/run-specs.sh spec/00-smoke/     # run specific directory
#   ./scripts/run-specs.sh spec/00-smoke/01-admin_api_spec.lua  # run specific file

set -euo pipefail

SPEC_PATH="${1:-spec/}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Ensure busted is available — 确保 busted 可用
eval "$(luarocks path --bin 2>/dev/null)" || true
if ! command -v busted &>/dev/null; then
    echo "错误: busted 未安装。运行: make setup-busted"
    exit 1
fi

# Build kong-rust — 编译 kong-rust
echo "=== 编译 kong-rust ==="
cargo build --quiet

# Set up Lua paths — 设置 Lua 路径
export LUA_PATH="${ROOT}/spec/?.lua;${ROOT}/spec/?/init.lua;${ROOT}/?.lua;$(lua -e 'print(package.path)' 2>/dev/null || echo '')"
export KONG_RUST_BIN="${ROOT}/target/debug/kong"

# Run specs — 运行测试
echo "=== 运行 Kong spec: ${SPEC_PATH} ==="
cd "${ROOT}"
busted \
    --helper=spec/helpers.lua \
    -o utfTerminal \
    --no-auto-insulate \
    -v \
    "${SPEC_PATH}"
```

- [ ] **Step 2: 更新 Makefile spec targets**

```makefile
spec: build ## 运行全部 Kong spec 测试
	@bash scripts/run-specs.sh

spec-file: build ## 运行指定 spec 文件 (SPEC=path/to/spec.lua)
	@bash scripts/run-specs.sh $(SPEC)

spec-smoke: build ## 运行烟雾测试
	@bash scripts/run-specs.sh spec/00-smoke/
```

- [ ] **Step 3: Commit**

```bash
chmod +x scripts/run-specs.sh
git add scripts/run-specs.sh Makefile
git commit -m "chore: 添加 spec 运行脚本和 Makefile 便捷目标
add spec runner script and Makefile convenience targets"
```

---

### Task 7: 更新项目文档

**Files:**
- Modify: `docs/tasks.md`
- Create: `docs/implementation-logs/phase0-step1-busted-compat.md`

- [ ] **Step 1: 更新 tasks.md 标记 8.12a 为进行中**

将 tasks.md 中 8.12a 的 `[ ]` 改为 `[-]`，添加子任务说明。

- [ ] **Step 2: 创建实现日志**

```markdown
# Phase 0 Step 1: busted + spec.helpers 核心兼容层

## 实现概要

搭建了 Kong 官方 spec 测试兼容框架，使 Kong 官方 spec 文件可以直接在 Kong-Rust 上运行。

## 架构

- 进程级集成测试：Kong-Rust 作为子进程启动，busted CLI 执行 spec 文件
- spec/helpers.lua：兼容层，提供 start_kong/stop_kong/proxy_client/admin_client/Blueprint 等 API
- HTTP 客户端：luasocket（替代 resty.http，不依赖 openresty）
- Fixture 创建：通过 Admin API（不直连数据库）

## 修改文件

| 操作 | 文件 |
|------|------|
| 新增 | `scripts/setup-busted.sh` |
| 新增 | `spec/helpers.lua` |
| 新增 | `spec/kong_tests.conf` |
| 新增 | `spec/fixtures/http_client.lua` |
| 新增 | `spec/00-smoke/01-admin_api_spec.lua` |
| 新增 | `scripts/run-specs.sh` |
| 新增 | `crates/kong-server/tests/spec_runner.rs` |
| 修改 | `Makefile` |

## 代码统计

- 新增文件：7
- 新增代码：~600 行 Lua + ~80 行 Rust + ~40 行 Shell
```

- [ ] **Step 3: Commit**

```bash
git add docs/tasks.md docs/implementation-logs/phase0-step1-busted-compat.md
git commit -m "docs: 记录 Phase 0 Step 1 busted 兼容层实现
document Phase 0 Step 1 busted compatibility layer implementation"
```

---

## 后续步骤（Phase 0 Step 2-5，本计划不覆盖）

完成 Step 1 后，后续工作：

1. **Step 2**: 从 Kong 官方仓库复制 `spec/02-integration/04-admin_api/` 测试文件，运行并修复失败
2. **Step 3**: 复制 `spec/03-plugins/` 核心插件测试（key-auth, rate-limiting, cors），运行并修复
3. **Step 4**: 根据 spec 失败修复 Kong-Rust 兼容性 bug
4. **Step 5**: 扩展到更多插件
