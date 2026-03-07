//! LuaJIT VM 管理
//!
//! 管理 Lua 虚拟机的创建和配置

use kong_core::error::{KongError, Result};
use mlua::prelude::*;

/// 创建一个预配置的 Lua VM
///
/// 预设:
/// - package.path 包含 Kong 标准路径
/// - 注入基础的 ngx 兼容表
pub fn create_lua_vm(kong_prefix: &str) -> Result<Lua> {
    let lua = unsafe { Lua::unsafe_new() };

    // 设置 Kong 标准 package.path
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

/// 在 Lua VM 中注入全局变量
pub fn set_global_string(lua: &Lua, name: &str, value: &str) -> Result<()> {
    let globals = lua.globals();
    globals
        .set(name, value.to_string())
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    Ok(())
}
