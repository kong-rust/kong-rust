--- spec.internal.module — shim for Kong-Rust test suite
-- Provides reload and reload_helpers for spec compatibility.
-- Kong-Rust 没有 router_flavor 概念，reload_helpers 直接返回 helpers。

local function reload(name)
  package.loaded[name] = nil
  return require(name)
end

local function reload_helpers(flavor)
  -- Kong-Rust does not have router_flavor; just reload helpers — 直接重载 helpers
  local helpers = reload("spec.helpers")
  return helpers
end

return {
  reload = reload,
  reload_helpers = reload_helpers,
}
