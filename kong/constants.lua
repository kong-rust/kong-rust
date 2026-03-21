-- kong/constants.lua
-- Shim module for Kong constants — Kong 常量 shim 模块

local _M = {}

_M.CJSON_MAX_PRECISION = 16

_M.HEADERS = {
    HOST_OVERRIDE       = "X-Host-Override",
    PROXY_LATENCY       = "X-Kong-Proxy-Latency",
    RESPONSE_LATENCY    = "X-Kong-Response-Latency",
    ADMIN_LATENCY       = "X-Kong-Admin-Latency",
    UPSTREAM_LATENCY    = "X-Kong-Upstream-Latency",
    UPSTREAM_STATUS     = "X-Kong-Upstream-Status",
    CONSUMER_ID         = "X-Consumer-ID",
    CONSUMER_CUSTOM_ID  = "X-Consumer-Custom-ID",
    CONSUMER_USERNAME   = "X-Consumer-Username",
    CREDENTIAL_IDENTIFIER = "X-Credential-Identifier",
    ANONYMOUS           = "X-Anonymous-Consumer",
    RATELIMIT_LIMIT     = "RateLimit-Limit",
    RATELIMIT_REMAINING = "RateLimit-Remaining",
    RATELIMIT_RESET     = "RateLimit-Reset",
    RETRY_AFTER         = "Retry-After",
    REQUEST_ID          = "X-Kong-Request-Id",
    VIA                 = "Via",
    SERVER              = "Server",
}

_M.PROTOCOLS = {
    "grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp",
}

-- PROTOCOLS_WITH_SUBSYSTEM mapping — 协议子系统映射
_M.PROTOCOLS_WITH_SUBSYSTEM = {
    http = "http",
    https = "http",
    tcp = "stream",
    tls = "stream",
    udp = "stream",
    tls_passthrough = "stream",
    grpc = "http",
    grpcs = "http",
}

-- Core entities — 核心实体
local CORE_ENTITIES = {
    "consumers",
    "certificates",
    "services",
    "routes",
    "snis",
    "upstreams",
    "targets",
    "plugins",
    "tags",
    "ca_certificates",
    "clustering_data_planes",
    "parameters",
    "vaults",
    "filter_chains",
    "workspaces",
}
_M.CORE_ENTITIES = CORE_ENTITIES

-- Also as a set — 同时作为集合使用
for _, v in ipairs(CORE_ENTITIES) do
    CORE_ENTITIES[v] = true
end

-- Bundled plugins — 内置插件
local BUNDLED_PLUGINS = {
    "jwt", "acl", "correlation-id", "cors", "oauth2",
    "tcp-log", "udp-log", "file-log", "http-log",
    "key-auth", "hmac-auth", "basic-auth",
    "ip-restriction", "request-transformer", "response-transformer",
    "request-size-limiting", "rate-limiting", "response-ratelimiting",
    "syslog", "loggly", "datadog", "statsd",
    "bot-detection", "aws-lambda", "request-termination",
    "azure-functions", "zipkin", "pre-function", "post-function",
    "prometheus", "proxy-cache", "session",
    "acme", "grpc-gateway", "grpc-web",
    "opentelemetry",
}
_M.BUNDLED_PLUGINS = {}
for _, v in ipairs(BUNDLED_PLUGINS) do
    _M.BUNDLED_PLUGINS[v] = true
end

-- Rate limiting — 限流
_M.RATELIMIT = {
    PERIODS = { "second", "minute", "hour", "day", "month", "year" },
}

-- Schema defaults — Schema 默认值
_M.DEFAULT_CONTENT_TYPE = "application/json; charset=utf-8"

return _M
