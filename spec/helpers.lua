-- spec/helpers.lua
-- Kong spec.helpers compatibility layer for Kong-Rust — Kong-Rust 的 spec.helpers 兼容层

local http_client = require("spec.fixtures.http_client")
local cjson = require("cjson")
local socket = require("socket")
local url_mod = require("socket.url")

math.randomseed(socket.gettime() * 1000)

---------------------------------------------------------------------------
-- ngx shim — ngx 全局变量兼容层
---------------------------------------------------------------------------
if not ngx then
    ngx = {
        null = cjson.null,
        escape_uri = function(str)
            return url_mod.escape(str)
        end,
        unescape_uri = function(str)
            return url_mod.unescape(str)
        end,
        re = {
            match = function(subject, regex)
                return string.match(subject, regex)
            end,
            find = function(subject, regex)
                return string.find(subject, regex)
            end,
        },
        log = function() end,
        NOTICE = 5,
        WARN = 4,
        ERR = 3,
        DEBUG = 8,
        INFO = 7,
        OK = 0,
        ERROR = -1,
        HTTP_OK = 200,
        HTTP_CREATED = 201,
        HTTP_NO_CONTENT = 204,
        HTTP_NOT_FOUND = 404,
        HTTP_BAD_REQUEST = 400,
        HTTP_INTERNAL_SERVER_ERROR = 500,
        config = {
            subsystem = function() return "http" end,
        },
        shared = {},
        now = function() return socket.gettime() end,
        time = function() return math.floor(socket.gettime()) end,
        update_time = function() end,
        sleep = function(seconds) socket.sleep(seconds) end,
        say = function() end,
        print = function() end,
        exit = function() end,
        var = {},
        ctx = {},
        header = {},
        req = {
            get_headers = function() return {} end,
            read_body = function() end,
            get_body_data = function() return "" end,
        },
        resp = {
            get_headers = function() return {} end,
        },
        timer = {
            at = function(delay, fn) return true end,
            every = function(delay, fn) return true end,
        },
        worker = {
            id = function() return 0 end,
            count = function() return 1 end,
            exiting = function() return false end,
        },
        encode_base64 = function(str)
            -- simple base64 encoding — 简单 base64 编码
            local b = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
            return ((str:gsub('.', function(x)
                local r, b_val = '', x:byte()
                for i = 8, 1, -1 do r = r .. (b_val % 2^i - b_val % 2^(i-1) > 0 and '1' or '0') end
                return r
            end) .. '0000'):gsub('%d%d%d?%d?%d?%d?', function(x)
                if (#x < 6) then return '' end
                local c = 0
                for i = 1, 6 do c = c + (x:sub(i, i) == '1' and 2^(6-i) or 0) end
                return b:sub(c + 1, c + 1)
            end) .. ({ '', '==', '=' })[#str % 3 + 1])
        end,
        decode_base64 = function(str)
            local b = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
            str = str:gsub('[^' .. b .. '=]', '')
            return (str:gsub('.', function(x)
                if x == '=' then return '' end
                local r, f = '', (b:find(x) - 1)
                for i = 6, 1, -1 do r = r .. (f % 2^i - f % 2^(i-1) > 0 and '1' or '0') end
                return r
            end):gsub('%d%d%d?%d?%d?%d?%d?%d?', function(x)
                if #x ~= 8 then return '' end
                local c = 0
                for i = 1, 8 do c = c + (x:sub(i, i) == '1' and 2^(8-i) or 0) end
                return string.char(c)
            end))
        end,
    }
end

local _M = {}

---------------------------------------------------------------------------
-- Penlight modules — Penlight 工具库
---------------------------------------------------------------------------

local pl_path = require("pl.path")
local pl_dir = require("pl.dir")
local pl_file = require("pl.file")
local pl_utils = require("pl.utils")

_M.dir = pl_dir
_M.path = pl_path
_M.file = pl_file
_M.utils = pl_utils

---------------------------------------------------------------------------
-- Configuration — 配置
---------------------------------------------------------------------------

-- auto-detect docker container port mapping — 自动检测 docker 容器端口映射
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
    -- listen addresses — 监听地址
    proxy_listen     = "0.0.0.0:9000, 0.0.0.0:9443 ssl",
    admin_listen     = "127.0.0.1:9001",
    -- ports — 端口
    proxy_port       = tonumber(os.getenv("KONG_SPEC_TEST_PROXY_PORT")) or 9000,
    proxy_ssl_port   = tonumber(os.getenv("KONG_SPEC_TEST_PROXY_SSL_PORT")) or 9443,
    admin_port       = tonumber(os.getenv("KONG_SPEC_TEST_ADMIN_PORT")) or 9001,
    admin_ssl_port   = tonumber(os.getenv("KONG_SPEC_TEST_ADMIN_SSL_PORT")) or 9444,
    status_port      = tonumber(os.getenv("KONG_SPEC_TEST_STATUS_PORT")) or 8007,
    -- hosts — 主机
    proxy_host       = os.getenv("KONG_SPEC_TEST_PROXY_HOST") or "127.0.0.1",
    admin_host       = os.getenv("KONG_SPEC_TEST_ADMIN_HOST") or "127.0.0.1",
    -- database — 数据库
    pg_host          = os.getenv("KONG_SPEC_TEST_PG_HOST") or "127.0.0.1",
    pg_port          = detect_pg_port(),
    pg_user          = os.getenv("KONG_SPEC_TEST_PG_USER") or "kong",
    pg_password      = os.getenv("KONG_SPEC_TEST_PG_PASSWORD") or "kong",
    pg_database      = os.getenv("KONG_SPEC_TEST_PG_DATABASE") or "kong_tests",
    database         = os.getenv("KONG_SPEC_TEST_DATABASE") or "postgres",
    -- paths — 路径
    prefix           = os.getenv("KONG_SPEC_TEST_PREFIX") or "servroot",
    -- ssl certificates — SSL 证书
    ssl_cert         = "spec/fixtures/kong_spec.crt",
    ssl_cert_key     = "spec/fixtures/kong_spec.key",
    admin_ssl_cert   = "spec/fixtures/kong_spec.crt",
    admin_ssl_cert_key = "spec/fixtures/kong_spec.key",
    -- worker config — worker 配置
    nginx_worker_processes = 1,
    -- logging — 日志
    log_level        = "warn",
    -- plugins
    plugins          = "bundled",
    -- loaded_plugins — 已加载的插件列表（用于 test_conf.loaded_plugins 查询）
    loaded_plugins   = {
        ["key-auth"] = true,
        ["basic-auth"] = true,
        ["rate-limiting"] = true,
        ["cors"] = true,
        ["tcp-log"] = true,
        ["file-log"] = true,
        ["http-log"] = true,
        ["ip-restriction"] = true,
        ["request-transformer"] = true,
        ["response-transformer"] = true,
        ["pre-function"] = true,
        ["post-function"] = true,
        ["acl"] = true,
        ["bot-detection"] = true,
        ["correlation-id"] = true,
        ["jwt"] = true,
        ["hmac-auth"] = true,
        ["oauth2"] = true,
        ["ldap-auth"] = true,
        ["session"] = true,
        ["request-size-limiting"] = true,
        ["request-termination"] = true,
        ["response-ratelimiting"] = true,
        ["syslog"] = true,
        ["loggly"] = true,
        ["datadog"] = true,
        ["udp-log"] = true,
        ["statsd"] = true,
        ["prometheus"] = true,
    },
}

---------------------------------------------------------------------------
-- db shim — 数据库对象兼容层（供 helpers.db.daos 使用）
---------------------------------------------------------------------------
_M.db = {
    daos = {}
}
-- Populate with entity objects that have schema.name — 填充具有 schema.name 的实体对象
for _, name in ipairs({"services", "routes", "consumers", "plugins", "upstreams",
                        "targets", "certificates", "snis", "ca_certificates", "vaults"}) do
    _M.db.daos[name] = { schema = { name = name } }
end

local KONG_RUST_BIN = os.getenv("KONG_RUST_BIN") or "./target/debug/kong"

local PID_FILE = os.getenv("KONG_SPEC_PID_FILE") or "/tmp/kong-rust-spec.pid"
local MOCK_UPSTREAM_PID_FILE = os.getenv("KONG_SPEC_MOCK_UPSTREAM_PID_FILE") or "/tmp/kong-rust-mock-upstream.pid"

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
        "nohup %s mock-upstream --port %d > /tmp/gw-mock-upstream.log 2>&1 & echo $! > %s",
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
-- Kong lifecycle — Kong 生命周期管理
---------------------------------------------------------------------------

-- build env string from conf table — 从配置表构建环境变量字符串
local function build_env_str(conf)
    conf = conf or {}

    -- Auto-start mock upstream — 自动启动 mock upstream
    _M.start_mock_upstream()

    local env = {}
    env.KONG_DATABASE = conf.database or _M.test_conf.database
    env.KONG_PG_HOST = conf.pg_host or _M.test_conf.pg_host
    env.KONG_PG_PORT = tostring(conf.pg_port or _M.test_conf.pg_port)
    env.KONG_PG_USER = conf.pg_user or _M.test_conf.pg_user
    -- Always use test_conf pg_password for actual DB connection — 始终使用 test_conf 的密码连接数据库
    -- conf.pg_password (e.g. "hide_me") is only for configuration display testing — conf.pg_password 仅用于配置展示测试
    env.KONG_PG_PASSWORD = _M.test_conf.pg_password
    -- Store display password separately if different — 存储展示用密码（如 "hide_me"）
    if conf.pg_password and conf.pg_password ~= _M.test_conf.pg_password then
        env.KONG_PG_PASSWORD_DISPLAY = conf.pg_password
    end
    env.KONG_PG_DATABASE = conf.pg_database or _M.test_conf.pg_database
    env.KONG_PROXY_LISTEN = conf.proxy_listen
        or string.format("0.0.0.0:%d", _M.test_conf.proxy_port)
    env.KONG_ADMIN_LISTEN = conf.admin_listen
        or string.format("0.0.0.0:%d", _M.test_conf.admin_port)
    env.KONG_STATUS_LISTEN = conf.status_listen
        or string.format("127.0.0.1:%d", _M.test_conf.status_port)
    env.KONG_LOG_LEVEL = conf.log_level or _M.test_conf.log_level

    if conf.plugins then
        env.KONG_PLUGINS = type(conf.plugins) == "table"
            and table.concat(conf.plugins, ",")
            or tostring(conf.plugins)
    end

    -- pass through any KONG_* keys from conf — 透传 conf 中的 KONG_* 键
    for k, v in pairs(conf) do
        local upper = k:upper()
        if upper:sub(1, 5) == "KONG_" and not env[upper] then
            env[upper] = tostring(v)
        elseif not env["KONG_" .. upper] then
            env["KONG_" .. upper] = tostring(v)
        end
    end

    local env_parts = {}
    for k, v in pairs(env) do
        env_parts[#env_parts + 1] = string.format("%s='%s'", k, v)
    end
    return table.concat(env_parts, " "), env
end

function _M.start_kong(conf)
    conf = conf or {}

    -- Stop any existing Kong instance before starting a new one — 启动新实例前先停止已有实例
    _M.stop_kong()

    -- Kill any orphaned kong processes matching our binary — 清理匹配我们 binary 的残留 kong 进程
    os.execute(string.format("pkill -f '%s' 2>/dev/null || true", KONG_RUST_BIN))

    -- Wait for ALL kong ports to be released — 等待所有 kong 端口释放
    local ports_to_check = {
        _M.test_conf.admin_port,
        _M.test_conf.proxy_port,
        _M.test_conf.status_port,
    }
    _M.wait_until(function()
        for _, port in ipairs(ports_to_check) do
            local s = socket.tcp()
            local ok = s:connect("127.0.0.1", port)
            s:close()
            if ok then return false end
        end
        return true
    end, 10)
    socket.sleep(0.5)

    local env_str, env = build_env_str(conf)

    if env.KONG_DATABASE ~= "off" then
        os.execute(string.format("%s %s db bootstrap 2>/dev/null || true",
            env_str, KONG_RUST_BIN))
    end

    local cmd = string.format(
        "%s nohup %s start > /tmp/gw-spec.log 2>&1 & echo $! > %s",
        env_str, KONG_RUST_BIN, PID_FILE)
    os.execute(cmd)

    local ok = _M.wait_until(function()
        local client = _M.admin_client()
        if not client then return false end
        local res = client:get("/status")
        return res and res.status == 200
    end, 30)

    if not ok then
        _M.stop_kong()
        error("Kong-Rust failed to start within 30 seconds. Check /tmp/kong-rust-spec.log")
    end

    return true
end

function _M.stop_kong(prefix, preserve_prefix, preserve_dc)
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

function _M.restart_kong(conf)
    _M.stop_kong()
    return _M.start_kong(conf)
end

function _M.reload_kong(conf)
    -- Kong-Rust: reload = restart (no hot reload support yet)
    -- Kong-Rust: reload = 重启（暂不支持热重载）
    return _M.restart_kong(conf)
end

function _M.cleanup_kong(prefix)
    _M.stop_kong()
end

---------------------------------------------------------------------------
-- HTTP Clients — HTTP 客户端
---------------------------------------------------------------------------

-- generic HTTP client constructor — 通用 HTTP 客户端构造器
-- supports both (host, port, timeout) and ({host=, port=, ...}) forms
_M.http_client = function(host_or_opts, port, timeout)
    if type(host_or_opts) == "table" then
        local opts = host_or_opts
        return http_client.new(
            opts.host or "127.0.0.1",
            opts.port,
            {
                timeout = opts.timeout and (opts.timeout / 1000) or 10,
                scheme = opts.scheme or "http",
            }
        )
    else
        return http_client.new(
            host_or_opts or "127.0.0.1",
            port,
            { timeout = timeout and (timeout / 1000) or 10 }
        )
    end
end

function _M.proxy_client(timeout, forced_port, forced_ip)
    return http_client.new(
        forced_ip or _M.test_conf.proxy_host,
        forced_port or _M.test_conf.proxy_port,
        { timeout = timeout or 10 }
    )
end

function _M.proxy_ssl_client(timeout, sni)
    return http_client.new(
        _M.test_conf.proxy_host,
        _M.test_conf.proxy_ssl_port,
        { timeout = timeout or 10, scheme = "https" }
    )
end

function _M.admin_client(timeout, forced_port)
    return http_client.new(
        _M.test_conf.admin_host,
        forced_port or _M.test_conf.admin_port,
        { timeout = timeout or 10 }
    )
end

function _M.admin_ssl_client(timeout)
    return http_client.new(
        _M.test_conf.admin_host,
        _M.test_conf.admin_ssl_port,
        { timeout = timeout or 10, scheme = "https" }
    )
end

---------------------------------------------------------------------------
-- Proxy/Admin port helpers — 代理/管理端口辅助
---------------------------------------------------------------------------

function _M.get_proxy_port(ssl)
    if ssl then
        return _M.test_conf.proxy_ssl_port
    end
    return _M.test_conf.proxy_port
end

function _M.get_proxy_ip(ssl)
    return _M.test_conf.proxy_host
end

---------------------------------------------------------------------------
-- Blueprint — 通过 Admin API 创建测试数据
---------------------------------------------------------------------------

local Blueprint = {}

-- sequence counter for generating unique names — 生成唯一名称的序列计数器
local seq_counter = 0
local function next_seq()
    seq_counter = seq_counter + 1
    return seq_counter
end

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
        vaults       = "/vaults",
    }

    -- Standard entity default value generators (like Kong's blueprints.lua)
    -- 标准实体默认值生成器（对齐 Kong 的 blueprints.lua）
    local standard_defaults = {}
    standard_defaults.services = function(overrides)
        overrides = overrides or {}
        return {
            protocol = overrides.protocol or "http",
            host = overrides.host or "127.0.0.1",
            port = overrides.port or 15555,
            name = overrides.name,
            path = overrides.path,
            tags = overrides.tags,
            enabled = overrides.enabled,
            connect_timeout = overrides.connect_timeout,
            read_timeout = overrides.read_timeout,
            write_timeout = overrides.write_timeout,
            retries = overrides.retries,
            client_certificate = overrides.client_certificate,
            tls_verify = overrides.tls_verify,
            tls_verify_depth = overrides.tls_verify_depth,
            ca_certificates = overrides.ca_certificates,
            url = overrides.url,
        }
    end
    standard_defaults.consumers = function(overrides)
        overrides = overrides or {}
        local n = next_seq()
        -- Add random suffix to avoid unique constraint violations across test runs — 添加随机后缀避免跨测试运行的唯一约束冲突
        local rand_suffix = math.random(100000, 999999)
        return {
            custom_id = overrides.custom_id or ("consumer-cid-" .. n .. "-" .. rand_suffix),
            username = overrides.username or ("consumer-" .. n .. "-" .. rand_suffix),
            tags = overrides.tags,
        }
    end
    standard_defaults.routes = function(overrides)
        overrides = overrides or {}
        local service = overrides.service
        if not service and not overrides.no_service then
            local svc_data = standard_defaults.services()
            local res = bp.admin:post("/services", {
                body = svc_data,
                headers = { ["Content-Type"] = "application/json" },
            })
            if res and res.status >= 200 and res.status < 300 then
                service = { id = cjson.decode(res.body).id }
            end
        end
        local out = {}
        for k, v in pairs(overrides) do
            if k ~= "no_service" then out[k] = v end
        end
        out.service = service
        return out
    end
    standard_defaults.upstreams = function(overrides)
        overrides = overrides or {}
        local n = next_seq()
        return {
            name = overrides.name or ("upstream-" .. n),
            slots = overrides.slots or 100,
            host_header = overrides.host_header,
            tags = overrides.tags,
        }
    end
    standard_defaults.targets = function(overrides)
        overrides = overrides or {}
        return {
            weight = overrides.weight or 10,
            target = overrides.target or "127.0.0.1:15555",
            upstream = overrides.upstream,
            tags = overrides.tags,
        }
    end

    -- default data generators for "named_*" entities — "named_*" 实体的默认数据生成器
    local defaults_generators = {
        named_services = function(overrides)
            local n = next_seq()
            local defaults = {
                protocol = "http",
                name = "service-" .. n,
                host = "service" .. n .. ".test",
                port = 15555,
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/services"
        end,
        named_routes = function(overrides)
            local n = next_seq()
            local defaults = {
                name = "route-" .. n,
                hosts = { "route" .. n .. ".test" },
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            -- auto-create a service if not specified — 如果未指定则自动创建 service
            if not defaults.service then
                local gen, ep = defaults_generators.named_services()
                local res = bp.admin:post(ep, {
                    body = gen,
                    headers = { ["Content-Type"] = "application/json" },
                })
                if res and res.status >= 200 and res.status < 300 then
                    local svc = cjson.decode(res.body)
                    defaults.service = { id = svc.id }
                end
            end
            return defaults, "/routes"
        end,
        key_auth_plugins = function(overrides)
            local defaults = {
                name = "key-auth",
                config = {},
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        acl_plugins = function(overrides)
            local defaults = {
                name = "acl",
                config = {},
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        hmac_auth_plugins = function(overrides)
            local defaults = {
                name = "hmac-auth",
                config = {},
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        basic_auth_plugins = function(overrides)
            local defaults = {
                name = "basic-auth",
                config = {},
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        rate_limiting_plugins = function(overrides)
            local defaults = {
                name = "rate-limiting",
                config = { minute = 100 },
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        cors_plugins = function(overrides)
            local defaults = {
                name = "cors",
                config = {},
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        rewriter_plugins = function(overrides)
            local defaults = {
                name = "pre-function",
                config = { access = { "kong.response.exit(200, '{\"message\":\"rewrite\"}')" } },
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        tcp_log_plugins = function(overrides)
            local defaults = {
                name = "tcp-log",
                config = { host = "127.0.0.1", port = 35001 },
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        file_log_plugins = function(overrides)
            local defaults = {
                name = "file-log",
                config = { path = os.tmpname() },
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
        http_log_plugins = function(overrides)
            local defaults = {
                name = "http-log",
                config = { http_endpoint = "http://127.0.0.1:35001" },
            }
            if overrides then
                for k, v in pairs(overrides) do defaults[k] = v end
            end
            return defaults, "/plugins"
        end,
    }

    -- make_entity_inserter: create insert/truncate/remove for an endpoint — 为 endpoint 创建 insert/truncate/remove
    local function make_entity_ops(key, endpoint, defaults_fn)
        return {
            insert = function(_, data, opts)
                -- if defaults_fn exists, merge defaults with data — 如果有 defaults_fn，将默认值与 data 合并
                if defaults_fn then
                    local defaults, ep = defaults_fn(data)
                    endpoint = ep or endpoint
                    data = defaults
                end
                data = data or {}

                local actual_endpoint = endpoint
                if key == "targets" and data.upstream then
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

            -- insert_n: batch insert N entities with optional defaults — 批量插入 N 个实体，可选默认值
            insert_n = function(self, count, defaults)
                local entities = {}
                for i = 1, count do
                    entities[i] = self:insert(defaults)
                end
                return entities
            end,

            -- remove: alias for delete by id — remove: 按 id 删除的别名
            remove = function(_, data)
                if data and data.id then
                    bp.admin:delete(endpoint .. "/" .. data.id)
                end
            end,
        }
    end

    setmetatable(bp, {
        __index = function(_, key)
            -- check named entity generators first — 先检查命名实体生成器
            local gen = defaults_generators[key]
            if gen then
                return make_entity_ops(key, nil, gen)
            end

            local endpoint = entity_endpoints[key]
            if not endpoint then return nil end

            -- use standard defaults if available — 使用标准默认值（如果有）
            local std_gen = standard_defaults[key]
            return make_entity_ops(key, endpoint, std_gen)
        end,
    })

    return bp
end

---------------------------------------------------------------------------
-- DB proxy — 数据库代理对象（通过 Admin API 模拟直接 DB 访问）
---------------------------------------------------------------------------
local DbProxy = {}

function DbProxy:new(admin_client_fn)
    local db = {}

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
        vaults       = "/vaults",
    }

    -- truncate: delete all entities of a type — 清空某类型的所有实体
    function db:truncate(entity_name)
        local endpoint = entity_endpoints[entity_name]
        if not endpoint then
            endpoint = "/" .. entity_name:gsub("_", "-")
        end
        if endpoint:find("%%s") then return true end

        local admin = admin_client_fn()
        if not admin then return true end

        -- paginate through all and delete — 分页遍历并删除
        local deleted = true
        while deleted do
            deleted = false
            local res = admin:get(endpoint)
            if res and res.status == 200 then
                local ok, body = pcall(cjson.decode, res.body)
                if ok and body and body.data and #body.data > 0 then
                    for _, item in ipairs(body.data) do
                        admin:delete(endpoint .. "/" .. item.id)
                        deleted = true
                    end
                end
            end
        end
        return true
    end

    -- entity proxy maker — 实体代理构造器
    local function make_entity_proxy(entity_name, endpoint)
        local proxy = {}

        function proxy:insert(data)
            local admin = admin_client_fn()
            local actual_endpoint = endpoint
            if entity_name == "targets" and data and data.upstream then
                local uid = type(data.upstream) == "table"
                    and (data.upstream.id or data.upstream.name)
                    or data.upstream
                actual_endpoint = string.format(endpoint, uid)
                data.upstream = nil
            end
            local res = admin:post(actual_endpoint, {
                body = data,
                headers = { ["Content-Type"] = "application/json" },
            })
            if not res then
                return nil, "failed to connect to Admin API"
            end
            if res.status >= 200 and res.status < 300 then
                return cjson.decode(res.body)
            end
            return nil, cjson.decode(res.body) or res.body
        end

        function proxy:select(pk_or_filter, opts)
            local admin = admin_client_fn()
            local id
            if type(pk_or_filter) == "table" then
                id = pk_or_filter.id
            else
                id = pk_or_filter
            end
            if not id then return nil, "primary key required" end
            local res = admin:get(endpoint .. "/" .. id)
            if res and res.status == 200 then
                return cjson.decode(res.body)
            end
            return nil
        end

        function proxy:select_by_name(name, opts)
            local admin = admin_client_fn()
            local res = admin:get(endpoint .. "/" .. name)
            if res and res.status == 200 then
                return cjson.decode(res.body)
            end
            return nil
        end

        function proxy:update(pk, data, opts)
            local admin = admin_client_fn()
            local id = type(pk) == "table" and pk.id or pk
            local res = admin:patch(endpoint .. "/" .. id, {
                body = data,
                headers = { ["Content-Type"] = "application/json" },
            })
            if res and res.status == 200 then
                return cjson.decode(res.body)
            end
            return nil, res and res.body or "update failed"
        end

        function proxy:delete(pk)
            local admin = admin_client_fn()
            local id = type(pk) == "table" and pk.id or pk
            admin:delete(endpoint .. "/" .. id)
            return true
        end

        function proxy:truncate()
            return db:truncate(entity_name)
        end

        function proxy:page(size, offset, opts)
            local admin = admin_client_fn()
            local query = {}
            if size then query.size = size end
            if offset then query.offset = offset end
            local path = endpoint
            if next(query) then
                local parts = {}
                for k, v in pairs(query) do
                    parts[#parts + 1] = k .. "=" .. tostring(v)
                end
                path = path .. "?" .. table.concat(parts, "&")
            end
            local res = admin:get(path)
            if res and res.status == 200 then
                local body = cjson.decode(res.body)
                return body.data, nil, body.offset
            end
            return {}, nil, nil
        end

        return proxy
    end

    setmetatable(db, {
        __index = function(_, key)
            local endpoint = entity_endpoints[key]
            if endpoint then
                return make_entity_proxy(key, endpoint)
            end
            return nil
        end,
    })

    return db
end

function _M.get_db_utils(strategy, tables, plugins)
    -- strategy is accepted but ignored (Kong-Rust only supports postgres)
    -- strategy 参数接受但忽略（Kong-Rust 仅支持 postgres）
    if strategy and strategy ~= "postgres" and strategy ~= "off" then
        -- skip strategies we don't support — 跳过不支持的策略
        return nil, "strategy '" .. strategy .. "' not supported"
    end

    -- Ensure Kong is running (auto-start if not) — 确保 Kong 正在运行（如果没有则自动启动）
    local admin = _M.admin_client()
    if not admin then
        -- Kong not running yet, start it — Kong 尚未运行，启动它
        _M.start_kong({ database = strategy or "postgres" })
        admin = _M.admin_client()
        if not admin then
            error("Failed to connect to Admin API after starting Kong-Rust")
        end
    else
        -- Check if connection works — 检查连接是否正常
        local res = admin:get("/status")
        if not res or res.status ~= 200 then
            _M.start_kong({ database = strategy or "postgres" })
            admin = _M.admin_client()
        end
    end

    -- truncate specified tables or all — 清空指定表或所有表
    if tables then
        for _, tbl in ipairs(tables) do
            local endpoint = "/" .. tbl:gsub("_", "-")
            local res = admin:get(endpoint)
            if res and res.status == 200 then
                local ok, body = pcall(cjson.decode, res.body)
                if ok and body and body.data then
                    for _, item in ipairs(body.data) do
                        admin:delete(endpoint .. "/" .. item.id)
                    end
                end
            end
        end
    end

    local bp = Blueprint:new(admin)
    local db = DbProxy:new(function() return _M.admin_client() end)
    return bp, db
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

function _M.wait_until(fn, timeout, step)
    timeout = timeout or 10
    step = step or 0.1
    local deadline = socket.gettime() + timeout
    local last_err
    while socket.gettime() < deadline do
        local ok, res = pcall(fn)
        if ok and res then
            return true
        end
        if not ok then
            last_err = res
        end
        socket.sleep(step)
    end
    return false, last_err
end

-- pwait_until: protected wait — 受保护的等待（函数无错误即成功）
function _M.pwait_until(fn, timeout, step)
    timeout = timeout or 10
    step = step or 0.1
    local deadline = socket.gettime() + timeout
    local last_err
    while socket.gettime() < deadline do
        local ok, err = pcall(fn)
        if ok then
            return true
        end
        last_err = err
        socket.sleep(step)
    end
    error("pwait_until timeout after " .. timeout .. "s: " .. tostring(last_err))
end

-- wait_for_file: wait for file to exist with given mode — 等待文件以指定模式存在
function _M.wait_for_file(mode, path, timeout)
    timeout = timeout or 10
    _M.pwait_until(function()
        -- use pl.path for mode checking — 使用 pl.path 检查文件模式
        if mode == "file" then
            assert(pl_path.isfile(path),
                string.format("expected '%s' to be a file", path))
        elseif mode == "directory" then
            assert(pl_path.isdir(path),
                string.format("expected '%s' to be a directory", path))
        else
            assert(pl_path.exists(path),
                string.format("expected '%s' to exist", path))
        end
    end, timeout)
end

-- wait_for_file_contents: wait for file to exist and be non-empty — 等待文件存在且非空
function _M.wait_for_file_contents(fname, timeout)
    assert(type(fname) == "string", "filename must be a string")
    timeout = timeout or 10

    -- try immediate read — 先尝试立即读取
    local data = pl_file.read(fname)
    if data and #data > 0 then
        return data
    end

    -- poll until file has content — 轮询直到文件有内容
    pcall(_M.wait_until, function()
        data = pl_file.read(fname)
        return data and #data > 0
    end, timeout)

    assert(data, "file (" .. fname .. ") does not exist or is not readable"
                 .. " after " .. tostring(timeout) .. " seconds")
    assert(#data > 0, "file (" .. fname .. ") exists but is empty after "
                      .. tostring(timeout) .. " seconds")
    return data
end

function _M.sleep(seconds)
    socket.sleep(seconds)
end

---------------------------------------------------------------------------
-- Shell utilities — Shell 工具
---------------------------------------------------------------------------

function _M.execute(cmd)
    local handle = io.popen(cmd .. " 2>&1")
    local result = handle:read("*a")
    local ok, _, code = handle:close()
    return result, "", code or (ok and 0 or 1)
end

-- kong_exec: execute kong CLI command — 执行 kong CLI 命令
function _M.kong_exec(cmd, env)
    local env_str = ""
    if env then
        local parts = {}
        for k, v in pairs(env) do
            local key = k:upper()
            if key:sub(1, 5) ~= "KONG_" then
                key = "KONG_" .. key
            end
            parts[#parts + 1] = string.format("%s=%s", key, tostring(v))
        end
        env_str = table.concat(parts, " ") .. " "
    end
    local full_cmd = string.format("%s%s %s", env_str, KONG_RUST_BIN, cmd)
    return _M.execute(full_cmd)
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
    if not admin then return end

    local entities = { "plugins", "snis", "routes", "services", "consumers",
                       "targets", "upstreams", "certificates", "ca_certificates" }
    for _, entity in ipairs(entities) do
        local res = admin:get("/" .. entity:gsub("_", "-"))
        if res and res.status == 200 then
            local ok, body = pcall(cjson.decode, res.body)
            if ok and body and body.data then
                for _, item in ipairs(body.data) do
                    admin:delete("/" .. entity:gsub("_", "-") .. "/" .. item.id)
                end
            end
        end
    end
end

---------------------------------------------------------------------------
-- TCP/UDP server — TCP/UDP 服务器
---------------------------------------------------------------------------

-- tcp_server: start a simple TCP echo server in a coroutine
-- TCP 服务器：在协程中启动简单的 TCP 回显服务器
function _M.tcp_server(port, opts)
    opts = opts or {}
    port = port or _M.mock_upstream_port

    local server = assert(socket.tcp())
    server:settimeout(opts.timeout or 60)
    assert(server:setoption("reuseaddr", true))
    assert(server:bind("*", port))
    assert(server:listen())

    -- return a thread-like object with :join() — 返回类线程对象，支持 :join()
    local result = nil
    local done = false

    -- use a coroutine to simulate llthreads2 behavior — 用协程模拟 llthreads2 行为
    local co = coroutine.create(function()
        local n = opts.requests or 1
        local data = {}
        for i = 1, n do
            local client, err = server:accept()
            if not client then
                result = nil
                done = true
                server:close()
                return
            end
            local line, recv_err = client:receive()
            if line then
                data[i] = line
                client:send((opts.prefix or "") .. line .. "\n")
            end
            client:close()
        end
        server:close()
        result = n == 1 and data[1] or data
        done = true
    end)

    -- start the coroutine — 启动协程
    coroutine.resume(co)

    return {
        join = function(self)
            -- poll until done — 轮询直到完成
            local deadline = socket.gettime() + (opts.timeout or 60)
            while not done and socket.gettime() < deadline do
                if coroutine.status(co) == "suspended" then
                    coroutine.resume(co)
                end
                socket.sleep(0.01)
            end
            return true, result
        end,
    }
end

-- udp_server: start a simple UDP server in a coroutine
-- UDP 服务器：在协程中启动简单的 UDP 服务器
function _M.udp_server(port, n, timeout)
    port = port or _M.mock_upstream_port
    n = n or 1
    timeout = timeout or 360

    local server = assert(socket.udp())
    server:settimeout(timeout)
    server:setoption("reuseaddr", true)
    server:setsockname("127.0.0.1", port)

    local result = nil
    local done = false

    local co = coroutine.create(function()
        local data = {}
        local i = 0
        while i < n do
            local pkt, err = server:receive()
            if not pkt then
                break
            end
            i = i + 1
            data[i] = pkt
        end
        server:close()
        result = n == 1 and data[1] or data
        done = true
    end)

    coroutine.resume(co)

    return {
        join = function(self)
            local deadline = socket.gettime() + timeout
            while not done and socket.gettime() < deadline do
                if coroutine.status(co) == "suspended" then
                    coroutine.resume(co)
                end
                socket.sleep(0.01)
            end
            return true, result
        end,
    }
end

---------------------------------------------------------------------------
-- Misc utilities — 杂项工具
---------------------------------------------------------------------------

function _M.uuid()
    local template = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
    return string.gsub(template, "[xy]", function(c)
        local v = (c == "x") and math.random(0, 0xf) or math.random(8, 0xb)
        return string.format("%x", v)
    end)
end

-- unindent: remove common leading whitespace — 移除公共前导空白
function _M.unindent(str, concat_newlines, spaced_newlines)
    str = string.match(str, "(.-%S*)%s*$")
    if not str then
        return ""
    end

    local level = math.huge
    local prefix = ""
    local len

    str = str:match("^%s") and "\n" .. str or str
    for pref in str:gmatch("\n(%s+)") do
        len = #pref
        if len < level then
            level = len
            prefix = pref
        end
    end

    local repl = concat_newlines and "" or "\n"
    repl = spaced_newlines and " " or repl

    return (str:gsub("^\n%s*", ""):gsub("\n" .. prefix, repl):gsub("\n$", ""):gsub("\\r", "\r"))
end

-- make_yaml_file: write YAML content to a temp file — 写 YAML 内容到临时文件
function _M.make_yaml_file(content, filename)
    filename = filename or pl_path.tmpname() .. ".yml"
    if content then
        local fd = assert(io.open(filename, "w"))
        assert(fd:write(_M.unindent(content)))
        assert(fd:write("\n"))
        assert(fd:close())
    end
    return filename
end

-- setenv / unsetenv: set/unset environment variables — 设置/取消环境变量
-- Note: Lua 5.1 has no native setenv; use os.execute as fallback
-- 注意：Lua 5.1 没有原生 setenv；使用 os.execute 作为后备
do
    local ok, posix = pcall(require, "posix")
    if ok and posix and posix.setenv then
        _M.setenv = function(name, value)
            posix.setenv(name, value)
        end
        _M.unsetenv = function(name)
            posix.setenv(name, nil)
        end
    else
        -- fallback: these only affect child processes, not current process
        -- 后备方案：仅影响子进程，不影响当前进程
        _M.setenv = function(name, value)
            -- no-op in pure Lua 5.1, but some C modules provide this
            -- 在纯 Lua 5.1 中无操作
        end
        _M.unsetenv = function(name)
            -- no-op — 无操作
        end
    end
end

-- deep_sort: recursively sort tables for comparison — 递归排序表以便比较
do
    local deep_sort

    local function deep_compare(a, b)
        if a == nil then a = "" end
        if b == nil then b = "" end

        deep_sort(a)
        deep_sort(b)

        if type(a) ~= type(b) then
            return type(a) < type(b)
        end

        if type(a) == "table" then
            return deep_compare(a[1], b[1])
        end

        if type(a) == "userdata" and type(b) == "userdata" then
            return false
        end

        return a < b
    end

    deep_sort = function(t)
        if type(t) == "table" then
            for _, v in pairs(t) do
                deep_sort(v)
            end
            table.sort(t, deep_compare)
        end
        return t
    end

    _M.deep_sort = deep_sort
end

-- intercept: capture stdout/stderr of a function — 捕获函数的标准输出/错误
function _M.intercept(fn, ...)
    -- simple implementation: just run the function — 简单实现：直接运行函数
    local old_print = print
    local output = {}
    print = function(...)
        local args = { ... }
        for i, v in ipairs(args) do
            args[i] = tostring(v)
        end
        output[#output + 1] = table.concat(args, "\t")
    end
    local results = { pcall(fn, ...) }
    print = old_print
    return table.concat(output, "\n"), results
end

-- make_temp_dir: create a temporary directory — 创建临时目录
function _M.make_temp_dir()
    local name = os.tmpname()
    os.remove(name)
    pl_dir.makepath(name)
    return name
end

-- generate_keys: generate RSA key pair (PEM) — 生成 RSA 密钥对
function _M.generate_keys(key_type)
    key_type = key_type or "RSA"
    local tmpfile = os.tmpname()
    local pubfile = tmpfile .. ".pub"
    if key_type == "RSA" then
        os.execute(string.format(
            "openssl genrsa -out %s 2048 2>/dev/null", tmpfile))
        os.execute(string.format(
            "openssl rsa -in %s -pubout -out %s 2>/dev/null", tmpfile, pubfile))
    end
    local private_key = pl_file.read(tmpfile) or ""
    local public_key = pl_file.read(pubfile) or ""
    os.remove(tmpfile)
    os.remove(pubfile)
    return private_key, public_key
end

---------------------------------------------------------------------------
-- get_available_port — 获取可用端口
-- Binds to port 0, reads the assigned port, then closes the socket.
---------------------------------------------------------------------------
function _M.get_available_port()
  local server = assert(socket.bind("127.0.0.1", 0))
  local _, port = server:getsockname()
  server:close()
  return tonumber(port)
end

return _M
