//! Kong PDK 兼容层 — 在 Lua VM 中注入 kong.* 和 ngx.* 全局表
//!
//! 实现的 PDK 模块:
//! - kong.request: 读取请求信息
//! - kong.response: 设置响应信息、短路
//! - kong.service.request: 修改发往上游的请求
//! - kong.log: 日志输出
//! - kong.ctx: 请求级共享数据
//! - ngx.req / ngx.log / ngx.var: 基本兼容

use kong_core::traits::RequestCtx;
use mlua::prelude::*;
use mlua::LuaSerdeExt;

/// 注入 kong 全局 PDK 表到 Lua VM
pub fn inject_kong_pdk(lua: &Lua, _ctx: &mut RequestCtx) -> LuaResult<()> {
    let globals = lua.globals();

    // 创建 kong 主表
    let kong = lua.create_table()?;

    // kong.request
    let request = lua.create_table()?;
    // kong.request.get_method()
    // （实际运行时这些值从请求上下文注入，这里提供框架）
    request.set(
        "get_method",
        lua.create_function(|_, _: ()| Ok("GET".to_string()))?,
    )?;
    request.set(
        "get_path",
        lua.create_function(|_, _: ()| Ok("/".to_string()))?,
    )?;
    request.set(
        "get_scheme",
        lua.create_function(|_, _: ()| Ok("http".to_string()))?,
    )?;
    request.set(
        "get_host",
        lua.create_function(|_, _: ()| Ok("localhost".to_string()))?,
    )?;
    request.set(
        "get_port",
        lua.create_function(|_, _: ()| Ok(80_i32))?,
    )?;
    request.set(
        "get_header",
        lua.create_function(|_, _name: String| -> LuaResult<Option<String>> {
            // 返回 nil（实际运行时从请求中查找）
            Ok(None)
        })?,
    )?;
    request.set(
        "get_headers",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            lua.create_table()
        })?,
    )?;
    kong.set("request", request)?;

    // kong.response
    let response = lua.create_table()?;
    response.set(
        "exit",
        lua.create_function(|_, (_status, _body, _headers): (u16, Option<String>, Option<LuaTable>)| -> LuaResult<()> {
            // 通过 Lua registry 设置短路标志
            // 实际实现需要更复杂的上下文传递
            Ok(())
        })?,
    )?;
    response.set(
        "set_header",
        lua.create_function(|_, (_name, _value): (String, String)| -> LuaResult<()> {
            Ok(())
        })?,
    )?;
    response.set(
        "add_header",
        lua.create_function(|_, (_name, _value): (String, String)| -> LuaResult<()> {
            Ok(())
        })?,
    )?;
    kong.set("response", response)?;

    // kong.service.request
    let service = lua.create_table()?;
    let service_request = lua.create_table()?;
    service_request.set(
        "set_header",
        lua.create_function(|_, (_name, _value): (String, String)| -> LuaResult<()> {
            Ok(())
        })?,
    )?;
    service_request.set(
        "clear_header",
        lua.create_function(|_, _name: String| -> LuaResult<()> {
            Ok(())
        })?,
    )?;
    service_request.set(
        "set_scheme",
        lua.create_function(|_, _scheme: String| -> LuaResult<()> {
            Ok(())
        })?,
    )?;
    service.set("request", service_request)?;
    kong.set("service", service)?;

    // kong.log
    let log = lua.create_table()?;
    log.set(
        "debug",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg
                .into_iter()
                .map(|v| format!("{:?}", v))
                .collect();
            tracing::debug!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    log.set(
        "info",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg
                .into_iter()
                .map(|v| format!("{:?}", v))
                .collect();
            tracing::info!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    log.set(
        "warn",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg
                .into_iter()
                .map(|v| format!("{:?}", v))
                .collect();
            tracing::warn!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    log.set(
        "err",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg
                .into_iter()
                .map(|v| format!("{:?}", v))
                .collect();
            tracing::error!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    kong.set("log", log)?;

    // kong.ctx
    let ctx_table = lua.create_table()?;
    let shared = lua.create_table()?;
    ctx_table.set("shared", shared)?;
    kong.set("ctx", ctx_table)?;

    globals.set("kong", kong)?;

    Ok(())
}

/// 注入 ngx 兼容层到 Lua VM
pub fn inject_ngx_compat(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    let ngx = lua.create_table()?;

    // ngx.log
    ngx.set(
        "log",
        lua.create_function(|_, (level, msg): (i32, LuaMultiValue)| -> LuaResult<()> {
            let parts: Vec<String> = msg
                .into_iter()
                .map(|v| format!("{:?}", v))
                .collect();
            let text = parts.join(" ");
            match level {
                8 => tracing::debug!("[ngx] {}", text),  // ngx.DEBUG
                7 => tracing::info!("[ngx] {}", text),    // ngx.INFO
                6 => tracing::info!("[ngx] {}", text),    // ngx.NOTICE
                5 => tracing::warn!("[ngx] {}", text),    // ngx.WARN
                4 => tracing::error!("[ngx] {}", text),   // ngx.ERR
                _ => tracing::error!("[ngx] {}", text),
            }
            Ok(())
        })?,
    )?;

    // ngx 日志级别常量
    ngx.set("DEBUG", 8)?;
    ngx.set("INFO", 7)?;
    ngx.set("NOTICE", 6)?;
    ngx.set("WARN", 5)?;
    ngx.set("ERR", 4)?;
    ngx.set("CRIT", 3)?;
    ngx.set("ALERT", 2)?;
    ngx.set("EMERG", 1)?;

    // ngx.OK, ngx.ERROR 等常量
    ngx.set("OK", 0)?;
    ngx.set("ERROR", -1)?;
    ngx.set("AGAIN", -2)?;
    ngx.set("DONE", -4)?;
    ngx.set("DECLINED", -5)?;

    // HTTP 状态码
    ngx.set("HTTP_OK", 200)?;
    ngx.set("HTTP_CREATED", 201)?;
    ngx.set("HTTP_NO_CONTENT", 204)?;
    ngx.set("HTTP_MOVED_PERMANENTLY", 301)?;
    ngx.set("HTTP_MOVED_TEMPORARILY", 302)?;
    ngx.set("HTTP_BAD_REQUEST", 400)?;
    ngx.set("HTTP_UNAUTHORIZED", 401)?;
    ngx.set("HTTP_FORBIDDEN", 403)?;
    ngx.set("HTTP_NOT_FOUND", 404)?;
    ngx.set("HTTP_NOT_ALLOWED", 405)?;
    ngx.set("HTTP_INTERNAL_SERVER_ERROR", 500)?;
    ngx.set("HTTP_BAD_GATEWAY", 502)?;
    ngx.set("HTTP_SERVICE_UNAVAILABLE", 503)?;
    ngx.set("HTTP_GATEWAY_TIMEOUT", 504)?;

    // ngx.null
    ngx.set("null", lua.null())?;

    // ngx.now()
    ngx.set(
        "now",
        lua.create_function(|_, _: ()| -> LuaResult<f64> {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            Ok(now.as_secs_f64())
        })?,
    )?;

    // ngx.time()
    ngx.set(
        "time",
        lua.create_function(|_, _: ()| -> LuaResult<i64> {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            Ok(now.as_secs() as i64)
        })?,
    )?;

    // ngx.sleep()
    ngx.set(
        "sleep",
        lua.create_function(|_, _seconds: f64| -> LuaResult<()> {
            // 在同步 Lua 上下文中不支持真正的 sleep
            Ok(())
        })?,
    )?;

    // ngx.re.match / ngx.re.find (简化实现)
    let re = lua.create_table()?;
    re.set(
        "match",
        lua.create_function(|lua, (subject, regex, _options): (String, String, Option<String>)| -> LuaResult<Option<LuaTable>> {
            // 简化实现：使用 Lua 的 string.match
            let code = format!(
                "return string.match({:?}, {:?})",
                subject, regex
            );
            let result: Option<String> = lua.load(&code).eval().ok();
            if let Some(m) = result {
                let t = lua.create_table()?;
                t.set(0, m)?;
                Ok(Some(t))
            } else {
                Ok(None)
            }
        })?,
    )?;
    ngx.set("re", re)?;

    // ngx.req 子表
    let req = lua.create_table()?;
    req.set(
        "get_headers",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            lua.create_table()
        })?,
    )?;
    req.set(
        "get_method",
        lua.create_function(|_, _: ()| -> LuaResult<String> {
            Ok("GET".to_string())
        })?,
    )?;
    ngx.set("req", req)?;

    // ngx.var 子表（空表，各字段按需填充）
    let var = lua.create_table()?;
    ngx.set("var", var)?;

    globals.set("ngx", ngx)?;

    Ok(())
}

/// 从 Lua VM 中同步上下文变更回 RequestCtx
pub fn sync_ctx_from_lua(lua: &Lua, ctx: &mut RequestCtx) -> LuaResult<()> {
    let globals = lua.globals();

    // 同步 kong.ctx.shared 回 RequestCtx
    if let Ok(kong) = globals.get::<LuaTable>("kong") {
        if let Ok(ctx_table) = kong.get::<LuaTable>("ctx") {
            if let Ok(shared) = ctx_table.get::<LuaTable>("shared") {
                for pair in shared.pairs::<String, LuaValue>() {
                    if let Ok((key, value)) = pair {
                        if let Ok(json_val) = lua.from_value::<serde_json::Value>(value) {
                            ctx.shared.insert(key, json_val);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
