//! LuaJIT VM management — LuaJIT VM 管理
//!
//! Manages Lua VM creation and configuration — 管理 Lua 虚拟机的创建和配置

use kong_core::error::{KongError, Result};
use mlua::prelude::*;

/// Create a pre-configured Lua VM — 创建一个预配置的 Lua VM
///
/// Presets: — 预设:
/// - package.path includes Kong standard paths — package.path 包含 Kong 标准路径
/// - Basic ngx compatibility table injected — 注入基础的 ngx 兼容表
pub fn create_lua_vm(kong_prefix: &str) -> Result<Lua> {
    let lua = unsafe { Lua::unsafe_new() };

    // Set Kong standard package.path — 设置 Kong 标准 package.path
    let lua_path = format!(
        "{}/?.lua;{}/?/init.lua;./?.lua;./?/init.lua",
        kong_prefix, kong_prefix
    );
    lua.load(format!(
        "package.path = '{}' .. ';' .. package.path",
        lua_path
    ))
    .exec()
    .map_err(|e| KongError::LuaError(e.to_string()))?;

    Ok(lua)
}

/// Inject a global string variable into the Lua VM — 在 Lua VM 中注入全局变量
pub fn set_global_string(lua: &Lua, name: &str, value: &str) -> Result<()> {
    let globals = lua.globals();
    globals
        .set(name, value.to_string())
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    Ok(())
}
