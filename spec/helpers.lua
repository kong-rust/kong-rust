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

-- 自动检测 docker 容器端口映射 — auto-detect docker container port mapping
local function detect_pg_port()
    local env_port = os.getenv("KONG_SPEC_TEST_PG_PORT")
    if env_port then return tonumber(env_port) end

    -- 尝试从 docker 获取实际映射端口
    local handle = io.popen("docker port kong-rust-dev-postgres-1 5432 2>/dev/null | cut -d: -f2")
    if handle then
        local result = handle:read("*l")
        handle:close()
        if result and result ~= "" then
            local port = tonumber(result)
            if port then return port end
        end
    end

    return 5432
end

_M.test_conf = {
    proxy_port       = tonumber(os.getenv("KONG_SPEC_TEST_PROXY_PORT")) or 9000,
    proxy_ssl_port   = tonumber(os.getenv("KONG_SPEC_TEST_PROXY_SSL_PORT")) or 9443,
    admin_port       = tonumber(os.getenv("KONG_SPEC_TEST_ADMIN_PORT")) or 9001,
    proxy_host       = os.getenv("KONG_SPEC_TEST_PROXY_HOST") or "127.0.0.1",
    admin_host       = os.getenv("KONG_SPEC_TEST_ADMIN_HOST") or "127.0.0.1",
    pg_host          = os.getenv("KONG_SPEC_TEST_PG_HOST") or "127.0.0.1",
    pg_port          = detect_pg_port(),
    pg_user          = os.getenv("KONG_SPEC_TEST_PG_USER") or "kong",
    pg_password      = os.getenv("KONG_SPEC_TEST_PG_PASSWORD") or "kong",
    pg_database      = os.getenv("KONG_SPEC_TEST_PG_DATABASE") or "kong_tests",
    database         = os.getenv("KONG_SPEC_TEST_DATABASE") or "postgres",
}

local KONG_RUST_BIN = os.getenv("KONG_RUST_BIN") or "./target/debug/kong"

local PID_FILE = os.getenv("KONG_SPEC_PID_FILE") or "/tmp/kong-rust-spec.pid"
local MOCK_UPSTREAM_PID_FILE = "/tmp/kong-rust-mock-upstream.pid"

---------------------------------------------------------------------------
-- Mock upstream constants — Mock 上游服务常量
---------------------------------------------------------------------------

_M.mock_upstream_protocol = "http"
_M.mock_upstream_host = "127.0.0.1"
_M.mock_upstream_hostname = "localhost"
_M.mock_upstream_port = tonumber(os.getenv("KONG_SPEC_MOCK_UPSTREAM_PORT")) or 15555
_M.mock_upstream_ssl_port = tonumber(os.getenv("KONG_SPEC_MOCK_UPSTREAM_SSL_PORT")) or 15556
_M.mock_upstream_url = string.format("http://127.0.0.1:%d", _M.mock_upstream_port)
_M.mock_upstream_ssl_url = string.format("https://127.0.0.1:%d", _M.mock_upstream_ssl_port)
_M.mock_upstream_stream_port = 15557
_M.mock_upstream_stream_ssl_port = 15558

---------------------------------------------------------------------------
-- Mock upstream lifecycle — Mock upstream 生命周期管理
---------------------------------------------------------------------------

function _M.start_mock_upstream()
    -- Check if already running — 检查是否已运行
    local f = io.open(MOCK_UPSTREAM_PID_FILE, "r")
    if f then
        local pid = f:read("*l")
        f:close()
        if pid and pid ~= "" then
            local ret = os.execute(string.format("kill -0 %s 2>/dev/null", pid))
            if ret == 0 or ret == true then
                return true  -- already running — 已在运行
            end
        end
    end

    local cmd = string.format(
        "nohup %s mock-upstream --port %d > /tmp/kong-rust-mock-upstream.log 2>&1 & echo $! > %s",
        KONG_RUST_BIN, _M.mock_upstream_port, MOCK_UPSTREAM_PID_FILE)
    os.execute(cmd)

    -- Wait for mock upstream to be ready — 等待 mock upstream 就绪
    local ok = _M.wait_until(function()
        local client = http_client.new("127.0.0.1", _M.mock_upstream_port, { timeout = 2 })
        if not client then return false end
        local res = client:get("/")
        return res and res.status == 200
    end, 10)

    if not ok then
        _M.stop_mock_upstream()
        error("Mock upstream failed to start. Check /tmp/kong-rust-mock-upstream.log")
    end

    return true
end

function _M.stop_mock_upstream()
    local f = io.open(MOCK_UPSTREAM_PID_FILE, "r")
    if f then
        local pid = f:read("*l")
        f:close()
        if pid and pid ~= "" then
            os.execute(string.format("kill -TERM %s 2>/dev/null || true", pid))
            _M.wait_until(function()
                local ret = os.execute(string.format("kill -0 %s 2>/dev/null", pid))
                return ret ~= 0 and ret ~= true
            end, 5)
        end
        os.remove(MOCK_UPSTREAM_PID_FILE)
    end
    return true
end

---------------------------------------------------------------------------
-- Kong lifecycle — Kong 生命周期管理
---------------------------------------------------------------------------

function _M.start_kong(conf)
    conf = conf or {}

    -- Auto-start mock upstream — 自动启动 mock upstream
    _M.start_mock_upstream()

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

    -- Auto-stop mock upstream — 自动停止 mock upstream
    _M.stop_mock_upstream()

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
-- Aligned with Kong spec/internal/asserts.lua — 对齐 Kong 原版断言系统
---------------------------------------------------------------------------

local say = require("say")
local luassert = require("luassert.assert")

-- Case-insensitive key lookup in a table — 大小写不敏感的键查找
local function lookup(t, k)
    local ok = k
    if type(k) ~= "string" then
        return t[k], k
    else
        k = k:lower()
    end
    for key, value in pairs(t) do
        if tostring(key):lower() == k then
            return value, key
        end
    end
    return nil, ok
end

-- Unindent helper — 去缩进辅助函数
local function unindent(str, concat_newlines, spaced_newlines)
    str = string.match(str, "(.-%S*)%s*$")
    if not str then
        return ""
    end

    local level  = math.huge
    local prefix = ""
    local len

    str = str:match("^%s") and "\n" .. str or str
    for pref in str:gmatch("\n(%s+)") do
        len = #prefix
        if len < level then
            level  = len
            prefix = pref
        end
    end

    local repl = concat_newlines and "" or "\n"
    repl = spaced_newlines and " " or repl

    return (str:gsub("^\n%s*", ""):gsub("\n" .. prefix, repl):gsub("\n$", ""):gsub("\\r", "\r"))
end

_M.unindent = unindent
_M.lookup = lookup

---------------------------------------------------------------------------
-- Modifier: response — 响应修饰符
-- Sets "kong_response" in assertion state for chained assertions
-- Usage: assert.response(res).has.status(200)
---------------------------------------------------------------------------
local function modifier_response(state, arguments, level)
    assert(arguments.n > 0,
        "response modifier requires a response object as argument")

    local res = arguments[1]

    assert(type(res) == "table" and type(res.read_body) == "function",
        "response modifier requires a response object as argument, got: " .. tostring(res))

    rawset(state, "kong_response", res)
    rawset(state, "kong_request", nil)

    return state
end
luassert:register("modifier", "response", modifier_response)

---------------------------------------------------------------------------
-- Modifier: request — 请求修饰符
-- Decodes mock_upstream echo body and sets "kong_request" in assertion state
-- Usage: assert.request(res).has.header("X-Forwarded-For")
---------------------------------------------------------------------------
local function modifier_request(state, arguments, level)
    local generic = "The assertion 'request' modifier takes a http response"
        .. " object as input to decode the json-body returned by"
        .. " mock_upstream, to retrieve the proxied request."

    local res = arguments[1]

    assert(type(res) == "table" and type(res.read_body) == "function",
        "Expected a http response object, got '" .. tostring(res) .. "'. " .. generic)

    local body, request, err
    body = assert(res:read_body())
    request, err = cjson.decode(body)

    assert(request, "Expected the http response object to have a json encoded body,"
        .. " but decoding gave error '" .. tostring(err) .. "'. Obtained body: "
        .. body .. "\n." .. generic)

    if lookup((res.headers or {}), "X-Powered-By") ~= "mock_upstream" then
        error("Could not determine the response to be from mock_upstream")
    end

    rawset(state, "kong_request", request)
    rawset(state, "kong_response", nil)

    return state
end
luassert:register("modifier", "request", modifier_request)

---------------------------------------------------------------------------
-- Modifier: certificate — 证书修饰符
-- Usage: assert.certificate(cert).has.cn("ssl-example.com")
---------------------------------------------------------------------------
local function modifier_certificate(state, arguments, level)
    local cert = arguments[1]
    assert(type(cert) == "string",
        "Expected a certificate text, got '" .. tostring(cert) .. "'.")
    rawset(state, "kong_certificate", cert)
    return state
end
luassert:register("modifier", "certificate", modifier_certificate)

---------------------------------------------------------------------------
-- Modifier: logfile/errlog — 日志文件修饰符
-- Usage: assert.logfile("/path/to/log").has.no.line("[error]", true)
---------------------------------------------------------------------------
local function modifier_errlog(state, args)
    local errlog_path = args[1] or "/tmp/kong-rust-spec.log"
    assert(type(errlog_path) == "string", "logfile modifier expects nil, or "
        .. "a string as argument, got: " .. type(errlog_path))
    rawset(state, "errlog_path", errlog_path)
    return state
end
luassert:register("modifier", "errlog", modifier_errlog)
luassert:register("modifier", "logfile", modifier_errlog)

---------------------------------------------------------------------------
-- Assertion: status / res_status — 状态码断言
-- Usage: assert.response(res).has.status(200)
--        assert.has.status(200, res)
---------------------------------------------------------------------------
local function res_status(state, args)
    assert(not rawget(state, "kong_request"),
        "Cannot check statuscode against a request object,"
        .. " only against a response object")

    local expected = args[1]
    local res = args[2] or rawget(state, "kong_response")

    assert(type(expected) == "number",
        "Expected response code must be a number value. Got: " .. tostring(expected))
    assert(type(res) == "table" and type(res.read_body) == "function",
        "Expected a http_client response. Got: " .. tostring(res))

    if expected ~= res.status then
        local body = res:read_body() or ""
        local stripped = body:match("^%s*(.-)%s*$") or body
        table.insert(args, 1, stripped)
        table.insert(args, 1, res.status)
        table.insert(args, 1, expected)
        args.n = 3
        return false
    else
        local body = res:read_body() or ""
        local stripped = body:match("^%s*(.-)%s*$") or body
        table.insert(args, 1, stripped)
        table.insert(args, 1, res.status)
        table.insert(args, 1, expected)
        args.n = 3
        return true, { stripped }
    end
end
say:set("assertion.res_status.negative", [[
Invalid response status code.
Status expected:
%s
Status received:
%s
Body:
%s
%s]])
say:set("assertion.res_status.positive", [[
Invalid response status code.
Status not expected:
%s
Status received:
%s
Body:
%s
%s]])
luassert:register("assertion", "status", res_status,
    "assertion.res_status.negative", "assertion.res_status.positive")
luassert:register("assertion", "res_status", res_status,
    "assertion.res_status.negative", "assertion.res_status.positive")

---------------------------------------------------------------------------
-- Assertion: jsonbody — JSON body 断言
-- Usage: assert.response(res).has.jsonbody()
---------------------------------------------------------------------------
local function jsonbody(state, args)
    assert(args[1] == nil and rawget(state, "kong_request") or rawget(state, "kong_response"),
        "the `jsonbody` assertion does not take parameters. " ..
        "Use the `response`/`request` modifiers to set the target to operate on")

    if rawget(state, "kong_response") then
        local body = rawget(state, "kong_response"):read_body()
        local json, err = cjson.decode(body)
        if not json then
            table.insert(args, 1, "Error decoding: " .. tostring(err) .. "\nResponse body:" .. body)
            args.n = 1
            return false
        end
        return true, {json}

    else
        local r = rawget(state, "kong_request")
        if r.post_data
            and (r.post_data.kind == "json" or r.post_data.kind == "json (error)")
            and r.post_data.params
        then
            local pd = r.post_data
            return true, { { params = pd.params, data = pd.text, error = pd.error, kind = pd.kind } }
        else
            error("No json data found in the request")
        end
    end
end
say:set("assertion.jsonbody.negative", [[
Expected response body to contain valid json. Got:
%s
]])
say:set("assertion.jsonbody.positive", [[
Expected response body to not contain valid json. Got:
%s
]])
luassert:register("assertion", "jsonbody", jsonbody,
    "assertion.jsonbody.negative",
    "assertion.jsonbody.positive")

---------------------------------------------------------------------------
-- Assertion: header — Header 断言
-- Usage: assert.response(res).has.header("Content-Type")
--        assert.request(res).has.header("X-Forwarded-For")
---------------------------------------------------------------------------
local function res_header(state, args)
    local header = args[1]
    local res = args[2] or rawget(state, "kong_request") or rawget(state, "kong_response")
    assert(type(res) == "table" and type(res.headers) == "table",
        "'header' assertion input does not contain a 'headers' subtable")
    local value = lookup(res.headers, header)
    table.insert(args, 1, res.headers)
    table.insert(args, 1, header)
    args.n = 2
    if not value then
        return false
    end
    return true, {value}
end
say:set("assertion.res_header.negative", [[
Expected header:
%s
But it was not found in:
%s
]])
say:set("assertion.res_header.positive", [[
Did not expected header:
%s
But it was found in:
%s
]])
luassert:register("assertion", "header", res_header,
    "assertion.res_header.negative",
    "assertion.res_header.positive")

---------------------------------------------------------------------------
-- Assertion: queryparam — Query 参数断言
-- Usage: assert.request(res).has.queryparam("foo")
---------------------------------------------------------------------------
local function req_query_param(state, args)
    local param = args[1]
    local req = rawget(state, "kong_request")
    assert(req, "'queryparam' assertion only works with a request object")
    local params
    if type(req.uri_args) == "table" then
        params = req.uri_args
    else
        error("No query parameters found in request object")
    end
    local value = lookup(params, param)
    table.insert(args, 1, params)
    table.insert(args, 1, param)
    args.n = 2
    if not value then
        return false
    end
    return true, {value}
end
say:set("assertion.req_query_param.negative", [[
Expected query parameter:
%s
But it was not found in:
%s
]])
say:set("assertion.req_query_param.positive", [[
Did not expected query parameter:
%s
But it was found in:
%s
]])
luassert:register("assertion", "queryparam", req_query_param,
    "assertion.req_query_param.negative",
    "assertion.req_query_param.positive")

---------------------------------------------------------------------------
-- Assertion: formparam — 表单参数断言
-- Usage: assert.request(res).has.formparam("bar")
---------------------------------------------------------------------------
local function req_form_param(state, args)
    local param = args[1]
    local req = rawget(state, "kong_request")
    assert(req, "'formparam' assertion can only be used with a mock_upstream request object")

    local value
    if req.post_data
        and (req.post_data.kind == "form" or req.post_data.kind == "multipart-form")
    then
        value = lookup(req.post_data.params or {}, param)
    else
        error("Could not determine the request to be from either mock_upstream")
    end

    table.insert(args, 1, req)
    table.insert(args, 1, param)
    args.n = 2
    if not value then
        return false
    end
    return true, {value}
end
say:set("assertion.req_form_param.negative", [[
Expected url encoded form parameter:
%s
But it was not found in request:
%s
]])
say:set("assertion.req_form_param.positive", [[
Did not expected url encoded form parameter:
%s
But it was found in request:
%s
]])
luassert:register("assertion", "formparam", req_form_param,
    "assertion.req_form_param.negative",
    "assertion.req_form_param.positive")

---------------------------------------------------------------------------
-- Assertion: cn — 证书 CN 断言
-- Usage: assert.cn("ssl-example.com", cert)
--        assert.certificate(cert).has.cn("ssl-example.com")
---------------------------------------------------------------------------
local function assert_cn(state, args)
    local expected = args[1]
    if args[2] and rawget(state, "kong_certificate") then
        error("assertion 'cn' takes either a 'certificate' modifier, or 2 parameters, not both")
    end
    local cert = args[2] or rawget(state, "kong_certificate")
    local cn = string.match(cert, "CN%s*=%s*([^%s,]+)")
    args[2] = cn or "(CN not found in certificate)"
    args.n = 2
    return cn == expected
end
say:set("assertion.cn.negative", [[
Expected certificate to have the given CN value.
Expected CN:
%s
Got instead:
%s
]])
say:set("assertion.cn.positive", [[
Expected certificate to not have the given CN value.
Expected CN to not be:
%s
Got instead:
%s
]])
luassert:register("assertion", "cn", assert_cn,
    "assertion.cn.negative",
    "assertion.cn.positive")

---------------------------------------------------------------------------
-- Assertion: contains — 数组包含断言
-- Usage: assert.contains("one", arr)
--        assert.contains("ee$", arr, true)  -- pattern matching
---------------------------------------------------------------------------
local function contains(state, args)
    local expected = args[1]
    local arr = args[2]
    local pattern = args[3]
    local found
    for i = 1, #arr do
        if (pattern and string.match(arr[i], expected)) or arr[i] == expected then
            found = i
            break
        end
    end
    return found ~= nil, {found}
end
say:set("assertion.contains.negative", [[
Expected array to contain element.
Expected to contain:
%s
]])
say:set("assertion.contains.positive", [[
Expected array to not contain element.
Expected to not contain:
%s
]])
luassert:register("assertion", "contains", contains,
    "assertion.contains.negative",
    "assertion.contains.positive")

---------------------------------------------------------------------------
-- Assertion: gt — 大于断言
-- Usage: assert.gt(base, value)
---------------------------------------------------------------------------
local function is_gt(state, arguments)
    local expected = arguments[1]
    local value = arguments[2]
    arguments[1] = value
    arguments[2] = expected
    return value > expected
end
say:set("assertion.gt.negative", [[
Given value (%s) should be greater than expected value (%s)
]])
say:set("assertion.gt.positive", [[
Given value (%s) should not be greater than expected value (%s)
]])
luassert:register("assertion", "gt", is_gt,
    "assertion.gt.negative",
    "assertion.gt.positive")

-- Matcher: gt — 大于匹配器
local function is_gt_matcher(state, arguments)
    local expected = arguments[1]
    return function(value)
        return value > expected
    end
end
luassert:register("matcher", "gt", is_gt_matcher)

---------------------------------------------------------------------------
-- Assertion: fail — 通用失败断言（调试用）
-- Usage: assert.fail(some, value)
---------------------------------------------------------------------------
local function fail(state, args)
    local out = {}
    for k,v in pairs(args) do out[k] = v end
    args[1] = out
    args.n = 1
    return false
end
say:set("assertion.fail.negative", [[
Fail assertion was called with the following parameters (formatted as a table);
%s
]])
luassert:register("assertion", "fail", fail,
    "assertion.fail.negative",
    "assertion.fail.negative")

---------------------------------------------------------------------------
-- Assertion: partial_match — 部分表匹配断言
-- Usage: assert.partial_match(subset_table, full_table)
---------------------------------------------------------------------------
local function partial_match(state, arguments)
    local function deep_matches(t1, t2, parent_keys)
        for key, v in pairs(t1) do
            local compound_key = (parent_keys and parent_keys .. "." .. key) or key
            if type(v) == "table" then
                local ok, ck, v1, v2 = deep_matches(t1[key], t2[key], compound_key)
                if not ok then
                    return ok, ck, v1, v2
                end
            else
                if (state.mod == true and t1[key] ~= t2[key]) or (state.mod == false and t1[key] == t2[key]) then
                    return false, compound_key, t1[key], t2[key]
                end
            end
        end
        return true
    end

    local partial_table = arguments[1]
    local full_table = arguments[2]
    local ok, compound_key, v1, v2 = deep_matches(partial_table, full_table)

    if not ok then
        arguments[1] = compound_key
        arguments[2] = v1
        arguments[3] = v2
        arguments.n = 3
        return not state.mod
    end

    return state.mod
end
say:set("assertion.partial_match.negative", [[
Values at key %s should not be equal
]])
say:set("assertion.partial_match.positive", [[
Values at key %s should be equal but are not.
Expected: %s, given: %s
]])
luassert:register("assertion", "partial_match", partial_match,
    "assertion.partial_match.positive",
    "assertion.partial_match.negative")

---------------------------------------------------------------------------
-- Assertion: line — 日志行匹配断言（使用 Lua 模式匹配代替 ngx.re）
-- Usage: assert.logfile().has.no.line("[error]", true)
---------------------------------------------------------------------------
do
    local function substr(subject, pattern)
        if subject:find(pattern, nil, true) ~= nil then
            return subject
        end
    end

    local function lua_match(subject, pattern)
        if subject:match(pattern) then
            return subject
        end
    end

    local function find_in_file(fpath, pattern, matcher)
        local fh = io.open(fpath, "r")
        if not fh then return nil end
        local found

        for line in fh:lines() do
            if matcher(line, pattern) then
                found = line
                break
            end
        end

        fh:close()
        return found
    end

    local function match_line(state, args)
        local regex = args[1]
        local plain = args[2]
        local timeout = args[3] or 2
        local fpath = args[4] or rawget(state, "errlog_path")

        assert(type(regex) == "string",
            "Expected the regex argument to be a string")
        assert(type(fpath) == "string",
            "Expected the file path argument to be a string")
        assert(type(timeout) == "number" and timeout >= 0,
            "Expected the timeout argument to be a number >= 0")

        -- Use plain string find or Lua pattern match — 使用纯字符串查找或 Lua 模式匹配
        local matcher = plain and substr or lua_match

        local found = find_in_file(fpath, regex, matcher)
        local deadline = socket.gettime() + timeout

        while not found and socket.gettime() <= deadline do
            socket.sleep(0.05)
            found = find_in_file(fpath, regex, matcher)
        end

        args[1] = fpath
        args[2] = regex
        args.n = 2

        if found then
            args[3] = found
            args.n = 3
        end

        return found
    end

    say:set("assertion.match_line.negative", unindent [[
        Expected file at:
        %s
        To match:
        %s
    ]])
    say:set("assertion.match_line.positive", unindent [[
        Expected file at:
        %s
        To not match:
        %s
        But matched line:
        %s
    ]])
    luassert:register("assertion", "line", match_line,
        "assertion.match_line.negative",
        "assertion.match_line.positive")
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
