-- response-transformer plugin schema — 响应转换插件 schema
local header_list = { type = "array", default = {}, elements = { type = "string" } }

return {
  name = "response-transformer",
  fields = {
    { protocols = { type = "set", elements = { type = "string", one_of = { "grpc", "grpcs", "http", "https" } }, default = { "grpc", "grpcs", "http", "https" } } },
    { config = {
        type = "record",
        fields = {
          { remove = {
              type = "record",
              fields = {
                { json   = header_list },
                { headers = header_list },
              },
            },
          },
          { rename = {
              type = "record",
              fields = {
                { json   = header_list },
                { headers = header_list },
              },
            },
          },
          { replace = {
              type = "record",
              fields = {
                { json   = header_list },
                { headers = header_list },
                { json_types = header_list },
              },
            },
          },
          { add = {
              type = "record",
              fields = {
                { json   = header_list },
                { headers = header_list },
                { json_types = header_list },
              },
            },
          },
          { append = {
              type = "record",
              fields = {
                { json   = header_list },
                { headers = header_list },
                { json_types = header_list },
              },
            },
          },
        },
      },
    },
  },
}
