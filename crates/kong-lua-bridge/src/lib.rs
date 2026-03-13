//! Kong Lua compatibility layer — runs Kong Lua plugins via mlua — Kong Lua 兼容层 — 通过 mlua 运行 Kong 的 Lua 插件
//!
//! Responsibilities: — 职责:
//! - Manage LuaJIT VM pool — 管理 LuaJIT VM 池
//! - Implement Kong PDK (kong.request, kong.response, ...) — 实现 Kong PDK（kong.request, kong.response, ...）
//! - Load and execute Lua plugins — 加载和执行 Lua 插件
//! - Provide ngx.* compatibility layer — 提供 ngx.* 兼容层

pub mod loader;
pub mod pdk;
pub mod runtime;
pub mod vm;

use kong_core::error::{KongError, Result};
use kong_core::traits::{Phase, PluginConfig, PluginHandler, RequestCtx};
use mlua::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

/// Lua plugin handler — wraps the lifecycle of a Lua plugin — Lua 插件 handler — 封装一个 Lua 插件的生命周期
pub struct LuaPluginHandler {
    /// Plugin name — 插件名称
    name: String,
    /// Plugin priority — 插件优先级
    priority: i32,
    /// Plugin version — 插件版本
    version: String,
    /// Lua plugin directory path — Lua 插件目录路径
    plugin_path: PathBuf,
    /// Whether each phase has a corresponding handler — 各阶段是否有对应的 handler
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

        // Execute handler:access(config) in the Lua VM — 在 Lua VM 中执行 handler:access(config)
        execute_lua_phase(&self.name, &self.plugin_path, "access", config, ctx)
    }

    async fn rewrite(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        if !self.phases.get(&Phase::Rewrite).copied().unwrap_or(false) {
            return Ok(());
        }
        execute_lua_phase(&self.name, &self.plugin_path, "rewrite", config, ctx)
    }

    async fn header_filter(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        if !self
            .phases
            .get(&Phase::HeaderFilter)
            .copied()
            .unwrap_or(false)
        {
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

    async fn init_worker(&self, config: &PluginConfig) -> Result<()> {
        if !self
            .phases
            .get(&Phase::InitWorker)
            .copied()
            .unwrap_or(false)
        {
            return Ok(());
        }

        let mut ctx = RequestCtx::default();
        execute_lua_phase(
            &self.name,
            &self.plugin_path,
            "init_worker",
            config,
            &mut ctx,
        )
    }
}

/// Execute a plugin phase in the Lua VM — 在 Lua VM 中执行插件阶段
fn execute_lua_phase(
    plugin_name: &str,
    plugin_path: &PathBuf,
    phase: &str,
    config: &PluginConfig,
    ctx: &mut RequestCtx,
) -> Result<()> {
    let lua = unsafe { Lua::unsafe_new() };

    runtime::configure_package_path(&lua, plugin_path)
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::set_phase(&lua, "init").map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::install(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;

    // Inject PDK (kong global table) — 注入 PDK（kong 全局表）
    pdk::inject_kong_pdk(&lua, ctx).map_err(|e| KongError::LuaError(e.to_string()))?;

    // Inject ngx compatibility layer — 注入 ngx 兼容层
    pdk::inject_ngx_compat(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;

    // Load plugin handler.lua — 加载插件 handler.lua
    let handler_path = plugin_path.join("handler.lua");
    if !handler_path.exists() {
        return Err(KongError::LuaError(format!(
            "handler.lua not found for plugin {}: {} — 插件 {} 的 handler.lua 不存在: {}",
            plugin_name,
            handler_path.display(),
            plugin_name,
            handler_path.display()
        )));
    }

    // Execute handler — 执行 handler
    let handler_code = std::fs::read_to_string(&handler_path).map_err(|e| {
        KongError::LuaError(format!(
            "Failed to read handler.lua: {} — 读取 handler.lua 失败: {}",
            e, e
        ))
    })?;

    let handler_table: LuaTable = lua.load(&handler_code).eval().map_err(|e| {
        KongError::LuaError(format!(
            "Failed to load handler.lua: {} — 加载 handler.lua 失败: {}",
            e, e
        ))
    })?;

    // The current bridge creates a fresh VM per phase call. — 当前桥接会为每次阶段调用创建新的 VM。
    // Bootstrap lifecycle hooks that some Kong plugins expect before the main phase. — 先补跑部分 Kong 插件依赖的生命周期钩子。
    let config_value = lua
        .to_value(&config.config)
        .map_err(|e| KongError::LuaError(e.to_string()))?;

    if phase != "init_worker" {
        let configure_fn: Option<LuaFunction> = handler_table.get("configure").ok();
        if let Some(configure_fn) = configure_fn {
            let configs = lua
                .create_table()
                .map_err(|e| KongError::LuaError(e.to_string()))?;
            configs
                .set(1, config_value.clone())
                .map_err(|e| KongError::LuaError(e.to_string()))?;
            let _ = configure_fn.call::<()>((handler_table.clone(), configs));
        }

        let init_worker_fn: Option<LuaFunction> = handler_table.get("init_worker").ok();
        if let Some(init_worker_fn) = init_worker_fn {
            runtime::set_phase(&lua, "init_worker")
                .map_err(|e| KongError::LuaError(e.to_string()))?;
            let _ = init_worker_fn.call::<()>((handler_table.clone(), config_value.clone()));
            runtime::set_phase(&lua, phase).map_err(|e| KongError::LuaError(e.to_string()))?;
        }
    }

    runtime::set_phase(&lua, phase).map_err(|e| KongError::LuaError(e.to_string()))?;

    // Call handler:phase(config) — 调用 handler:phase(config)
    let phase_fn: Option<LuaFunction> = handler_table.get(phase).ok();

    if let Some(func) = phase_fn {
        func.call::<()>((handler_table, config_value))
            .map_err(|e| {
                KongError::LuaError(format!(
                    "Plugin {} failed in {} phase: {} — 插件 {} {} 阶段执行失败: {}",
                    plugin_name, phase, e, plugin_name, phase, e
                ))
            })?;
    }

    // Sync changes from Lua side back to RequestCtx — 从 Lua 侧同步回 RequestCtx 的变更
    pdk::sync_ctx_from_lua(&lua, ctx).map_err(|e| KongError::LuaError(e.to_string()))?;

    Ok(())
}
