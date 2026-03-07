//! Lua 插件兼容性集成测试
//!
//! 验证 kong-lua-bridge 能正确加载和执行 Lua 插件代码

use std::collections::HashMap;
use std::path::PathBuf;

use kong_core::traits::{Phase, RequestCtx};
use kong_lua_bridge::loader;

// ========== 插件加载器测试 ==========

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
    // 标准格式
    assert_eq!(loader::extract_priority("PRIORITY = 1000"), Some(1000));

    // 带空格
    assert_eq!(
        loader::extract_priority("  PRIORITY  =  750  "),
        Some(750)
    );

    // 多行中提取
    let code = r#"
        local M = {}
        M.PRIORITY = 900
        M.VERSION = "1.0.0"
        return M
    "#;
    assert_eq!(loader::extract_priority(code), Some(900));

    // 无 PRIORITY
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

// ========== PDK 注入测试 ==========

#[test]
fn test_pdk_kong_table_exists() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    // 验证 kong 全局表存在
    let kong: mlua::Table = lua.globals().get("kong").unwrap();

    // 验证子表存在
    let _request: mlua::Table = kong.get("request").unwrap();
    let _response: mlua::Table = kong.get("response").unwrap();
    let _log: mlua::Table = kong.get("log").unwrap();
    let _ctx_table: mlua::Table = kong.get("ctx").unwrap();
}

#[test]
fn test_ngx_compat_injected() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    // 验证 ngx 全局表存在
    let ngx: mlua::Table = lua.globals().get("ngx").unwrap();

    // 验证常用常量
    let http_ok: i32 = ngx.get("HTTP_OK").unwrap();
    assert_eq!(http_ok, 200);

    let http_not_found: i32 = ngx.get("HTTP_NOT_FOUND").unwrap();
    assert_eq!(http_not_found, 404);

    let http_internal_error: i32 = ngx.get("HTTP_INTERNAL_SERVER_ERROR").unwrap();
    assert_eq!(http_internal_error, 500);

    // 验证日志级别常量
    let debug: i32 = ngx.get("DEBUG").unwrap();
    assert_eq!(debug, 8);

    let err: i32 = ngx.get("ERR").unwrap();
    assert_eq!(err, 4);
}

#[test]
fn test_ngx_time_functions() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    kong_lua_bridge::pdk::inject_ngx_compat(&lua).unwrap();

    // ngx.now() 应返回当前时间戳
    let result: f64 = lua
        .load("return ngx.now()")
        .eval()
        .unwrap();
    assert!(result > 1_000_000_000.0); // 合理的 Unix 时间戳

    // ngx.time() 应返回整数时间戳
    let result: i64 = lua
        .load("return ngx.time()")
        .eval()
        .unwrap();
    assert!(result > 1_000_000_000);
}

#[test]
fn test_pdk_kong_log_functions() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    // 调用日志函数不应 panic
    lua.load(r#"
        kong.log.debug("debug message")
        kong.log.info("info message")
        kong.log.warn("warn message")
        kong.log.err("error message")
    "#)
    .exec()
    .unwrap();
}

#[test]
fn test_pdk_kong_ctx_shared() {
    let lua = unsafe { mlua::Lua::unsafe_new() };
    let mut ctx = RequestCtx::new();

    kong_lua_bridge::pdk::inject_kong_pdk(&lua, &mut ctx).unwrap();

    // 写入 kong.ctx.shared
    lua.load(r#"
        kong.ctx.shared.my_key = "my_value"
        kong.ctx.shared.counter = 42
    "#)
    .exec()
    .unwrap();

    // 同步回 RequestCtx
    kong_lua_bridge::pdk::sync_ctx_from_lua(&lua, &mut ctx).unwrap();

    // 验证数据同步
    assert!(ctx.shared.contains_key("my_key"));
    assert_eq!(
        ctx.shared.get("my_key").unwrap().as_str().unwrap(),
        "my_value"
    );
}

// ========== LuaPluginHandler 测试 ==========

#[test]
fn test_lua_plugin_handler_creation() {
    let phases = HashMap::from([
        (Phase::Access, true),
        (Phase::Log, true),
    ]);

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

// ========== Lua VM 基础测试 ==========

#[test]
fn test_lua_vm_basic_execution() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    // 基本 Lua 执行
    let result: i32 = lua.load("return 1 + 2").eval().unwrap();
    assert_eq!(result, 3);

    // 字符串操作
    let result: String = lua
        .load("return string.format('hello %s', 'world')")
        .eval()
        .unwrap();
    assert_eq!(result, "hello world");

    // 表操作
    let result: Vec<i32> = lua
        .load("return {1, 2, 3}")
        .eval()
        .unwrap();
    assert_eq!(result, vec![1, 2, 3]);
}

#[test]
fn test_lua_vm_table_creation_and_method_call() {
    let lua = unsafe { mlua::Lua::unsafe_new() };

    // 模拟 Kong 插件 handler 的模式
    lua.load(r#"
        local handler = {}

        function handler:access(conf)
            self._accessed = true
            self._conf_name = conf.name
        end

        function handler:log(conf)
            self._logged = true
        end

        return handler
    "#)
    .exec()
    .unwrap();
}

#[test]
fn test_lua_serde_roundtrip() {
    use mlua::LuaSerdeExt;

    let lua = unsafe { mlua::Lua::unsafe_new() };

    // 测试 Rust -> Lua -> Rust 序列化往返
    let config = serde_json::json!({
        "key": "value",
        "number": 42,
        "nested": { "a": 1 }
    });

    let lua_value = lua.to_value(&config).unwrap();
    lua.globals().set("test_config", lua_value).unwrap();

    let result: String = lua
        .load("return test_config.key")
        .eval()
        .unwrap();
    assert_eq!(result, "value");

    let result: i32 = lua
        .load("return test_config.number")
        .eval()
        .unwrap();
    assert_eq!(result, 42);

    let result: i32 = lua
        .load("return test_config.nested.a")
        .eval()
        .unwrap();
    assert_eq!(result, 1);
}
