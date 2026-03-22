-- error-generator plugin schema — 错误生成插件 schema (仅测试用)
return {
  name = "error-generator",
  fields = {
    { protocols = { type = "set", elements = { type = "string", one_of = { "grpc", "grpcs", "http", "https" } }, default = { "grpc", "grpcs", "http", "https" } } },
    { config = {
        type = "record",
        fields = {
          { rewrite = { type = "boolean", default = false } },
          { access = { type = "boolean", default = false } },
          { header_filter = { type = "boolean", default = false } },
          { log = { type = "boolean", default = false } },
        },
      },
    },
  },
}
