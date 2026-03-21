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
    self.reopen = opts.reopen or false
    return self
end

function Client:_url(path)
    return string.format("%s://%s:%d%s", self.scheme, self.host, self.port, path or "/")
end

-- encode_args: encode table to application/x-www-form-urlencoded — 编码表为 form-urlencoded 格式
local function encode_args(args)
    local parts = {}
    for k, v in pairs(args) do
        if type(v) == "table" then
            -- multi-value: key=v1&key=v2 — 多值
            for _, item in ipairs(v) do
                parts[#parts + 1] = url_mod.escape(tostring(k)) .. "=" .. url_mod.escape(tostring(item))
            end
        else
            parts[#parts + 1] = url_mod.escape(tostring(k)) .. "=" .. url_mod.escape(tostring(v))
        end
    end
    return table.concat(parts, "&")
end

-- generate multipart boundary — 生成 multipart 边界
local function generate_boundary()
    return "----FormBoundary" .. string.format("%x%x", math.random(0, 0xFFFFFFFF), math.random(0, 0xFFFFFFFF))
end

-- encode multipart/form-data body — 编码 multipart/form-data 请求体
local function encode_multipart(body, boundary)
    local parts = {}
    for k, v in pairs(body) do
        parts[#parts + 1] = string.format(
            "--%s\r\nContent-Disposition: form-data; name=\"%s\"\r\n\r\n%s",
            boundary, k, tostring(v))
    end
    parts[#parts + 1] = "--" .. boundary .. "--"
    return table.concat(parts, "\r\n") .. "\r\n"
end

function Client:send(opts)
    opts = opts or {}
    local method = (opts.method or "GET"):upper()
    local path = opts.path or "/"

    -- append query parameters to path — 将查询参数追加到路径
    if opts.query then
        local query_str
        if type(opts.query) == "table" then
            query_str = encode_args(opts.query)
        else
            query_str = tostring(opts.query)
        end
        if #query_str > 0 then
            local sep = path:find("?") and "&" or "?"
            path = path .. sep .. query_str
        end
    end

    local full_url = self:_url(path)
    local response_body = {}
    local request_body = opts.body

    -- build request headers — 构建请求头
    local req_headers = {}
    if opts.headers then
        for k, v in pairs(opts.headers) do
            req_headers[k] = v
        end
    end

    -- determine content type for body encoding — 根据 Content-Type 编码请求体
    local content_type = nil
    for k, v in pairs(req_headers) do
        if k:lower() == "content-type" then
            content_type = v:lower()
            break
        end
    end

    if type(request_body) == "table" then
        if content_type and content_type:find("application/x%-www%-form%-urlencoded") then
            -- form-urlencoded encoding — form-urlencoded 编码
            request_body = encode_args(request_body)
        elseif content_type and content_type:find("multipart/form%-data") then
            -- multipart encoding — multipart 编码
            local boundary = generate_boundary()
            request_body = encode_multipart(request_body, boundary)
            -- update Content-Type with boundary — 更新 Content-Type 附带 boundary
            for k, _ in pairs(req_headers) do
                if k:lower() == "content-type" then
                    req_headers[k] = "multipart/form-data; boundary=" .. boundary
                    break
                end
            end
        else
            -- default: JSON encoding — 默认：JSON 编码
            local cjson = require("cjson")
            request_body = cjson.encode(request_body)
            if not content_type then
                req_headers["Content-Type"] = "application/json"
            end
        end
    end

    if request_body then
        req_headers["Content-Length"] = tostring(#request_body)
    end

    -- use HTTPS via luasec if scheme is https — 如果是 https 则使用 luasec
    local requester = http_socket
    if self.scheme == "https" then
        local ok, https = pcall(require, "ssl.https")
        if ok then
            requester = https
        end
    end

    local ok, status_code, response_headers = requester.request({
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

    -- normalize response headers (lowercase keys) — 标准化响应头（小写键）
    local norm_headers = {}
    if response_headers then
        for k, v in pairs(response_headers) do
            norm_headers[k:lower()] = v
        end
    end

    -- build response object — 构建响应对象
    local body_str = table.concat(response_body)
    local res = {
        status = status_code,
        headers = norm_headers,
        body = body_str,
    }

    -- read_body(): compatible with resty.http response — 兼容 resty.http 响应
    function res:read_body()
        return self.body
    end

    return res
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

function Client:options(path, opts)
    opts = opts or {}
    opts.method = "OPTIONS"
    opts.path = path
    return self:send(opts)
end

function Client:head(path, opts)
    opts = opts or {}
    opts.method = "HEAD"
    opts.path = path
    return self:send(opts)
end

function Client:close()
    -- no-op for luasocket (connection-per-request) — luasocket 无需关闭
end

return _M
