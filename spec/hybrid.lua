------------------------------------------------------------------
-- Hybrid mode test runner for Kong-Rust — Kong-Rust 混合模式测试运行器
--
-- Supports both "traditional" and "hybrid" deployment modes.
-- 支持 "traditional" 和 "hybrid" 两种部署模式。
--
-- In hybrid mode, iterates over 3 RPC combinations:
--   (off,off), (on,off), (on,on)
-- 混合模式下遍历 3 种 RPC 组合。
--
-- @copyright Copyright 2024 Kong-Rust Authors
-- @module spec.hybrid
------------------------------------------------------------------

local helpers = require("spec.helpers")
local assert = require("luassert")

--- Build a patched helpers module for a specific deployment combination
-- 为特定部署组合构建 patched helpers 模块
local function get_patched_helpers(strategy, deploy, rpc, rpc_sync, opts)
  local _M = {}
  local _prefix = nil

  _M.data_plane = nil
  _M.control_plane = nil

  --- Start Kong instance(s) — 启动 Kong 实例
  -- In traditional mode: starts a single instance.
  -- In hybrid mode: starts CP + DP with clustering certs.
  function _M.start_kong(env, tables, preserve_prefix, fixtures)
    local ret, v
    if deploy == "traditional" then
      ret, v = helpers.start_kong(env, tables, preserve_prefix, fixtures)
      if ret then
        _prefix = env and env.prefix or "servroot"
      end
      _M.data_plane = helpers.get_running_conf(_prefix)

    else
      -- Hybrid mode — 混合模式
      local hybrid_envs = {
        cluster_cert = "spec/fixtures/kong_clustering.crt",
        cluster_cert_key = "spec/fixtures/kong_clustering.key",
        lua_ssl_trusted_certificate = "spec/fixtures/kong_clustering.crt",
        cluster_rpc = rpc,
        cluster_rpc_sync = rpc_sync,
        prefix = "",
        role = "",
        cluster_listen = "",
        cluster_telemetry_listen = "",
        cluster_control_plane = "",
        cluster_telemetry_endpoint = "",
      }

      -- Validate: user must not specify hybrid-specific keys — 校验：用户不可指定混合模式专用参数
      for k, _ in pairs(hybrid_envs) do
        assert.is_nil(env[k], "can't specify " .. k .. " in env of start_kong in hybrid mode")
      end

      if env.database then
        assert.equal(env.database, strategy, "database must be the same as strategy in hybrid mode")
      end

      -- Merge user env with hybrid defaults — 合并用户 env 与混合模式默认值
      local cp_envs = {}
      for k, v2 in pairs(env) do cp_envs[k] = v2 end
      for k, v2 in pairs(hybrid_envs) do cp_envs[k] = v2 end

      -- Control Plane config — 控制面配置
      cp_envs.database = strategy
      cp_envs.role = "control_plane"
      cp_envs.prefix = "servroot"
      cp_envs.cluster_listen = "127.0.0.1:9005"
      cp_envs.cluster_telemetry_listen = "127.0.0.1:9006"
      cp_envs.cluster_control_plane = nil
      cp_envs.cluster_telemetry_endpoint = nil

      assert(helpers.start_kong(cp_envs, tables, preserve_prefix, fixtures))

      -- Data Plane config — 数据面配置
      local dp_envs = {}
      for k, v2 in pairs(env) do dp_envs[k] = v2 end
      for k, v2 in pairs(hybrid_envs) do dp_envs[k] = v2 end

      dp_envs.database = "off"
      dp_envs.role = "data_plane"
      dp_envs.prefix = "servroot2"
      dp_envs.cluster_control_plane = "127.0.0.1:9005"
      dp_envs.cluster_telemetry_endpoint = "127.0.0.1:9006"
      dp_envs.cluster_listen = nil
      dp_envs.cluster_telemetry_listen = nil

      assert(helpers.start_kong(dp_envs, nil, preserve_prefix, nil))

      _M.control_plane = helpers.get_running_conf("servroot")
      _M.data_plane = helpers.get_running_conf("servroot2")

      if rpc_sync == "on" and not (opts and opts.dont_wait_full_sync) then
        -- Wait for full sync to complete — 等待全量同步完成
        assert.logfile(_M.data_plane.nginx_err_logs).has.line("[kong.sync.v2] full sync ends", true, 10)
      end

      ret = true
    end

    if strategy ~= "off" then
      _M.wait_for_all_config_update()
    end

    return ret, v
  end

  --- Stop Kong instance(s) — 停止 Kong 实例
  function _M.stop_kong(prefix, preserve_prefix, preserve_dc, signal, nowait)
    local ret, v

    if deploy == "hybrid" then
      assert.is_nil(prefix, "can't specify prefix in hybrid mode")
      ret = helpers.stop_kong("servroot", preserve_prefix, preserve_dc, signal, nowait)
      if ret then
        ret = helpers.stop_kong("servroot2", preserve_prefix, preserve_dc, signal, nowait)
      end
    else
      ret, v = helpers.stop_kong(prefix, preserve_prefix, preserve_dc, signal, nowait)
    end

    _prefix = nil
    _M.control_plane = nil
    _M.data_plane = nil

    return ret, v
  end

  --- Clean log file(s) — 清理日志文件
  function _M.clean_logfile(logfile)
    if not logfile and deploy == "hybrid" then
      helpers.clean_logfile(helpers.get_running_conf("servroot").nginx_err_logs)
      helpers.clean_logfile(helpers.get_running_conf("servroot2").nginx_err_logs)
    else
      helpers.clean_logfile(logfile)
    end
  end

  --- Get prefix for a given role — 获取指定角色的前缀
  function _M.get_prefix_for(role)
    if deploy == "hybrid" then
      role = role or "data_plane"
      if role == "data_plane" then
        return "servroot2"
      else
        return "servroot"
      end
    else
      return _prefix
    end
  end

  --- Wait for config propagation — 等待配置传播
  function _M.wait_for_all_config_update(wait_opts)
    if strategy ~= "off" then
      local copied_opts = {}
      if wait_opts then
        for k, v2 in pairs(wait_opts) do copied_opts[k] = v2 end
      end
      helpers.wait_for_all_config_update(copied_opts)
    end
  end

  --- Format tags string for test descriptions — 格式化测试描述标签
  function _M.format_tags()
    local tags = "#" .. strategy .. " #" .. deploy
    if rpc == "on" then
      tags = tags .. " #rpc"
    end
    if rpc_sync == "on" then
      tags = tags .. " #rpc_sync"
    end
    return tags
  end

  --- Reload helpers with same parameters — 以相同参数重新加载 helpers
  function _M.reload_helpers()
    return get_patched_helpers(strategy, deploy, rpc, rpc_sync, opts)
  end

  return setmetatable(_M, { __index = helpers })
end


--- Run tests in different deployment topologies — 在不同部署拓扑下运行测试
--
-- Iterates over strategies x deploys x RPC combinations:
-- 遍历 策略 x 部署模式 x RPC 组合:
--   - strategy: from each_strategy() (e.g. "postgres", "off")
--   - deploy: "traditional", "hybrid"
--   - rpc: {"off","off"}, {"on","off"}, {"on","on"}
--
-- Skips invalid combinations:
-- 跳过无效组合:
--   - DB-less + hybrid
--   - traditional + rpc!=off (redundant)
--
-- @param opts (optional table) options — 可选配置表
--   - dont_wait_full_sync: skip waiting for sync — 跳过同步等待
--   - strategies_iterator: custom iterator — 自定义策略迭代器
-- @param fn (function) test body receiving (helpers, strategy, deploy, rpc, rpc_sync)
local function run_for_each_deploy(opts, fn)
  opts = opts or {}
  local strategies_iterator = opts.strategies_iterator or helpers.each_strategy

  for _, strategy in strategies_iterator() do
  for _, deploy in ipairs({ "traditional", "hybrid" }) do
  for _, v in ipairs({ {"off", "off"}, {"on", "off"}, {"on", "on"} }) do
    local rpc, rpc_sync = v[1], v[2]

    if strategy == "off" then
      if deploy == "hybrid" then
        -- DB-less mode doesn't support hybrid — DB-less 不支持混合模式
        goto continue
      end
      if not (rpc == "off" and rpc_sync == "off") then
        -- Only test DB-less once — DB-less 只测一次
        goto continue
      end
    end

    if deploy == "traditional" and not (rpc == "off" and rpc_sync == "off") then
      -- Only test traditional once — 传统模式只测一次
      goto continue
    end

    local patched = get_patched_helpers(strategy, deploy, rpc, rpc_sync, opts)

    -- Test body — 测试主体
    fn(patched, strategy, deploy, rpc, rpc_sync)

    ::continue::
  end -- rpc combinations
  end -- deploy modes
  end -- strategies
end

return {
  run_for_each_deploy = run_for_each_deploy,
}
