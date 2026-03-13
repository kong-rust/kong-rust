//! Lua plugin compatibility integration tests — Lua 插件兼容性集成测试
//!
//! Verifies that kong-lua-bridge can correctly load and execute Lua plugin code — 验证 kong-lua-bridge 能正确加载和执行 Lua 插件代码

use std::collections::HashMap;
use std::path::PathBuf;

use kong_core::traits::{Phase, RequestCtx};
use kong_lua_bridge::loader;

// ========== Plugin loader tests — 插件加载器测试 ==========

#[test]
fn test_detect_phases_from_handler_code() {
    let handler_code = r#"
        local MyPlugin = {}

        function MyPlugin:access(conf)
            kong.log.info("access phase")
        end

        function MyPlugin:header_filter(conf)
            kong.response.set_header("X-Custom", "value")
        end

        function MyPlugin:log(conf)
            kong.log.info("logging")
        end

        MyPlugin.PRIORITY = 1000
        MyPlugin.VERSION = "1.0.0"

        return MyPlugin
    "#;

    let phases = loader::detect_phases(handler_code);
    assert!(*phases.get(&Phase::Access).unwrap_or(&false));
    assert!(*phases.get(&Phase::HeaderFilter).unwrap_or(&false));
    assert!(*phases.get(&Phase::Log).unwrap_or(&false));
    assert!(!phases.get(&Phase::Rewrite).unwrap_or(&false));
}

#[test]
fn test_extract_priority() {
    assert_eq!(loader::extract_priority("PRIORITY = 1000"), Some(1000));
    assert_eq!(loader::extract_priority("  PRIORITY  =  750  "), Some(750));

    let code = r#"
        local M = {}
        M.PRIORITY = 900
        M.VERSION = "1.0.0"
        return M
    "#;
    assert_eq!(loader::extract_priority(code), Some(900));
    assert_eq!(loader::extract_priority("local x = 1"), None);
}

#[test]
fn test_extract_version() {
    assert_eq!(
        loader::extract_version("VERSION = \"2.1.0\""),
        Some("2.1.0".to_string())
    );

    let code = r#"
        M.PRIORITY = 1000
        M.VERSION = "3.0.0-beta"
        return M
    "#;
    assert_eq!(
        loader::extract_version(code),
        Some("3.0.0-beta".to_string())
    );
}

// ========== PDK injection tests ��� PDK 注入测试 ==========

#[test]
fn test_pdk_kong_table_exists() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let kong: mlua::Table = lua.globals().get("kong").unwrap();
    let _request: mlua::Table = kong.get("request").unwrap();
    let _response: mlua::Table = kong.get("response").unwrap();
    let _log: mlua::Table = kong.get("log").unwrap();
    let _ctx_table: mlua::Table = kong.get("ctx").unwrap();
    let _service: mlua::Table = kong.get("service").unwrap();
    let _client: mlua::Table = kong.get("client").unwrap();
}

#[test]
fn test_ngx_compat_injected() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    let ngx: mlua::Table = lua.globals().get("ngx").unwrap();

    let http_ok: i32 = ngx.get("HTTP_OK").unwrap();
    assert_eq!(http_ok, 200);

    let http_not_found: i32 = ngx.get("HTTP_NOT_FOUND").unwrap();
    assert_eq!(http_not_found, 404);

    let http_internal_error: i32 = ngx.get("HTTP_INTERNAL_SERVER_ERROR").unwrap();
    assert_eq!(http_internal_error, 500);

    let debug: i32 = ngx.get("DEBUG").unwrap();
    assert_eq!(debug, 8);

    let err: i32 = ngx.get("ERR").unwrap();
    assert_eq!(err, 4);
}

#[test]
fn test_ngx_time_functions() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    let result: f64 = lua.load("return ngx.now()").eval().unwrap();
    assert!(result > 1_000_000_000.0);

    let result: i64 = lua.load("return ngx.time()").eval().unwrap();
    assert!(result > 1_000_000_000);
}

#[test]
fn test_pdk_kong_log_functions() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        kong.log.debug("debug message")
        kong.log.info("info message")
        kong.log.warn("warn message")
        kong.log.err("error message")
    "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn test_pdk_kong_ctx_shared() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        kong.ctx.shared.my_key = "my_value"
        kong.ctx.shared.counter = 42
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert!(ctx.shared.contains_key("my_key"));
    assert_eq!(
        ctx.shared.get("my_key").unwrap().as_str().unwrap(),
        "my_value"
    );
}

// ========== PDK real data tests (fixed hardcoded stubs) — PDK 真实数据测试（修复硬编码桩） ==========

#[test]
fn test_pdk_request_get_method_returns_real_method() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_method = "POST".to_string();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua.load("return kong.request.get_method()").eval().unwrap();
    assert_eq!(result, "POST", "PDK should return the real request method, not hardcoded GET — PDK 应返回真实的请求方法，而不是硬编码 GET");
}

#[test]
fn test_pdk_request_get_path_returns_real_path() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_path = "/api/v1/users".to_string();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua.load("return kong.request.get_path()").eval().unwrap();
    assert_eq!(
        result, "/api/v1/users",
        "PDK should return the real request path — PDK 应返回真实的请求路径"
    );
}

#[test]
fn test_pdk_request_get_scheme_returns_real_scheme() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_scheme = "https".to_string();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua.load("return kong.request.get_scheme()").eval().unwrap();
    assert_eq!(result, "https");
}

#[test]
fn test_pdk_request_get_host_returns_real_host() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_host = "api.example.com".to_string();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua.load("return kong.request.get_host()").eval().unwrap();
    assert_eq!(result, "api.example.com");
}

#[test]
fn test_pdk_request_get_port_returns_real_port() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_port = 8443;

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: i32 = lua.load("return kong.request.get_port()").eval().unwrap();
    assert_eq!(result, 8443);
}

#[test]
fn test_pdk_request_get_header_returns_real_header() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_headers
        .insert("x-api-key".to_string(), "secret-key-123".to_string());
    ctx.request_headers
        .insert("content-type".to_string(), "application/json".to_string());

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua
        .load(r#"return kong.request.get_header("x-api-key")"#)
        .eval()
        .unwrap();
    assert_eq!(result, "secret-key-123");

    let result: String = lua
        .load(r#"return kong.request.get_header("content-type")"#)
        .eval()
        .unwrap();
    assert_eq!(result, "application/json");

    // Non-existent header returns nil — 不存在的 header 返回 nil
    let result: Option<String> = lua
        .load(r#"return kong.request.get_header("x-nonexistent")"#)
        .eval()
        .unwrap();
    assert!(result.is_none());
}

#[test]
fn test_pdk_request_get_headers_returns_all_headers() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_headers
        .insert("host".to_string(), "example.com".to_string());
    ctx.request_headers
        .insert("accept".to_string(), "text/html".to_string());

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua
        .load(
            r#"
            local h = kong.request.get_headers()
            return h.host
        "#,
        )
        .eval()
        .unwrap();
    assert_eq!(result, "example.com");
}

#[test]
fn test_pdk_response_exit_sets_short_circuit() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    // Call kong.response.exit(403, '{"message":"forbidden"}') — 调用 kong.response.exit(403, '{"message":"forbidden"}')
    lua.load(r#"kong.response.exit(403, '{"message":"forbidden"}')"#)
        .exec()
        .unwrap();

    // Sync back to RequestCtx — 同步回 RequestCtx
    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert!(
        ctx.short_circuited,
        "exit() should set the short-circuit flag — exit() 应设置短路标志"
    );
    assert_eq!(
        ctx.exit_status,
        Some(403),
        "exit() should set the status code — exit() 应设置状态码"
    );
    assert_eq!(
        ctx.exit_body.as_deref(),
        Some(r#"{"message":"forbidden"}"#),
        "exit() should set the response body — exit() 应设置响应体"
    );
}

#[test]
fn test_pdk_response_exit_with_headers() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        kong.response.exit(401, "unauthorized", {
            ["WWW-Authenticate"] = "Bearer"
        })
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert!(ctx.short_circuited);
    assert_eq!(ctx.exit_status, Some(401));
    assert_eq!(ctx.exit_body.as_deref(), Some("unauthorized"));
    let exit_headers = ctx.exit_headers.as_ref().unwrap();
    assert_eq!(exit_headers.get("WWW-Authenticate").unwrap(), "Bearer");
}

#[test]
fn test_pdk_service_request_set_header() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        kong.service.request.set_header("X-Consumer-ID", "abc-123")
        kong.service.request.set_header("X-Custom", "value")
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert_eq!(ctx.upstream_headers_to_set.len(), 2);
    // Note: HashMap iteration order is non-deterministic, check containment — 注意：HashMap 遍历顺序不确定，检查包含关系
    let headers: HashMap<String, String> = ctx.upstream_headers_to_set.into_iter().collect();
    assert_eq!(headers.get("X-Consumer-ID").unwrap(), "abc-123");
    assert_eq!(headers.get("X-Custom").unwrap(), "value");
}

#[test]
fn test_pdk_service_request_clear_header() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        kong.service.request.clear_header("X-Remove-Me")
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert!(ctx
        .upstream_headers_to_remove
        .contains(&"X-Remove-Me".to_string()));
}

#[test]
fn test_pdk_response_set_header() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        kong.response.set_header("X-Response-Custom", "response-value")
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    let headers: HashMap<String, String> = ctx.response_headers_to_set.into_iter().collect();
    assert_eq!(headers.get("X-Response-Custom").unwrap(), "response-value");
}

#[test]
fn test_pdk_client_get_ip() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.client_ip = "192.168.1.100".to_string();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    let result: String = lua.load("return kong.client.get_ip()").eval().unwrap();
    assert_eq!(result, "192.168.1.100");
}

#[test]
fn test_ngx_req_get_method_returns_real_method() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_method = "DELETE".to_string();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();
    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    let result: String = lua.load("return ngx.req.get_method()").eval().unwrap();
    assert_eq!(
        result, "DELETE",
        "ngx.req.get_method() should return the real method — ngx.req.get_method() 应返回真实方法"
    );
}

#[test]
fn test_ngx_req_get_headers_returns_real_headers() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_headers
        .insert("host".to_string(), "myhost.com".to_string());

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();
    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    let result: String = lua
        .load(
            r#"
            local h = ngx.req.get_headers()
            return h.host
        "#,
        )
        .eval()
        .unwrap();
    assert_eq!(result, "myhost.com");
}

#[test]
fn test_ngx_var_reads_real_data() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.client_ip = "10.0.0.5".to_string();
    ctx.request_scheme = "https".to_string();
    ctx.request_host = "api.test.com".to_string();
    ctx.request_port = 8443;

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();
    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    let ip: String = lua.load("return ngx.var.remote_addr").eval().unwrap();
    assert_eq!(ip, "10.0.0.5");

    let scheme: String = lua.load("return ngx.var.scheme").eval().unwrap();
    assert_eq!(scheme, "https");

    let host: String = lua.load("return ngx.var.host").eval().unwrap();
    assert_eq!(host, "api.test.com");
}

// ========== LuaPluginHandler tests — LuaPluginHandler 测试 ==========

#[test]
fn test_lua_plugin_handler_creation() {
    let phases = HashMap::from([(Phase::Access, true), (Phase::Log, true)]);

    let handler = kong_lua_bridge::LuaPluginHandler::new(
        "test-plugin".to_string(),
        1000,
        "1.0.0".to_string(),
        PathBuf::from("/tmp/test-plugin"),
        phases,
    );

    use kong_core::traits::PluginHandler;
    assert_eq!(handler.name(), "test-plugin");
    assert_eq!(handler.priority(), 1000);
    assert_eq!(handler.version(), "1.0.0");
}

// ========== Lua VM basic tests — Lua VM 基础测试 ==========

#[test]
fn test_lua_vm_basic_execution() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    let result: i32 = lua.load("return 1 + 2").eval().unwrap();
    assert_eq!(result, 3);

    let result: String = lua
        .load("return string.format('hello %s', 'world')")
        .eval()
        .unwrap();
    assert_eq!(result, "hello world");

    let result: Vec<i32> = lua.load("return {1, 2, 3}").eval().unwrap();
    assert_eq!(result, vec![1, 2, 3]);
}

#[test]
fn test_lua_vm_table_creation_and_method_call() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    lua.load(
        r#"
        local handler = {}

        function handler:access(conf)
            self._accessed = true
            self._conf_name = conf.name
        end

        function handler:log(conf)
            self._logged = true
        end

        return handler
    "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn test_lua_serde_roundtrip() {
    use mlua::LuaSerdeExt;

    let lua = unsafe { mlua::Lua::unsafe_new() };

    let config = serde_json::json!({
        "key": "value",
        "number": 42,
        "nested": { "a": 1 }
    });

    let lua_value = lua.to_value(&config).unwrap();
    lua.globals().set("test_config", lua_value).unwrap();

    let result: String = lua.load("return test_config.key").eval().unwrap();
    assert_eq!(result, "value");

    let result: i32 = lua.load("return test_config.number").eval().unwrap();
    assert_eq!(result, 42);

    let result: i32 = lua.load("return test_config.nested.a").eval().unwrap();
    assert_eq!(result, 1);
}

// ========== End-to-end scenario tests — 综合场景测试 ==========

#[test]
fn test_pdk_full_request_context() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_method = "PUT".to_string();
    ctx.request_path = "/api/v2/items/42".to_string();
    ctx.request_scheme = "https".to_string();
    ctx.request_host = "store.example.com".to_string();
    ctx.request_port = 443;
    ctx.request_query_string = "format=json&pretty=true".to_string();
    ctx.client_ip = "203.0.113.50".to_string();
    ctx.request_headers
        .insert("authorization".to_string(), "Bearer tok123".to_string());
    ctx.request_headers
        .insert("content-type".to_string(), "application/json".to_string());

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    // Verify all fields are correctly readable — 验证所有字段都能正确读取
    let method: String = lua.load("return kong.request.get_method()").eval().unwrap();
    assert_eq!(method, "PUT");

    let path: String = lua.load("return kong.request.get_path()").eval().unwrap();
    assert_eq!(path, "/api/v2/items/42");

    let scheme: String = lua.load("return kong.request.get_scheme()").eval().unwrap();
    assert_eq!(scheme, "https");

    let host: String = lua.load("return kong.request.get_host()").eval().unwrap();
    assert_eq!(host, "store.example.com");

    let port: i32 = lua.load("return kong.request.get_port()").eval().unwrap();
    assert_eq!(port, 443);

    let qs: String = lua
        .load("return kong.request.get_raw_query()")
        .eval()
        .unwrap();
    assert_eq!(qs, "format=json&pretty=true");

    let ip: String = lua.load("return kong.client.get_ip()").eval().unwrap();
    assert_eq!(ip, "203.0.113.50");

    let auth: String = lua
        .load(r#"return kong.request.get_header("authorization")"#)
        .eval()
        .unwrap();
    assert_eq!(auth, "Bearer tok123");
}

#[test]
fn test_pdk_plugin_simulation_auth_check() {
    // Simulate a simple auth plugin behavior — 模拟一个简单的认证插件行为
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_method = "GET".to_string();
    ctx.request_path = "/protected/resource".to_string();
    // No authorization header -> should be short-circuited — 没有 authorization header → 应该被短路

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    // Simulate plugin logic — 模拟插件逻辑
    lua.load(
        r#"
        local auth = kong.request.get_header("authorization")
        if not auth then
            kong.response.exit(401, '{"message":"unauthorized"}')
        end
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert!(ctx.short_circuited);
    assert_eq!(ctx.exit_status, Some(401));
    assert_eq!(
        ctx.exit_body.as_deref(),
        Some(r#"{"message":"unauthorized"}"#)
    );
}

#[test]
fn test_pdk_plugin_simulation_auth_pass() {
    // Has authorization header -> should not short-circuit — 有 authorization header → 不应短路
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();
    ctx.request_method = "GET".to_string();
    ctx.request_path = "/protected/resource".to_string();
    ctx.request_headers.insert(
        "authorization".to_string(),
        "Bearer valid-token".to_string(),
    );

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    lua.load(
        r#"
        local auth = kong.request.get_header("authorization")
        if not auth then
            kong.response.exit(401, '{"message":"unauthorized"}')
        else
            kong.service.request.set_header("X-Authenticated", "true")
        end
    "#,
    )
    .exec()
    .unwrap();

    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    assert!(
        !ctx.short_circuited,
        "Should not short-circuit when authorization is present — 有 authorization 时不应短路"
    );
    let headers: HashMap<String, String> = ctx.upstream_headers_to_set.into_iter().collect();
    assert_eq!(headers.get("X-Authenticated").unwrap(), "true");
}
