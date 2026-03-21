-- error-generator plugin handler — 错误生成插件 handler (仅测试用)
-- Generates errors in specified phases for testing — 在指定阶段生成错误用于测试
-- Priority 1000000 matches Kong OSS test plugin — 优先级 1000000 与 Kong OSS 测试插件一致

local ErrorGeneratorHandler = {
  PRIORITY = 1000000,
  VERSION  = "1.0.0",
}

function ErrorGeneratorHandler:rewrite(conf)
  if conf.rewrite then
    error("this plugin throws an error — 此插件抛出错误")
  end
end

function ErrorGeneratorHandler:access(conf)
  if conf.access then
    error("this plugin throws an error — 此插件抛出错误")
  end
end

function ErrorGeneratorHandler:header_filter(conf)
  if conf.header_filter then
    error("this plugin throws an error — 此插件抛出错误")
  end
end

function ErrorGeneratorHandler:log(conf)
  if conf.log then
    error("this plugin throws an error — 此插件抛出错误")
  end
end

return ErrorGeneratorHandler
