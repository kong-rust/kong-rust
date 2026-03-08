//! Kong PDK compatibility layer — injects kong.* and ngx.* global tables into Lua VM — Kong PDK 兼容层 — 在 Lua VM 中注入 kong.* 和 ngx.* 全局表
//!
//! Implemented PDK modules: — 实现的 PDK 模块:
//! - kong.request: read request info (from __kong_req_data global table) — kong.request: 读取请求信息（从 __kong_req_data 全局表）
//! - kong.response: set response info, short-circuit (via __kong_short_circuited etc.) — kong.response: 设置响应信息、短路（通过 __kong_short_circuited 等全局变量）
//! - kong.service.request: modify upstream request (via __kong_upstream_headers etc.) — kong.service.request: 修改发往上游的请求（通过 __kong_upstream_headers 等）
//! - kong.log: log output — kong.log: 日志输出
//! - kong.ctx: request-level shared data — kong.ctx: 请求级共享数据
//! - ngx.req / ngx.log / ngx.var: basic compatibility — ngx.req / ngx.log / ngx.var: 基本兼容

use kong_core::traits::RequestCtx;
use mlua::prelude::*;
use mlua::LuaSerdeExt;

/// Inject the kong global PDK table into the Lua VM — 注入 kong 全局 PDK 表到 Lua VM
///
/// Core mechanism: snapshot RequestCtx into the Lua global __kong_req_data, — 核心机制：将 RequestCtx 快照写入 Lua 全局变量 __kong_req_data，
/// PDK functions read from this table instead of returning hardcoded values. — PDK 函数从这个表读取，而非硬编码返回值。
pub fn inject_kong_pdk(lua: &Lua, ctx: &mut RequestCtx) -> LuaResult<()> {
    let globals = lua.globals();

    // ====== 1. Write request data into Lua global table — 将请求数据写入 Lua 全局表 ======
    let req_data = lua.create_table()?;
    req_data.set("method", ctx.request_method.as_str())?;
    req_data.set("path", ctx.request_path.as_str())?;
    req_data.set("scheme", ctx.request_scheme.as_str())?;
    req_data.set("host", ctx.request_host.as_str())?;
    req_data.set("port", ctx.request_port as i32)?;
    req_data.set("query_string", ctx.request_query_string.as_str())?;
    req_data.set("client_ip", ctx.client_ip.as_str())?;

    // Request headers table — 请求头表
    let headers_table = lua.create_table()?;
    for (name, value) in &ctx.request_headers {
        headers_table.set(name.as_str(), value.as_str())?;
    }
    req_data.set("headers", headers_table)?;
    globals.set("__kong_req_data", req_data)?;

    // Response data table (populated during header_filter/log phases) — 响应数据表（header_filter/log 阶段填充）
    let resp_data = lua.create_table()?;
    if let Some(status) = ctx.response_status {
        resp_data.set("status", status as i32)?;
    }
    let resp_headers_table = lua.create_table()?;
    for (name, value) in &ctx.response_headers {
        resp_headers_table.set(name.as_str(), value.as_str())?;
    }
    resp_data.set("headers", resp_headers_table)?;
    globals.set("__kong_resp_data", resp_data)?;

    // Initialize short-circuit flags — 初始化短路标志
    globals.set("__kong_short_circuited", false)?;
    globals.set("__kong_exit_status", LuaValue::Nil)?;
    globals.set("__kong_exit_body", LuaValue::Nil)?;
    globals.set("__kong_exit_headers", lua.create_table()?)?;

    // Initialize upstream request header modification queue — 初始化上游请求头修改队列
    globals.set("__kong_upstream_headers_set", lua.create_table()?)?;
    globals.set("__kong_upstream_headers_remove", lua.create_table()?)?;

    // Initialize response header modification queue — 初始化响应头修改队列
    globals.set("__kong_response_headers_set", lua.create_table()?)?;
    globals.set("__kong_response_headers_remove", lua.create_table()?)?;

    // ====== 2. Create kong main table — 创建 kong 主表 ======
    let kong = lua.create_table()?;

    // ====== kong.request ======
    let request = lua.create_table()?;

    request.set("get_method", lua.create_function(|lua, _: ()| {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let method: String = data.get("method")?;
        if method.is_empty() {
            Ok("GET".to_string())
        } else {
            Ok(method)
        }
    })?)?;

    request.set("get_path", lua.create_function(|lua, _: ()| {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let path: String = data.get("path")?;
        if path.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(path)
        }
    })?)?;

    request.set("get_scheme", lua.create_function(|lua, _: ()| {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let scheme: String = data.get("scheme")?;
        if scheme.is_empty() {
            Ok("http".to_string())
        } else {
            Ok(scheme)
        }
    })?)?;

    request.set("get_host", lua.create_function(|lua, _: ()| {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let host: String = data.get("host")?;
        if host.is_empty() {
            Ok("localhost".to_string())
        } else {
            Ok(host)
        }
    })?)?;

    request.set("get_port", lua.create_function(|lua, _: ()| {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let port: i32 = data.get("port")?;
        if port == 0 { Ok(80_i32) } else { Ok(port) }
    })?)?;

    request.set("get_header", lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let headers: LuaTable = data.get("headers")?;
        let value: Option<String> = headers.get(name.to_lowercase())?;
        Ok(value)
    })?)?;

    request.set("get_headers", lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let headers: LuaTable = data.get("headers")?;
        Ok(headers)
    })?)?;

    request.set("get_query", lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
        // Simplified implementation: returns empty table (full impl needs query string parsing) — 简化实现：返回空表（完整实现需要解析 query string）
        lua.create_table()
    })?)?;

    request.set("get_raw_query", lua.create_function(|lua, _: ()| -> LuaResult<String> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let qs: String = data.get("query_string")?;
        Ok(qs)
    })?)?;

    kong.set("request", request)?;

    // ====== kong.response ======
    let response = lua.create_table()?;

    response.set("exit", lua.create_function(|lua, (status, body, headers): (u16, Option<String>, Option<LuaTable>)| -> LuaResult<()> {
        let g = lua.globals();
        g.set("__kong_short_circuited", true)?;
        g.set("__kong_exit_status", status)?;
        if let Some(b) = body {
            g.set("__kong_exit_body", b)?;
        }
        if let Some(h) = headers {
            g.set("__kong_exit_headers", h)?;
        }
        Ok(())
    })?)?;

    response.set("set_header", lua.create_function(|lua, (name, value): (String, String)| -> LuaResult<()> {
        let g = lua.globals();
        let headers_set: LuaTable = g.get("__kong_response_headers_set")?;
        headers_set.set(name, value)?;
        Ok(())
    })?)?;

    response.set("add_header", lua.create_function(|lua, (name, value): (String, String)| -> LuaResult<()> {
        let g = lua.globals();
        let headers_set: LuaTable = g.get("__kong_response_headers_set")?;
        headers_set.set(name, value)?;
        Ok(())
    })?)?;

    response.set("get_status", lua.create_function(|lua, _: ()| -> LuaResult<Option<i32>> {
        let data: LuaTable = lua.globals().get("__kong_resp_data")?;
        let status: Option<i32> = data.get("status").ok();
        Ok(status)
    })?)?;

    response.set("get_header", lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
        let data: LuaTable = lua.globals().get("__kong_resp_data")?;
        let headers: LuaTable = data.get("headers")?;
        let value: Option<String> = headers.get(name.to_lowercase())?;
        Ok(value)
    })?)?;

    response.set("get_headers", lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
        let data: LuaTable = lua.globals().get("__kong_resp_data")?;
        let headers: LuaTable = data.get("headers")?;
        Ok(headers)
    })?)?;

    kong.set("response", response)?;

    // ====== kong.service.request ======
    let service = lua.create_table()?;
    let service_request = lua.create_table()?;

    service_request.set("set_header", lua.create_function(|lua, (name, value): (String, String)| -> LuaResult<()> {
        let g = lua.globals();
        let headers_set: LuaTable = g.get("__kong_upstream_headers_set")?;
        headers_set.set(name, value)?;
        Ok(())
    })?)?;

    service_request.set("clear_header", lua.create_function(|lua, name: String| -> LuaResult<()> {
        let g = lua.globals();
        let headers_remove: LuaTable = g.get("__kong_upstream_headers_remove")?;
        let len = headers_remove.len()? + 1;
        headers_remove.set(len, name)?;
        Ok(())
    })?)?;

    service_request.set("set_scheme", lua.create_function(|_, _scheme: String| -> LuaResult<()> {
        // Scheme modification not yet supported (needs handling in upstream_peer) — scheme 修改暂不支持（需要在 upstream_peer 中处理）
        Ok(())
    })?)?;

    service.set("request", service_request)?;

    // kong.service.response
    let service_response = lua.create_table()?;
    service_response.set("get_status", lua.create_function(|lua, _: ()| -> LuaResult<Option<i32>> {
        let data: LuaTable = lua.globals().get("__kong_resp_data")?;
        let status: Option<i32> = data.get("status").ok();
        Ok(status)
    })?)?;
    service_response.set("get_header", lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
        let data: LuaTable = lua.globals().get("__kong_resp_data")?;
        let headers: LuaTable = data.get("headers")?;
        let value: Option<String> = headers.get(name.to_lowercase())?;
        Ok(value)
    })?)?;
    service_response.set("get_headers", lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
        let data: LuaTable = lua.globals().get("__kong_resp_data")?;
        let headers: LuaTable = data.get("headers")?;
        Ok(headers)
    })?)?;
    service.set("response", service_response)?;

    kong.set("service", service)?;

    // ====== kong.log ======
    let log = lua.create_table()?;
    log.set(
        "debug",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg.into_iter().map(|v| format!("{:?}", v)).collect();
            tracing::debug!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    log.set(
        "info",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg.into_iter().map(|v| format!("{:?}", v)).collect();
            tracing::info!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    log.set(
        "warn",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg.into_iter().map(|v| format!("{:?}", v)).collect();
            tracing::warn!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    log.set(
        "err",
        lua.create_function(|_, msg: LuaMultiValue| -> LuaResult<()> {
            let parts: Vec<String> = msg.into_iter().map(|v| format!("{:?}", v)).collect();
            tracing::error!("[lua] {}", parts.join(" "));
            Ok(())
        })?,
    )?;
    kong.set("log", log)?;

    // ====== kong.ctx ======
    let ctx_table = lua.create_table()?;
    let shared = lua.create_table()?;
    // Pre-populate with existing shared data — 预填充现有 shared 数据
    for (key, value) in &ctx.shared {
        if let Ok(lua_val) = lua.to_value(value) {
            shared.set(key.as_str(), lua_val)?;
        }
    }
    ctx_table.set("shared", shared)?;
    kong.set("ctx", ctx_table)?;

    // ====== kong.client ======
    let client = lua.create_table()?;
    client.set("get_ip", lua.create_function(|lua, _: ()| -> LuaResult<String> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let ip: String = data.get("client_ip")?;
        Ok(ip)
    })?)?;
    client.set("get_forwarded_ip", lua.create_function(|lua, _: ()| -> LuaResult<String> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let ip: String = data.get("client_ip")?;
        Ok(ip)
    })?)?;
    kong.set("client", client)?;

    globals.set("kong", kong)?;

    Ok(())
}

/// Inject ngx compatibility layer into the Lua VM — 注入 ngx 兼容层到 Lua VM
pub fn inject_ngx_compat(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    let ngx = lua.create_table()?;

    // ngx.log
    ngx.set(
        "log",
        lua.create_function(|_, (level, msg): (i32, LuaMultiValue)| -> LuaResult<()> {
            let parts: Vec<String> = msg.into_iter().map(|v| format!("{:?}", v)).collect();
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

    // ngx log level constants — ngx 日志级别常量
    ngx.set("DEBUG", 8)?;
    ngx.set("INFO", 7)?;
    ngx.set("NOTICE", 6)?;
    ngx.set("WARN", 5)?;
    ngx.set("ERR", 4)?;
    ngx.set("CRIT", 3)?;
    ngx.set("ALERT", 2)?;
    ngx.set("EMERG", 1)?;

    // ngx.OK, ngx.ERROR and other constants — ngx.OK, ngx.ERROR 等常量
    ngx.set("OK", 0)?;
    ngx.set("ERROR", -1)?;
    ngx.set("AGAIN", -2)?;
    ngx.set("DONE", -4)?;
    ngx.set("DECLINED", -5)?;

    // HTTP status codes — HTTP 状态码
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
            Ok(())
        })?,
    )?;

    // ngx.re.match / ngx.re.find
    let re = lua.create_table()?;
    re.set(
        "match",
        lua.create_function(|lua, (subject, regex, _options): (String, String, Option<String>)| -> LuaResult<Option<LuaTable>> {
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

    // ngx.req — reads real data from __kong_req_data — ngx.req — 从 __kong_req_data 读取真实数据
    let req = lua.create_table()?;
    req.set("get_headers", lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let headers: LuaTable = data.get("headers")?;
        Ok(headers)
    })?)?;
    req.set("get_method", lua.create_function(|lua, _: ()| -> LuaResult<String> {
        let data: LuaTable = lua.globals().get("__kong_req_data")?;
        let method: String = data.get("method")?;
        if method.is_empty() { Ok("GET".to_string()) } else { Ok(method) }
    })?)?;
    req.set("get_uri_args", lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
        lua.create_table()
    })?)?;
    ngx.set("req", req)?;

    // ngx.var — dynamically reads from __kong_req_data — ngx.var — 从 __kong_req_data 动态读取
    let var_meta = lua.create_table()?;
    var_meta.set("__index", lua.create_function(|lua, (_table, key): (LuaTable, String)| -> LuaResult<LuaValue> {
        let data: LuaResult<LuaTable> = lua.globals().get("__kong_req_data");
        if let Ok(data) = data {
            match key.as_str() {
                "remote_addr" => {
                    let ip: String = data.get("client_ip").unwrap_or_default();
                    return Ok(LuaValue::String(lua.create_string(&ip)?));
                }
                "scheme" => {
                    let scheme: String = data.get("scheme").unwrap_or_default();
                    return Ok(LuaValue::String(lua.create_string(&scheme)?));
                }
                "host" => {
                    let host: String = data.get("host").unwrap_or_default();
                    return Ok(LuaValue::String(lua.create_string(&host)?));
                }
                "request_uri" => {
                    let path: String = data.get("path").unwrap_or_default();
                    return Ok(LuaValue::String(lua.create_string(&path)?));
                }
                "uri" => {
                    let path: String = data.get("path").unwrap_or_default();
                    return Ok(LuaValue::String(lua.create_string(&path)?));
                }
                "server_port" => {
                    let port: i32 = data.get("port").unwrap_or(80);
                    return Ok(LuaValue::String(lua.create_string(&port.to_string())?));
                }
                _ => {}
            }
        }
        Ok(LuaValue::Nil)
    })?)?;

    let var = lua.create_table()?;
    var.set_metatable(Some(var_meta));
    ngx.set("var", var)?;

    globals.set("ngx", ngx)?;

    Ok(())
}

/// Sync context changes from the Lua VM back to RequestCtx — 从 Lua VM 中同步上下文变更回 RequestCtx
pub fn sync_ctx_from_lua(lua: &Lua, ctx: &mut RequestCtx) -> LuaResult<()> {
    let globals = lua.globals();

    // 1. Sync kong.ctx.shared back to RequestCtx — 同步 kong.ctx.shared 回 RequestCtx
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

    // 2. Sync short-circuit flags — 同步短路标志
    if let Ok(true) = globals.get::<bool>("__kong_short_circuited") {
        ctx.short_circuited = true;
        ctx.exit_status = globals.get::<Option<u16>>("__kong_exit_status").ok().flatten();
        ctx.exit_body = globals.get::<Option<String>>("__kong_exit_body").ok().flatten();

        // Sync short-circuit response headers — 同步短路响应头
        if let Ok(exit_headers) = globals.get::<LuaTable>("__kong_exit_headers") {
            let mut headers = std::collections::HashMap::new();
            for pair in exit_headers.pairs::<String, String>() {
                if let Ok((k, v)) = pair {
                    headers.insert(k, v);
                }
            }
            if !headers.is_empty() {
                ctx.exit_headers = Some(headers);
            }
        }
    }

    // 3. Sync upstream request header modifications — 同步上游请求头修改
    if let Ok(headers_set) = globals.get::<LuaTable>("__kong_upstream_headers_set") {
        for pair in headers_set.pairs::<String, String>() {
            if let Ok((name, value)) = pair {
                ctx.upstream_headers_to_set.push((name, value));
            }
        }
    }

    if let Ok(headers_remove) = globals.get::<LuaTable>("__kong_upstream_headers_remove") {
        for pair in headers_remove.pairs::<i64, String>() {
            if let Ok((_idx, name)) = pair {
                ctx.upstream_headers_to_remove.push(name);
            }
        }
    }

    // 4. Sync response header modifications — 同步响应头修改
    if let Ok(headers_set) = globals.get::<LuaTable>("__kong_response_headers_set") {
        for pair in headers_set.pairs::<String, String>() {
            if let Ok((name, value)) = pair {
                ctx.response_headers_to_set.push((name, value));
            }
        }
    }

    if let Ok(headers_remove) = globals.get::<LuaTable>("__kong_response_headers_remove") {
        for pair in headers_remove.pairs::<i64, String>() {
            if let Ok((_idx, name)) = pair {
                ctx.response_headers_to_remove.push(name);
            }
        }
    }

    Ok(())
}
