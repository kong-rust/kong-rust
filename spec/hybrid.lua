--- spec.hybrid — shim for Kong-Rust test suite
-- Kong-Rust 不支持 hybrid 部署模式，此 shim 仅运行 traditional 模式。

local helpers = require("spec.helpers")

local function run_for_each_deploy(opts, fn)
  opts = opts or {}
  local strategies_iterator = opts.strategies_iterator or helpers.each_strategy

  for _, strategy in strategies_iterator() do
    -- Kong-Rust: only traditional, rpc=off, rpc_sync=off — 仅传统模式
    local deploy = "traditional"
    local rpc, rpc_sync = "off", "off"

    -- Create a thin wrapper around helpers with extra methods — 创建 helpers 包装
    local patched = {}

    function patched.format_tags()
      return "#" .. strategy .. " #" .. deploy
    end

    function patched.wait_for_all_config_update(wait_opts)
      if strategy ~= "off" then
        helpers.wait_for_all_config_update(wait_opts)
      end
    end

    function patched.reload_helpers()
      return patched
    end

    setmetatable(patched, { __index = helpers })

    fn(patched, strategy, deploy, rpc, rpc_sync)
  end
end

return {
  run_for_each_deploy = run_for_each_deploy,
}
