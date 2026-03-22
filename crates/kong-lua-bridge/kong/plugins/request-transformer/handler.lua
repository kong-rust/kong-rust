-- request-transformer plugin handler — 请求转换插件 handler
-- Modifies request headers/body/querystring in the access phase — 在 access 阶段修改请求头/请求体/查询参数
-- Priority 801 matches Kong OSS — 优先级 801 与 Kong OSS 一致

local RequestTransformerHandler = {
  PRIORITY = 801,
  VERSION  = "1.0.0",
}

function RequestTransformerHandler:access(conf)
  -- Remove headers — 删除请求头
  if conf.remove and conf.remove.headers then
    for _, name in ipairs(conf.remove.headers) do
      kong.service.request.clear_header(name)
    end
  end

  -- Rename headers — 重命名请求头
  if conf.rename and conf.rename.headers then
    for _, entry in ipairs(conf.rename.headers) do
      local old_name, new_name = entry:match("^([^:]+):(.+)$")
      if old_name and new_name then
        local value = kong.request.get_header(old_name)
        if value then
          kong.service.request.set_header(new_name, value)
          kong.service.request.clear_header(old_name)
        end
      end
    end
  end

  -- Replace headers — 替换请求头
  if conf.replace and conf.replace.headers then
    for _, entry in ipairs(conf.replace.headers) do
      local name, value = entry:match("^([^:]+):(.+)$")
      if name and value then
        if kong.request.get_header(name) then
          kong.service.request.set_header(name, value)
        end
      end
    end
  end

  -- Add headers — 添加请求头
  if conf.add and conf.add.headers then
    for _, entry in ipairs(conf.add.headers) do
      local name, value = entry:match("^([^:]+):(.+)$")
      if name and value then
        if not kong.request.get_header(name) then
          kong.service.request.set_header(name, value)
        end
      end
    end
  end

  -- Append headers — 追加请求头
  if conf.append and conf.append.headers then
    for _, entry in ipairs(conf.append.headers) do
      local name, value = entry:match("^([^:]+):(.+)$")
      if name and value then
        kong.service.request.add_header(name, value)
      end
    end
  end
end

return RequestTransformerHandler
