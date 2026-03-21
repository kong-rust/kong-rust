-- spec/helpers.lua
-- Kong spec.helpers compatibility layer for Kong-Rust — Kong-Rust 的 spec.helpers 兼容层

local http_client = require("spec.fixtures.http_client")
local cjson = require("cjson")
local socket = require("socket")

math.randomseed(socket.gettime() * 1000)

local _M = {}

---------------------------------------------------------------------------
-- Configuration — 配置
---------------------------------------------------------------------------

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

local KONG_RUST_BIN = os.getenv("KONG_RUST_BIN") or "./target/debug/kong"

local PID_FILE = os.getenv("KONG_SPEC_PID_FILE") or "/tmp/kong-rust-spec.pid"

---------------------------------------------------------------------------
-- Kong lifecycle — Kong 生命周期管理
---------------------------------------------------------------------------

function _M.start_kong(conf)
    conf = conf or {}

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

    local env_parts = {}
    for k, v in pairs(env) do
        env_parts[#env_parts + 1] = string.format("%s=%s", k, v)
    end
    local env_str = table.concat(env_parts, " ")

    if env.KONG_DATABASE ~= "off" then
        os.execute(string.format("%s %s db bootstrap 2>/dev/null || true",
            env_str, KONG_RUST_BIN))
    end

    local cmd = string.format(
        "%s nohup %s start > /tmp/kong-rust-spec.log 2>&1 & echo $! > %s",
        env_str, KONG_RUST_BIN, PID_FILE)
    os.execute(cmd)

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

function _M.stop_kong()
    local f = io.open(PID_FILE, "r")
    if f then
        local pid = f:read("*l")
        f:close()
        if pid and pid ~= "" then
            os.execute(string.format("kill -TERM %s 2>/dev/null || true", pid))
            _M.wait_until(function()
                local ret = os.execute(string.format("kill -0 %s 2>/dev/null", pid))
                return ret ~= 0 and ret ~= true
            end, 10)
        end
        os.remove(PID_FILE)
    end

    return true
end

---------------------------------------------------------------------------
-- HTTP Clients — HTTP 客户端
---------------------------------------------------------------------------

function _M.proxy_client(timeout)
    return http_client.new(
        _M.test_conf.proxy_host,
        _M.test_conf.proxy_port,
        { timeout = timeout or 10 }
    )
end

function _M.proxy_ssl_client(timeout)
    return http_client.new(
        _M.test_conf.proxy_host,
        _M.test_conf.proxy_ssl_port,
        { timeout = timeout or 10, scheme = "https" }
    )
end

function _M.admin_client(timeout)
    return http_client.new(
        _M.test_conf.admin_host,
        _M.test_conf.admin_port,
        { timeout = timeout or 10 }
    )
end

---------------------------------------------------------------------------
-- Blueprint — 通过 Admin API 创建测试数据
---------------------------------------------------------------------------

local Blueprint = {}

function Blueprint:new(admin_client)
    local bp = { admin = admin_client }

    local entity_endpoints = {
        services     = "/services",
        routes       = "/routes",
        consumers    = "/consumers",
        plugins      = "/plugins",
        upstreams    = "/upstreams",
        targets      = "/upstreams/%s/targets",
        certificates = "/certificates",
        snis         = "/snis",
        ca_certificates = "/ca-certificates",
    }

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

function _M.get_db_utils(strategy, tables, plugins)
    local admin = _M.admin_client()
    local bp = Blueprint:new(admin)
    return bp, nil
end

---------------------------------------------------------------------------
-- Assertions — 断言扩展
---------------------------------------------------------------------------

local say = require("say")
local luassert = require("luassert")

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

function _M.wait_until(fn, timeout)
    timeout = timeout or 10
    local deadline = socket.gettime() + timeout
    while socket.gettime() < deadline do
        local ok, res = pcall(fn)
        if ok and res then
            return true
        end
        socket.sleep(0.1)
    end
    return false
end

function _M.sleep(seconds)
    socket.sleep(seconds)
end

---------------------------------------------------------------------------
-- Iterators — 迭代器
---------------------------------------------------------------------------

local STRATEGIES = { "postgres" }

function _M.each_strategy()
    local i = 0
    return function()
        i = i + 1
        if STRATEGIES[i] then
            return i, STRATEGIES[i]
        end
    end
end

_M.all_strategies = _M.each_strategy

---------------------------------------------------------------------------
-- Cleanup — 清理
---------------------------------------------------------------------------

function _M.clean_db()
    local admin = _M.admin_client()
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

function _M.execute(cmd)
    local handle = io.popen(cmd .. " 2>&1")
    local result = handle:read("*a")
    local ok, _, code = handle:close()
    return result, "", code or (ok and 0 or 1)
end

function _M.uuid()
    local template = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
    return string.gsub(template, "[xy]", function(c)
        local v = (c == "x") and math.random(0, 0xf) or math.random(8, 0xb)
        return string.format("%x", v)
    end)
end

return _M
