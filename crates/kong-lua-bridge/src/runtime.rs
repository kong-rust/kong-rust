use std::path::Path;

use mlua::prelude::*;

const COMPAT_MODULES: &str = r#"
package.preload["string.buffer"] = function()
  local Buffer = {}
  Buffer.__index = Buffer

  function Buffer.new(_hint)
    local self = {
      _parts = {},
      _len = 0,
    }

    return setmetatable(self, {
      __index = Buffer,
      __len = function(obj)
        return obj._len
      end,
    })
  end

  function Buffer:put(value)
    local text = tostring(value or "")
    self._parts[#self._parts + 1] = text
    self._len = self._len + #text
    return self
  end

  function Buffer:putf(fmt, ...)
    return self:put(string.format(fmt, ...))
  end

  function Buffer:get()
    return table.concat(self._parts)
  end

  function Buffer:free()
    self._parts = {}
    self._len = 0
  end

  return {
    new = Buffer.new,
  }
end

package.preload["table.new"] = function()
  return function()
    return {}
  end
end

package.preload["kong.tools.yield"] = function()
  return {
    yield = function()
      return nil
    end,
  }
end

package.preload["resty.counter"] = function()
  local Counter = {}
  Counter.__index = Counter

  function Counter.new(dict_name, sync_interval)
    return setmetatable({
      dict_name = dict_name,
      sync_interval = sync_interval,
      pending = {},
    }, Counter), nil
  end

  function Counter:incr(key, value)
    value = value or 1
    self.pending[key] = (self.pending[key] or 0) + value
  end

  function Counter:sync()
    local dict = ngx.shared[self.dict_name]
    if not dict then
      return nil, "shared dict not found: " .. tostring(self.dict_name)
    end

    for key, value in pairs(self.pending) do
      dict:incr(key, value, 0)
    end

    self.pending = {}
    return true
  end

  return {
    new = Counter.new,
  }
end

package.preload["prometheus_resty_counter"] = function()
  return require("resty.counter")
end

package.preload["kong.meta"] = function()
  return {
    version = (kong and kong.version) or "3.0.0",
  }
end

package.preload["kong.constants"] = function()
  return {
    CLUSTERING_SYNC_STATUS = {
      KONG_VERSION_INCOMPATIBLE = "KONG_VERSION_INCOMPATIBLE",
      PLUGIN_SET_INCOMPATIBLE = "PLUGIN_SET_INCOMPATIBLE",
      PLUGIN_VERSION_INCOMPATIBLE = "PLUGIN_VERSION_INCOMPATIBLE",
    },
  }
end

package.preload["kong.runloop.balancer"] = function()
  return {
    get_all_upstreams = function()
      return {}
    end,
    get_upstream_health = function()
      return nil
    end,
  }
end

package.preload["kong.runloop.wasm"] = function()
  local enabled = false

  return {
    set_enabled = function(value)
      enabled = not not value
    end,
    metrics_data = function()
      return enabled
    end,
  }
end

package.preload["kong.db.schema.typedefs"] = function()
  return {
    protocols = {
      type = "set",
      elements = {
        type = "string",
      },
    },
  }
end
"#;

pub fn install(lua: &Lua) -> LuaResult<()> {
    lua.load(COMPAT_MODULES).exec()
}

pub fn set_phase(lua: &Lua, phase: &str) -> LuaResult<()> {
    lua.globals().set("__kong_phase", phase)
}

pub fn configure_package_path(lua: &Lua, plugin_path: &Path) -> LuaResult<()> {
    let mut entries = Vec::new();

    for ancestor in plugin_path.ancestors().take(6) {
        let value = ancestor.display().to_string().replace('\\', "\\\\");
        entries.push(format!("{value}/?.lua"));
        entries.push(format!("{value}/?/init.lua"));
    }

    let package_path = entries.join(";");
    lua.load(format!(
        "package.path = {:?} .. ';' .. package.path",
        package_path
    ))
    .exec()
}
