use std::path::PathBuf;

use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use kong_lua_bridge::{loader, runtime};
use serde_json::json;

fn prometheus_plugin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("kong/plugins/prometheus")
}

fn setup_prometheus_lua(lua: &mlua::Lua, ctx: &mut RequestCtx, phase: &str) {
    runtime::configure_package_path(lua, &prometheus_plugin_path()).unwrap();
    runtime::set_phase(lua, phase).unwrap();
    kong_lua_bridge::pdk::inject_kong_pdk(lua, ctx).unwrap();
    kong_lua_bridge::pdk::inject_ngx_compat(lua).unwrap();
}

#[test]
fn test_prometheus_plugin_is_loadable_from_transplanted_copy() {
    let plugin_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("kong/plugins");
    let handlers = loader::load_lua_plugins(&[plugin_root], &["prometheus".to_string()]).unwrap();

    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].name(), "prometheus");
    assert_eq!(handlers[0].priority(), 13);
    assert!(!handlers[0].version().is_empty());
}

#[test]
fn test_prometheus_unit_metric_names() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    setup_prometheus_lua(&lua, &mut ctx, "init_worker");

    let result: (bool, bool, bool, bool, bool, bool, bool, bool) = lua
        .load(
            r#"
            local prom = require("kong.plugins.prometheus.prometheus").init("prometheus_metrics", "kong_")

            return
              prom:counter("mem_used") ~= nil,
              prom:counter("Mem_Used") ~= nil,
              prom:counter(":mem_used") ~= nil,
              prom:counter("mem_used:") ~= nil,
              prom:counter("_mem_used_") ~= nil,
              prom:counter("mem-used") == nil,
              prom:counter("0name") == nil,
              prom:counter("name$") == nil
        "#,
        )
        .eval()
        .unwrap();

    assert_eq!(result, (true, true, true, true, true, true, true, true));
}

#[test]
fn test_prometheus_unit_metric_label_names() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    setup_prometheus_lua(&lua, &mut ctx, "init_worker");

    let result: (bool, bool, bool, bool, bool, bool, bool, bool) = lua
        .load(
            r#"
            local prom = require("kong.plugins.prometheus.prometheus").init("prometheus_metrics", "kong_")

            return
              prom:counter("mem0", nil, {"LUA"}) ~= nil,
              prom:counter("mem1", nil, {"lua"}) ~= nil,
              prom:counter("mem2", nil, {"_lua_"}) ~= nil,
              prom:counter("mem3", nil, {":lua"}) == nil,
              prom:counter("mem4", nil, {"0lua"}) == nil,
              prom:counter("mem5", nil, {"lua*"}) == nil,
              prom:counter("mem6", nil, {"lua\\5.1"}) == nil,
              prom:counter("mem8", nil, {"lua-vm"}) == nil
        "#,
        )
        .eval()
        .unwrap();

    assert_eq!(result, (true, true, true, true, true, true, true, true));
}

#[test]
fn test_prometheus_unit_metric_full_name_and_output() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    setup_prometheus_lua(&lua, &mut ctx, "init_worker");

    let output: String = lua
        .load(
            r#"
            local prom = require("kong.plugins.prometheus.prometheus").init("metrics", "kong_")

            local m = prom:counter("mem", nil, {"lua"})
            m:inc(2, {"2.1"})

            m = prom:counter("file", nil, {"path"})
            m:inc(3, {"\\root"})

            m = prom:counter("user", nil, {"name"})
            m:inc(5, {"\"quote"})
            m:inc(1, {"\"quote"})

            prom._counter:sync()

            local chunks = {}
            prom:metric_data(function(d)
              chunks[#chunks + 1] = d
            end)

            return table.concat(chunks)
        "#,
        )
        .eval()
        .unwrap();

    assert!(output.contains(r#"kong_mem{lua="2.1"} 2"#));
    assert!(output.contains(r#"kong_file{path="\\root"} 3"#));
    assert!(output.contains(r#"kong_user{name="\"quote"} 6"#));
}

#[tokio::test]
async fn test_transplanted_prometheus_handler_runs_and_exports_metrics() {
    let plugin_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("kong/plugins");
    let handlers = loader::load_lua_plugins(&[plugin_root], &["prometheus".to_string()]).unwrap();
    let handler = &handlers[0];

    let config = PluginConfig {
        name: "prometheus".to_string(),
        config: json!({
            "per_consumer": true,
            "status_code_metrics": true,
            "latency_metrics": true,
            "bandwidth_metrics": true,
            "ai_metrics": false,
            "upstream_health_metrics": false,
            "wasm_metrics": false
        }),
    };

    handler.init_worker(&config).await.unwrap();

    let mut ctx = RequestCtx::new();
    ctx.response_source = Some("service".to_string());
    ctx.log_serialize = Some(json!({
        "service": { "name": "svc" },
        "route": { "name": "route-1" },
        "consumer": { "username": "alice" },
        "workspace_name": "default",
        "request": { "size": 128 },
        "response": { "status": 201, "size": 512 },
        "latencies": { "request": 30, "proxy": 12, "kong": 18 }
    }));

    handler.log(&config, &mut ctx).await.unwrap();

    let lua = unsafe { mlua::Lua::unsafe_new() };
    setup_prometheus_lua(&lua, &mut ctx, "init");
    lua.load(r#"handler = require("kong.plugins.prometheus.handler")"#)
        .exec()
        .unwrap();

    runtime::set_phase(&lua, "init_worker").unwrap();
    lua.load(r#"handler:init_worker({})"#).exec().unwrap();
    lua.load(
        r#"
        handler:configure({
          {
            per_consumer = true,
            status_code_metrics = true,
            latency_metrics = true,
            bandwidth_metrics = true,
            ai_metrics = false,
            upstream_health_metrics = false,
            wasm_metrics = false,
          }
        })
    "#,
    )
    .exec()
    .unwrap();

    runtime::set_phase(&lua, "log").unwrap();
    lua.load(
        r#"
        kong.log.serialize = function()
          return {
            service = { name = "svc" },
            route = { name = "route-1" },
            consumer = { username = "alice" },
            workspace_name = "default",
            request = { size = 128 },
            response = { status = 201, size = 512 },
            latencies = { request = 30, proxy = 12, kong = 18 },
          }
        end
    "#,
    )
    .exec()
    .unwrap();
    lua.load(
        r#"
        handler:log({
          per_consumer = true,
          status_code_metrics = true,
          latency_metrics = true,
          bandwidth_metrics = true,
          ai_metrics = false,
        })
    "#,
    )
    .exec()
    .unwrap();

    let metrics: String = lua
        .load(
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
        .unwrap();

    assert!(
        metrics.contains(r#"kong_http_requests_total{service="svc",route="route-1",code="201",source="service",workspace="default",consumer="alice"} 1"#),
        "{}",
        metrics
    );
    assert!(
        metrics.contains(r#"kong_bandwidth_bytes{service="svc",route="route-1",direction="ingress",workspace="default",consumer="alice"} 128"#),
        "{}",
        metrics
    );
    assert!(
        metrics.contains(r#"kong_bandwidth_bytes{service="svc",route="route-1",direction="egress",workspace="default",consumer="alice"} 512"#),
        "{}",
        metrics
    );
}
