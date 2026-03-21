--- spec.internal.sys — shim for Kong-Rust test suite
-- Provides setenv/unsetenv via FFI.
-- 通过 FFI 提供环境变量操作。

local ffi = require("ffi")

ffi.cdef [[
  int setenv(const char *name, const char *value, int overwrite);
  int unsetenv(const char *name);
]]

local function setenv(env, value)
  assert(type(env) == "string", "env must be a string")
  assert(type(value) == "string", "value must be a string")
  return ffi.C.setenv(env, value, 1) == 0
end

local function unsetenv(env)
  assert(type(env) == "string", "env must be a string")
  return ffi.C.unsetenv(env) == 0
end

return {
  setenv = setenv,
  unsetenv = unsetenv,
}
