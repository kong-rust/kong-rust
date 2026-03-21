-- spec/fixtures/http_client.lua
-- HTTP client wrapper using luasocket — 基于 luasocket 的 HTTP 客户端封装

local socket = require("socket")
local http_socket = require("socket.http")
local ltn12 = require("ltn12")
local url_mod = require("socket.url")

local _M = {}
local Client = {}
Client.__index = Client

function _M.new(host, port, opts)
    opts = opts or {}
    local self = setmetatable({}, Client)
    self.host = host or "127.0.0.1"
    self.port = port
    self.scheme = opts.scheme or "http"
    self.timeout = (opts.timeout or 10) * 1000
    return self
end

function Client:_url(path)
    return string.format("%s://%s:%d%s", self.scheme, self.host, self.port, path or "/")
end

function Client:send(opts)
    opts = opts or {}
    local method = (opts.method or "GET"):upper()
    local path = opts.path or "/"

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

    local ok, status_code, response_headers = http_socket.request({
        url = full_url,
        method = method,
        headers = req_headers,
        source = request_body and ltn12.source.string(request_body) or nil,
        sink = ltn12.sink.table(response_body),
        redirect = false,
    })

    if not ok then
        return nil, status_code
    end

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
        read_body = function(self)
            return self.body
        end,
    }
end

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

function Client:close()
end

return _M
