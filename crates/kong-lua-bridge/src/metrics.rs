use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use kong_config::KongConfig;
use kong_core::error::{KongError, Result};
use kong_core::traits::RequestCtx;
use mlua::prelude::{LuaFunction, LuaTable};
use mlua::LuaSerdeExt;

use crate::{loader, pdk, runtime};

static HTTP_TOTAL_REQUESTS: AtomicU64 = AtomicU64::new(0);

/// Record a completed HTTP request for Kong runtime statistics. — 为 Kong 运行时统计记录一次已完成的 HTTP 请求。
pub fn record_http_request() {
    HTTP_TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
}

/// Return the accumulated HTTP request count exposed via kong.nginx.get_statistics(). — 返回通过 kong.nginx.get_statistics() 暴露的累计 HTTP 请求数。
pub fn http_total_requests() -> u64 {
    HTTP_TOTAL_REQUESTS.load(Ordering::Relaxed)
}

/// Render Prometheus metrics through the transplanted official plugin runtime. — 通过移植后的官方插件运行时渲染 Prometheus 指标文本。
pub fn collect_prometheus_metrics(
    config: &KongConfig,
    plugin_configs: &[serde_json::Value],
) -> Result<String> {
    let plugin_path = resolve_prometheus_plugin_path(config)?;
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::default();

    runtime::configure_package_path(&lua, &plugin_path)
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::install(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::set_phase(&lua, "init").map_err(|e| KongError::LuaError(e.to_string()))?;
    pdk::inject_kong_pdk(&lua, &mut ctx).map_err(|e| KongError::LuaError(e.to_string()))?;
    pdk::inject_ngx_compat(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;

    let kong: LuaTable = lua
        .globals()
        .get("kong")
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    let configuration: LuaTable = kong
        .get("configuration")
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    configuration
        .set("role", config.role.clone())
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    configuration
        .set(
            "stream_listeners",
            lua.to_value(&config.stream_listen)
                .map_err(|e| KongError::LuaError(e.to_string()))?,
        )
        .map_err(|e| KongError::LuaError(e.to_string()))?;

    lua.load(
        r#"
        local exporter = require("kong.plugins.prometheus.exporter")
        exporter.init()
    "#,
    )
    .exec()
    .map_err(|e| KongError::LuaError(e.to_string()))?;

    runtime::set_phase(&lua, "init_worker").map_err(|e| KongError::LuaError(e.to_string()))?;
    lua.load(
        r#"
        local exporter = require("kong.plugins.prometheus.exporter")
        exporter.init_worker()
    "#,
    )
    .exec()
    .map_err(|e| KongError::LuaError(e.to_string()))?;

    runtime::set_phase(&lua, "configure").map_err(|e| KongError::LuaError(e.to_string()))?;
    let configs = lua
        .create_table()
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    for (index, plugin_config) in plugin_configs.iter().enumerate() {
        configs
            .set(
                index + 1,
                lua.to_value(plugin_config)
                    .map_err(|e| KongError::LuaError(e.to_string()))?,
            )
            .map_err(|e| KongError::LuaError(e.to_string()))?;
    }
    let exporter: LuaTable = lua
        .load(r#"return require("kong.plugins.prometheus.exporter")"#)
        .eval()
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    let configure: LuaFunction = exporter
        .get("configure")
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    configure
        .call::<()>(configs)
        .map_err(|e| KongError::LuaError(e.to_string()))?;

    runtime::set_phase(&lua, "content").map_err(|e| KongError::LuaError(e.to_string()))?;
    lua.load(
        r#"
        local exporter = require("kong.plugins.prometheus.exporter")
        local chunks = {}
        exporter.metric_data(function(chunk)
          chunks[#chunks + 1] = chunk
        end)
        return table.concat(chunks)
    "#,
    )
    .eval()
    .map_err(|e| KongError::LuaError(e.to_string()))
}

fn resolve_prometheus_plugin_path(config: &KongConfig) -> Result<PathBuf> {
    let plugin_dirs = loader::resolve_plugin_dirs(&config.prefix);
    plugin_dirs
        .into_iter()
        .map(|dir| dir.join("prometheus"))
        .find(|path| path.join("handler.lua").exists())
        .ok_or_else(|| KongError::NotFound {
            entity_type: "plugin".to_string(),
            id: "prometheus".to_string(),
        })
}
