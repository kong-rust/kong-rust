-- request-termination plugin handler — 请求终止插件 handler
-- Short-circuits the request and returns a configurable response — 短路请求并返回可配置的响应
-- Priority 2 matches Kong OSS — 优先级 2 与 Kong OSS 一致

local cjson = require "cjson"

local RequestTerminationHandler = {
  PRIORITY = 2,
  VERSION  = "1.0.0",
}

-- Check if value is non-nil and not cjson.null — 检查值是否非 nil 且不是 cjson.null
local function is_present(val)
  return val ~= nil and val ~= cjson.null and val ~= ngx.null
end

-- Default status code messages (Kong OSS compatible) — 默认状态码消息（Kong OSS 兼容）
local DEFAULT_MESSAGES = {
  [400] = "Bad request",
  [401] = "Unauthorized",
  [403] = "Forbidden",
  [404] = "Not found",
  [405] = "Method not allowed",
  [408] = "Request timeout",
  [409] = "Conflict",
  [410] = "Gone",
  [411] = "Length required",
  [412] = "Precondition failed",
  [413] = "Payload too large",
  [414] = "URI too long",
  [415] = "Unsupported media type",
  [429] = "Too many requests",
  [500] = "An unexpected error occurred",
  [501] = "Not implemented",
  [502] = "An invalid response was received from the upstream server",
  [503] = "Service unavailable",
  [504] = "Gateway timeout",
}

function RequestTerminationHandler:access(conf)
  local status = conf.status_code or 503
  local body = conf.body
  local message = conf.message
  local content_type = conf.content_type
  local echo = conf.echo
  local trigger = conf.trigger

  -- Echo mode — 回显模式
  if echo then
    -- Check trigger — 检查触发器
    if is_present(trigger) then
      -- Convert trigger to header-name format (replace dashes, lowercase) — 转换 trigger 为 header-name 格式
      local trigger_header = trigger
      local found = false

      -- Check as header — 检查作为 header
      local header_val = kong.request.get_header(trigger_header)
      if header_val then
        found = true
      end

      -- Check as query parameter — 检查作为查询参数
      if not found then
        local query = kong.request.get_query()
        if query and query[trigger] then
          found = true
        end
      end

      if not found then
        -- Trigger not matched, pass through — 触发器未匹配，透传
        return
      end
    end

    -- Build echo response — 构建回显响应
    local echo_response = {}

    -- Get request info — 获取请求信息
    local req_headers = kong.request.get_headers()
    local req_method = kong.request.get_method()
    local req_path = kong.request.get_path()
    local req_query = kong.request.get_query()
    local req_host = kong.request.get_host()
    local req_port = kong.request.get_port()
    local req_scheme = kong.request.get_scheme()
    local req_body = kong.request.get_raw_body()
    local uri_captures = kong.request.get_uri_captures()

    -- Clean up headers (lowercase keys, remove hop-by-hop headers) — 清理 headers（小写 key，移除逐跳头）
    local hop_by_hop = {
      ["connection"] = true,
      ["te"] = true,
      ["transfer-encoding"] = true,
      ["upgrade"] = true,
      ["proxy-authorization"] = true,
      ["proxy-connection"] = true,
      ["keep-alive"] = true,
      ["user-agent"] = true,
    }
    local clean_headers = {}
    for k, v in pairs(req_headers) do
      local lk = k:lower()
      if not hop_by_hop[lk] then
        clean_headers[lk] = v
      end
    end

    echo_response.request = {
      headers = clean_headers,
      host = req_host,
      method = req_method,
      path = req_path,
      port = req_port,
      query = req_query or {},
      raw_body = req_body or "",
      scheme = req_scheme,
      uri_captures = uri_captures or { named = {}, unnamed = {} },
    }

    -- Get matched route info — 获取匹配的路由信息
    local route = kong.router.get_route()
    if route then
      echo_response.matched_route = route
    end

    return kong.response.exit(status, cjson.encode(echo_response))
  end

  if is_present(body) then
    -- Custom body with optional content_type — 自定义 body + 可选 content_type
    local headers = {}
    if is_present(content_type) then
      headers["Content-Type"] = content_type
    end
    return kong.response.exit(status, body, headers)
  end

  if is_present(message) then
    -- JSON message — JSON 消息
    return kong.response.exit(status, cjson.encode({ message = message }))
  end

  -- Default: use status-specific message — 默认：使用状态码特定的消息
  local default_msg = DEFAULT_MESSAGES[status] or "Service unavailable"
  return kong.response.exit(status, cjson.encode({ message = default_msg }))
end

return RequestTerminationHandler
