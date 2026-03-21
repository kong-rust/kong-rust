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

function RequestTerminationHandler:access(conf)
  local status = conf.status_code or 503
  local body = conf.body
  local message = conf.message
  local content_type = conf.content_type

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

  -- Default: empty body — 默认: 空 body
  return kong.response.exit(status)
end

return RequestTerminationHandler
