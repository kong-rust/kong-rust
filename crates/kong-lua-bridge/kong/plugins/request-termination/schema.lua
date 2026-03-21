-- request-termination plugin schema — 请求终止插件 schema
return {
  name = "request-termination",
  fields = {
    { protocols = { type = "set", elements = { type = "string", one_of = { "grpc", "grpcs", "http", "https" } }, default = { "grpc", "grpcs", "http", "https" } } },
    { config = {
        type = "record",
        fields = {
          { status_code = { type = "integer", default = 503, between = { 100, 599 } } },
          { message = { type = "string" } },
          { body = { type = "string" } },
          { content_type = { type = "string", default = "application/json; charset=utf-8" } },
          { trigger = { type = "string" } },
          { echo = { type = "boolean", required = true, default = false } },
        },
      },
    },
  },
}
