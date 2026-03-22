-- resty/http.lua
-- Shim module for lua-resty-http using luasocket — 基于 luasocket 的 resty.http 兼容层
-- Provides resty.http compatible API for use outside of OpenResty

local socket = require("socket")
local http_socket = require("socket.http")
local ltn12 = require("ltn12")
local url_mod = require("socket.url")

local _M = {
    _VERSION = "0.17.1-shim",
}

local mt = { __index = _M }

function _M.new(_)
    local self = setmetatable({
        host = nil,
        port = nil,
        timeout = 10000,
        scheme = "http",
        keepalive = false,
    }, mt)
    return self
end

function _M:connect(host, port, opts)
    opts = opts or {}
    if type(host) == "table" then
        opts = host
        host = opts.host
        port = opts.port
    end
    self.host = host
    self.port = port
    self.scheme = opts.scheme or (port == 443 and "https" or "http")
    if opts.ssl_verify == false then
        self.ssl_verify = false
    end
    return true
end

function _M:set_timeout(timeout)
    self.timeout = timeout
end

function _M:set_timeouts(connect_timeout, send_timeout, read_timeout)
    self.timeout = connect_timeout or 10000
end

function _M:request(params)
    params = params or {}
    local method = (params.method or "GET"):upper()
    local path = params.path or "/"

    -- build query string — 构建查询字符串
    if params.query then
        local query_str
        if type(params.query) == "table" then
            local parts = {}
            for k, v in pairs(params.query) do
                parts[#parts + 1] = url_mod.escape(tostring(k)) .. "=" .. url_mod.escape(tostring(v))
            end
            query_str = table.concat(parts, "&")
        else
            query_str = tostring(params.query)
        end
        if #query_str > 0 then
            path = path .. "?" .. query_str
        end
    end

    local url = string.format("%s://%s:%d%s", self.scheme, self.host, self.port, path)

    local request_body = params.body
    local response_body = {}

    local headers = params.headers or {}

    -- use HTTPS via luasec if needed — 如果需要使用 luasec 的 HTTPS
    local requester = http_socket
    if self.scheme == "https" then
        local ok, https = pcall(require, "ssl.https")
        if ok then
            requester = https
        end
    end

    local ok, status_code, response_headers = requester.request({
        url = url,
        method = method,
        headers = headers,
        source = request_body and ltn12.source.string(request_body) or nil,
        sink = ltn12.sink.table(response_body),
        redirect = false,
    })

    if not ok then
        return nil, status_code
    end

    -- normalize headers — 标准化 header
    local norm_headers = {}
    if response_headers then
        for k, v in pairs(response_headers) do
            norm_headers[k:lower()] = v
        end
    end

    local body_str = table.concat(response_body)
    local res = {
        status = status_code,
        headers = norm_headers,
        body = body_str,
        reason = "OK",
        has_body = #body_str > 0,
    }

    function res:read_body()
        return self.body
    end

    return res
end

function _M:close()
    return true
end

function _M:set_keepalive(...)
    return true
end

return _M
