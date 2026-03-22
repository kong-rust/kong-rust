-- request-transformer plugin schema — 请求转换插件 schema
local string_list = { type = "array", default = {}, elements = { type = "string" } }

return {
  name = "request-transformer",
  fields = {
    { protocols = { type = "set", elements = { type = "string", one_of = { "grpc", "grpcs", "http", "https" } }, default = { "grpc", "grpcs", "http", "https" } } },
    { config = {
        type = "record",
        fields = {
          { http_method = { type = "string" } },
          { remove = {
              type = "record",
              fields = {
                { body = string_list },
                { headers = string_list },
                { querystring = string_list },
              },
            },
          },
          { rename = {
              type = "record",
              fields = {
                { body = string_list },
                { headers = string_list },
                { querystring = string_list },
              },
            },
          },
          { replace = {
              type = "record",
              fields = {
                { body = string_list },
                { headers = string_list },
                { querystring = string_list },
                { uri = { type = "string" } },
              },
            },
          },
          { add = {
              type = "record",
              fields = {
                { body = string_list },
                { headers = string_list },
                { querystring = string_list },
              },
            },
          },
          { append = {
              type = "record",
              fields = {
                { body = string_list },
                { headers = string_list },
                { querystring = string_list },
              },
            },
          },
        },
      },
    },
  },
}
