--- spec.helpers.http_mock — shim for Kong-Rust test suite
-- Provides a lightweight HTTP mock server using LuaSocket.
-- Compatible with the Kong http_mock API: new(), start(), stop(), get_request().
--
-- 轻量级 HTTP 模拟服务器，用于测试中捕获和回放请求。

local socket = require "socket"

local _M = {}
local _MT = { __index = _M }

--- Create a new http_mock instance.
-- @param listen  port number, or "host:port" string
-- @param routes  (optional) table of path -> handler definitions (ignored in shim)
-- @param opts    (optional) options table (ignored in shim)
-- @return http_mock instance
function _M.new(listen, routes, opts)
  local host, port
  if type(listen) == "number" then
    host = "127.0.0.1"
    port = listen
  elseif type(listen) == "string" then
    local h, p = listen:match("^(.+):(%d+)$")
    if h and p then
      host = h
      port = tonumber(p)
    else
      -- try as plain port
      port = tonumber(listen)
      host = "127.0.0.1"
    end
  end

  assert(port, "http_mock.new: could not determine port from: " .. tostring(listen))

  local self = setmetatable({
    host = host,
    port = port,
    routes = routes or {},
    opts = opts or {},
    server = nil,
    requests = {},    -- captured request list — 捕获的请求列表
    _running = false,
    _thread = nil,
  }, _MT)

  return self
end

--- Parse a raw HTTP request string into a table.
-- 将原始 HTTP 请求字符串解析为 table
local function parse_request(raw)
  if not raw or raw == "" then
    return nil
  end

  local req = {
    headers = {},
    raw = raw,
  }

  local lines = {}
  for line in raw:gmatch("([^\r\n]*)\r?\n?") do
    lines[#lines + 1] = line
  end

  if #lines == 0 then
    return nil
  end

  -- Parse request line — 解析请求行
  local method, uri, version = lines[1]:match("^(%S+)%s+(%S+)%s+(%S+)")
  req.method = method
  req.uri = uri
  req.version = version

  -- Parse headers — 解析头部
  local i = 2
  while i <= #lines do
    local line = lines[i]
    if line == "" then
      i = i + 1
      break
    end
    local name, value = line:match("^([^:]+):%s*(.*)")
    if name then
      -- Keep original case for header names — 保持原始大小写
      req.headers[name] = value
    end
    i = i + 1
  end

  -- Remaining is body — 剩余部分为请求体
  local body_parts = {}
  while i <= #lines do
    body_parts[#body_parts + 1] = lines[i]
    i = i + 1
  end
  if #body_parts > 0 then
    req.body = table.concat(body_parts, "\n")
  end

  return req
end

--- Build an HTTP response string — 构建 HTTP 响应
local function build_response(status, headers, body)
  status = status or 200
  body = body or ""
  headers = headers or {}

  local status_text = ({
    [200] = "OK",
    [201] = "Created",
    [204] = "No Content",
    [400] = "Bad Request",
    [404] = "Not Found",
    [500] = "Internal Server Error",
  })[status] or "OK"

  local resp = "HTTP/1.1 " .. status .. " " .. status_text .. "\r\n"

  if not headers["Content-Length"] then
    headers["Content-Length"] = tostring(#body)
  end
  if not headers["Content-Type"] then
    headers["Content-Type"] = "text/plain"
  end
  if not headers["Connection"] then
    headers["Connection"] = "close"
  end

  for k, v in pairs(headers) do
    resp = resp .. k .. ": " .. v .. "\r\n"
  end
  resp = resp .. "\r\n" .. body

  return resp
end

--- Start the mock server. Spawns a coroutine-based accept loop.
-- 启动模拟服务器
function _M:start()
  if self._running then
    return true
  end

  local server, err = socket.bind(self.host, self.port)
  if not server then
    return nil, "http_mock: failed to bind " .. self.host .. ":" .. self.port .. ": " .. tostring(err)
  end

  server:settimeout(0.1)
  self.server = server
  self._running = true

  -- Accept loop runs in a background coroutine managed by polling — 后台轮询接受连接
  -- We use a simple approach: the accept loop runs in the main thread
  -- via non-blocking accept, polled in get_request() and stop().
  return true
end

--- Poll for incoming connections and capture requests — 轮询并捕获请求
function _M:_poll(timeout)
  if not self._running or not self.server then
    return
  end

  local deadline = socket.gettime() + (timeout or 0.1)

  while socket.gettime() < deadline do
    local client = self.server:accept()
    if client then
      client:settimeout(1)
      local raw, err = client:receive("*a")
      if not raw then
        -- receive might return partial data on timeout — 超时可能返回部分数据
        raw = err == "timeout" and (client:receive("*l")) or nil
      end

      -- Try to read full request with line-by-line approach — 逐行读取完整请求
      if not raw then
        client:settimeout(0.5)
        local lines = {}
        local content_length = 0
        -- Read request line and headers — 读取请求行和头部
        while true do
          local line, lerr = client:receive("*l")
          if not line then break end
          lines[#lines + 1] = line
          if line == "" then break end  -- end of headers
          local cl = line:match("^[Cc]ontent%-[Ll]ength:%s*(%d+)")
          if cl then content_length = tonumber(cl) end
        end
        -- Read body if present — 读取请求体
        if content_length > 0 then
          local body = client:receive(content_length)
          if body then
            lines[#lines + 1] = body
          end
        end
        if #lines > 0 then
          raw = table.concat(lines, "\r\n")
        end
      end

      local req = parse_request(raw)
      if req then
        self.requests[#self.requests + 1] = req
      end

      -- Send a basic 200 response — 发送基本的 200 响应
      local response = build_response(200, {}, '{"message":"ok"}')
      pcall(function() client:send(response) end)
      pcall(function() client:close() end)
    end
  end
end

--- Get the next captured request (with timeout / polling).
-- 获取下一个捕获的请求（带超时轮询）
-- @param timeout  (optional) seconds to wait, default 5
-- @return request table with .method, .uri, .headers, .body
function _M:get_request(timeout)
  timeout = timeout or 5
  local deadline = socket.gettime() + timeout

  while socket.gettime() < deadline do
    self:_poll(0.2)
    if #self.requests > 0 then
      return table.remove(self.requests, 1)
    end
  end

  error("http_mock:get_request() timed out after " .. timeout .. "s waiting for a request")
end

--- Get all captured request logs — 获取所有捕获的请求日志
function _M:get_all_logs()
  self:_poll(0.5)
  local logs = self.requests
  self.requests = {}
  return logs
end

--- Get a session (returns a table with .req) — 获取会话
function _M:get_session(timeout)
  local req = self:get_request(timeout)
  return { req = req }
end

--- Stop the mock server — 停止模拟服务器
function _M:stop(force)
  self._running = false
  if self.server then
    pcall(function() self.server:close() end)
    self.server = nil
  end
  return true
end

return _M
