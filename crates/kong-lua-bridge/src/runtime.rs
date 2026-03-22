use std::path::Path;

use mlua::prelude::*;
use mlua::LuaSerdeExt;

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

do
  local raw_pairs = pairs
  local raw_ipairs = ipairs

  pairs = function(value)
    local mt = type(value) == "table" and getmetatable(value) or nil
    if mt and type(mt.__pairs) == "function" then
      return mt.__pairs(value)
    end
    return raw_pairs(value)
  end

  ipairs = function(value)
    local mt = type(value) == "table" and getmetatable(value) or nil
    if mt and type(mt.__ipairs) == "function" then
      return mt.__ipairs(value)
    end
    return raw_ipairs(value)
  end
end

package.preload["table.new"] = function()
  return function()
    return {}
  end
end

package.preload["table.clone"] = function()
  return function(t)
    if type(t) ~= "table" then
      return t
    end

    local copy = {}
    for k, v in pairs(t) do
      copy[k] = v
    end
    return copy
  end
end

package.preload["table.isarray"] = function()
  return function(t)
    if type(t) ~= "table" then
      return false
    end

    local count = 0
    for k in pairs(t) do
      if type(k) ~= "number" or k < 1 or math.floor(k) ~= k then
        return false
      end
      count = count + 1
    end

    for i = 1, count do
      if t[i] == nil then
        return false
      end
    end

    return true
  end
end

package.preload["pl.tablex"] = function()
  return {
    readonly = function(t)
      return setmetatable({}, {
        __index = t,
        __newindex = function()
          error("attempt to modify readonly table")
        end,
        __pairs = function()
          return next, t, nil
        end,
      })
    end,
  }
end

package.preload["pl.stringx"] = function()
  local function split(input, sep)
    sep = sep or "%s+"
    local text = tostring(input or "")
    local result = {}

    if sep == "" then
      for i = 1, #text do
        result[#result + 1] = text:sub(i, i)
      end
      return result
    end

    if sep == "%s+" then
      for part in text:gmatch("%S+") do
        result[#result + 1] = part
      end
      return result
    end

    local start = 1
    while true do
      local first, last = text:find(sep, start, true)
      if not first then
        result[#result + 1] = text:sub(start)
        break
      end
      result[#result + 1] = text:sub(start, first - 1)
      start = last + 1
    end

    return result
  end

  return {
    split = split,
  }
end

package.preload["cjson.safe"] = function()
  return {}
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

    -- Flush immediately in the compatibility runtime because each phase uses a
    -- fresh Lua VM and there is no real ngx timer loop to sync per-worker
    -- counters later. — 在兼容运行时中立即刷入共享字典，因为每个阶段都会使用新的 Lua VM，
    -- 并且当前没有真实的 ngx 定时器循环去稍后同步 worker 本地计数器。
    local dict = ngx.shared[self.dict_name]
    if not dict then
      return nil, "shared dict not found: " .. tostring(self.dict_name)
    end

    dict:incr(key, value, 0)
    self.pending[key] = nil
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
  local ver = (kong and kong.version) or "3.10.0"
  return {
    version = ver,
    _VERSION = ver,
    _VERSION_TABLE = { major = 3, minor = 10, patch = 0 },
    _SERVER_TOKENS = "kong/" .. ver,
  }
end

package.preload["kong.constants"] = function()
  return {
    CLUSTERING_SYNC_STATUS = {
      KONG_VERSION_INCOMPATIBLE = "KONG_VERSION_INCOMPATIBLE",
      PLUGIN_SET_INCOMPATIBLE = "PLUGIN_SET_INCOMPATIBLE",
      PLUGIN_VERSION_INCOMPATIBLE = "PLUGIN_VERSION_INCOMPATIBLE",
    },
    HEADERS = {},
  }
end

package.preload["kong.tools.table"] = function()
  local empty = require("pl.tablex").readonly({})

    return {
      EMPTY = empty,
      deep_copy = function(value)
      if type(value) ~= "table" then
        return value
      end

      local copy = {}
      for k, v in pairs(value) do
        copy[k] = type(v) == "table" and require("kong.tools.table").deep_copy(v) or v
      end
      return copy
    end,
    cycle_aware_deep_copy = function(value)
      return require("kong.tools.table").deep_copy(value)
    end,
    table_contains = function(arr, needle)
      if type(arr) ~= "table" then
        return false
      end

      for _, value in pairs(arr) do
        if value == needle then
          return true
        end
      end
      return false
    end,
  }
end

package.preload["kong.tools.string"] = function()
  local function split_impl(input, sep, limit)
    sep = sep or ","
    local result = {}
    local pattern = string.format("([^%s]+)", sep:gsub("%%", "%%%%"))

    for chunk in tostring(input):gmatch(pattern) do
      result[#result + 1] = chunk
      if limit and #result >= limit then
        break
      end
    end

    return result
  end

  return {
    split = function(input, sep)
      return split_impl(input, sep)
    end,
    splitn = function(input, sep, count)
      return split_impl(input, sep, count)
    end,
  }
end

package.preload["kong.tools.gzip"] = function()
  return {
    deflate_gzip = function(value)
      return value
    end,
  }
end

package.preload["socket.url"] = function()
  return {
    parse = function(url)
      local scheme, authority, path = tostring(url):match("^(https?)://([^/]+)(.*)$")
      if not scheme then
        return nil
      end

      local host, port = authority:match("^([^:]+):?(%d*)$")
      return {
        scheme = scheme,
        authority = authority,
        host = host,
        port = port ~= "" and port or nil,
        path = path ~= "" and path or "/",
      }
    end,
  }
end

package.preload["resty.http"] = function()
  return {
    new = function()
      return {
        set_timeout = function() end,
        request_uri = function()
          return nil, "resty.http request_uri is not implemented in compat runtime"
        end,
      }
    end,
  }
end

package.preload["resty.gcp.request.credentials.accesstoken"] = function()
  return function()
    return nil, "resty.gcp credentials are not implemented in compat runtime"
  end
end

package.preload["resty.aws.config"] = function()
  return {}
end

package.preload["resty.aws"] = function()
  return function()
    return nil, "resty.aws is not implemented in compat runtime"
  end
end

package.preload["kong.tools.aws_stream"] = function()
  return {
    decode = function()
      return nil, "aws stream decode is not implemented in compat runtime"
    end,
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
    protocols_http = {
      type = "set",
      elements = {
        type = "string",
      },
      default = { "http", "https", "grpc", "grpcs" },
    },
    url = function(definition)
      definition = definition or {}
      definition.type = "string"
      return definition
    end,
  }
end
"#;

pub fn install(lua: &Lua) -> LuaResult<()> {
    lua.load(COMPAT_MODULES).exec()?;
    install_cjson_safe(lua)
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

fn install_cjson_safe(lua: &Lua) -> LuaResult<()> {
    let module = lua.create_table()?;
    module.set(
        "encode",
        lua.create_function(|lua, value: LuaValue| -> LuaResult<(String, LuaValue)> {
            let json: serde_json::Value = lua.from_value(value)?;
            let encoded = serde_json::to_string(&json).map_err(LuaError::external)?;
            Ok((encoded, LuaValue::Nil))
        })?,
    )?;
    module.set(
        "decode",
        lua.create_function(|lua, value: String| -> LuaResult<(LuaValue, LuaValue)> {
            match serde_json::from_str::<serde_json::Value>(&value) {
                Ok(json) => Ok((lua.to_value(&json)?, LuaValue::Nil)),
                Err(err) => Ok((LuaValue::Nil, LuaValue::String(lua.create_string(err.to_string())?))),
            }
        })?,
    )?;

    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    for module_name in ["cjson", "cjson.safe"] {
        let module_for_require = module.clone();
        preload.set(
            module_name,
            lua.create_function(move |_, _: ()| Ok(module_for_require.clone()))?,
        )?;
    }

    Ok(())
}
