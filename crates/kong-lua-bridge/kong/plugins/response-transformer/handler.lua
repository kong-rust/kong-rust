-- response-transformer plugin handler — 响应转换插件 handler
-- Modifies response headers in the header_filter phase — 在 header_filter 阶段修改响应头
-- Priority 800 matches Kong OSS — 优先级 800 与 Kong OSS 一致

local ResponseTransformerHandler = {
  PRIORITY = 800,
  VERSION  = "1.0.0",
}

function ResponseTransformerHandler:header_filter(conf)
  -- Remove headers — 删除响应头
  if conf.remove and conf.remove.headers then
    for _, name in ipairs(conf.remove.headers) do
      kong.response.clear_header(name)
    end
  end

  -- Replace headers — 替换响应头
  if conf.replace and conf.replace.headers then
    for _, entry in ipairs(conf.replace.headers) do
      local name, value = entry:match("^([^:]+):(.+)$")
      if name and value then
        kong.response.set_header(name, value)
      end
    end
  end

  -- Add headers — 添加响应头
  if conf.add and conf.add.headers then
    for _, entry in ipairs(conf.add.headers) do
      local name, value = entry:match("^([^:]+):(.+)$")
      if name and value then
        kong.response.set_header(name, value)
      end
    end
  end

  -- Append headers — 追加响应头
  if conf.append and conf.append.headers then
    for _, entry in ipairs(conf.append.headers) do
      local name, value = entry:match("^([^:]+):(.+)$")
      if name and value then
        kong.response.add_header(name, value)
      end
    end
  end
end

return ResponseTransformerHandler
