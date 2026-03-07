//! Kong Lua 兼容层 — 通过 mlua 运行 Kong 的 Lua 插件
//!
//! 职责:
//! - 管理 LuaJIT VM 池
//! - 实现 Kong PDK（kong.request, kong.response, ...）
//! - 加载和执行 Lua 插件
//! - 提供 ngx.* 兼容层

pub mod pdk;
pub mod vm;
pub mod loader;

use std::collections::HashMap;
use std::path::PathBuf;
use kong_core::error::{KongError, Result};
use kong_core::traits::{Phase, PluginConfig, PluginHandler, RequestCtx};
use mlua::prelude::*;

/// Lua 插件 handler — 封装一个 Lua 插件的生命周期
pub struct LuaPluginHandler {
    /// 插件名称
    name: String,
    /// 插件优先级
    priority: i32,
    /// 插件版本
    version: String,
    /// Lua 插件目录路径
    plugin_path: PathBuf,
    /// 各阶段是否有对应的 handler
    phases: HashMap<Phase, bool>,
}

impl LuaPluginHandler {
    pub fn new(
        name: String,
        priority: i32,
        version: String,
        plugin_path: PathBuf,
        phases: HashMap<Phase, bool>,
    ) -> Self {
        Self {
            name,
            priority,
            version,
            plugin_path,
            phases,
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for LuaPluginHandler {
    fn priority(&self) -> i32 {
        self.priority
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        if !self.phases.get(&Phase::Access).copied().unwrap_or(false) {
            return Ok(());
        }

        // 在 Lua VM 中执行 handler:access(config)
        execute_lua_phase(&self.name, &self.plugin_path, "access", config, ctx)
    }

    async fn rewrite(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        if !self.phases.get(&Phase::Rewrite).copied().unwrap_or(false) {
            return Ok(());
        }
        execute_lua_phase(&self.name, &self.plugin_path, "rewrite", config, ctx)
    }

    async fn header_filter(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        if !self.phases.get(&Phase::HeaderFilter).copied().unwrap_or(false) {
            return Ok(());
        }
        execute_lua_phase(&self.name, &self.plugin_path, "header_filter", config, ctx)
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        if !self.phases.get(&Phase::Log).copied().unwrap_or(false) {
            return Ok(());
        }
        execute_lua_phase(&self.name, &self.plugin_path, "log", config, ctx)
    }
}

/// 在 Lua VM 中执行插件阶段
fn execute_lua_phase(
    plugin_name: &str,
    plugin_path: &PathBuf,
    phase: &str,
    config: &PluginConfig,
    ctx: &mut RequestCtx,
) -> Result<()> {
    let lua = unsafe { Lua::unsafe_new() };

    // 设置 package.path 包含插件目录
    let plugin_dir = plugin_path.parent().unwrap_or(plugin_path.as_path());
    let package_path = format!(
        "{}/?.lua;{}/?.lua",
        plugin_dir.display(),
        plugin_path.display()
    );

    lua.load(format!(
        "package.path = '{}' .. ';' .. package.path",
        package_path
    ))
    .exec()
    .map_err(|e| KongError::LuaError(e.to_string()))?;

    // 注入 PDK（kong 全局表）
    pdk::inject_kong_pdk(&lua, ctx)
        .map_err(|e| KongError::LuaError(e.to_string()))?;

    // 注入 ngx 兼容层
    pdk::inject_ngx_compat(&lua)
        .map_err(|e| KongError::LuaError(e.to_string()))?;

    // 加载插件 handler.lua
    let handler_path = plugin_path.join("handler.lua");
    if !handler_path.exists() {
        return Err(KongError::LuaError(format!(
            "插件 {} 的 handler.lua 不存在: {}",
            plugin_name,
            handler_path.display()
        )));
    }

    // 执行 handler
    let handler_code = std::fs::read_to_string(&handler_path)
        .map_err(|e| KongError::LuaError(format!("读取 handler.lua 失败: {}", e)))?;

    let handler_table: LuaTable = lua
        .load(&handler_code)
        .eval()
        .map_err(|e| KongError::LuaError(format!("加载 handler.lua 失败: {}", e)))?;

    // 调用 handler:phase(config)
    let phase_fn: Option<LuaFunction> = handler_table
        .get(phase)
        .ok();

    if let Some(func) = phase_fn {
        let config_value = lua
            .to_value(&config.config)
            .map_err(|e| KongError::LuaError(e.to_string()))?;

        func.call::<()>((handler_table, config_value))
            .map_err(|e| KongError::LuaError(format!(
                "插件 {} {} 阶段执行失败: {}",
                plugin_name, phase, e
            )))?;
    }

    // 从 Lua 侧同步回 RequestCtx 的变更
    pdk::sync_ctx_from_lua(&lua, ctx)
        .map_err(|e| KongError::LuaError(e.to_string()))?;

    Ok(())
}
