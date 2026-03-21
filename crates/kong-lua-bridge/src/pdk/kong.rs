use kong_core::traits::RequestCtx;
use mlua::prelude::*;
use mlua::LuaSerdeExt;
use std::collections::HashMap;

use crate::metrics;

fn parse_query_string(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|segment| !segment.is_empty())
        .map(|segment| match segment.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (segment.to_string(), String::new()),
        })
        .collect()
}

fn default_log_serialize(lua: &Lua, ctx: &RequestCtx) -> LuaResult<LuaValue> {
    let message = lua.create_table()?;

    if let Some(route_id) = ctx.route_id {
        let route = lua.create_table()?;
        route.set("id", route_id.to_string())?;
        message.set("route", route)?;
    }

    if let Some(service_id) = ctx.service_id {
        let service = lua.create_table()?;
        service.set("id", service_id.to_string())?;
        service.set("name", service_id.to_string())?;
        message.set("service", service)?;
    }

    let request = lua.create_table()?;
    request.set("method", ctx.request_method.as_str())?;
    request.set("path", ctx.request_path.as_str())?;
    message.set("request", request)?;

    let response = lua.create_table()?;
    if let Some(status) = ctx.response_status {
        response.set("status", status)?;
    }
    message.set("response", response)?;

    Ok(LuaValue::Table(message))
}

/// Convert Lua scalar values into optional strings while tolerating ngx.null. — 将 Lua 标量值转换为可选字符串，并兼容 ngx.null。
fn lua_value_to_optional_string(lua: &Lua, value: LuaValue) -> LuaResult<Option<String>> {
    match value {
        LuaValue::Nil | LuaValue::LightUserData(_) => Ok(None),
        LuaValue::String(value) => Ok(Some(value.to_string_lossy().to_string())),
        LuaValue::Integer(value) => Ok(Some(value.to_string())),
        LuaValue::Number(value) => Ok(Some(value.to_string())),
        LuaValue::Boolean(value) => Ok(Some(value.to_string())),
        other => Ok(lua
            .coerce_string(other)?
            .map(|value| value.to_string_lossy().to_string())),
    }
}

/// Inject the Kong global table into the Lua VM. — 将 Kong 全局表注入 Lua VM。
pub fn inject_kong_pdk(lua: &Lua, ctx: &mut RequestCtx) -> LuaResult<()> {
    let globals = lua.globals();

    let req_data = lua.create_table()?;
    req_data.set("method", ctx.request_method.as_str())?;
    req_data.set("path", ctx.request_path.as_str())?;
    req_data.set("scheme", ctx.request_scheme.as_str())?;
    req_data.set("host", ctx.request_host.as_str())?;
    req_data.set("port", ctx.request_port as i32)?;
    req_data.set("query_string", ctx.request_query_string.as_str())?;
    req_data.set("client_ip", ctx.client_ip.as_str())?;
    req_data.set("body", ctx.request_body.clone())?;

    let headers_table = lua.create_table()?;
    for (name, value) in &ctx.request_headers {
        headers_table.set(name.as_str(), value.as_str())?;
    }
    req_data.set("headers", headers_table)?;
    globals.set("__kong_req_data", req_data)?;

    let resp_data = lua.create_table()?;
    if let Some(status) = ctx.response_status {
        resp_data.set("status", status as i32)?;
    }
    let resp_headers_table = lua.create_table()?;
    for (name, value) in &ctx.response_headers {
        resp_headers_table.set(name.as_str(), value.as_str())?;
    }
    resp_data.set("headers", resp_headers_table)?;
    resp_data.set("body", ctx.service_response_body.clone())?;
    globals.set("__kong_resp_data", resp_data)?;

    globals.set("__kong_short_circuited", false)?;
    globals.set("__kong_exit_status", LuaValue::Nil)?;
    globals.set("__kong_exit_body", LuaValue::Nil)?;
    globals.set("__kong_exit_headers", lua.create_table()?)?;
    globals.set("__kong_upstream_headers_set", lua.create_table()?)?;
    globals.set("__kong_upstream_headers_remove", lua.create_table()?)?;
    globals.set("__kong_upstream_query_set", lua.create_table()?)?;
    globals.set("__kong_upstream_path", LuaValue::Nil)?;
    globals.set("__kong_upstream_scheme", LuaValue::Nil)?;
    globals.set("__kong_upstream_target_host", LuaValue::Nil)?;
    globals.set("__kong_upstream_target_port", LuaValue::Nil)?;
    globals.set("__kong_upstream_body", LuaValue::Nil)?;
    globals.set("__kong_request_buffering_enabled", false)?;
    globals.set("__kong_retry_callback_registered", false)?;
    globals.set("__kong_response_headers_set", lua.create_table()?)?;
    globals.set("__kong_response_headers_remove", lua.create_table()?)?;
    globals.set(
        "__kong_log_serialize",
        match &ctx.log_serialize {
            Some(value) => lua.to_value(value)?,
            None => default_log_serialize(lua, ctx)?,
        },
    )?;
    globals.set(
        "__kong_response_source",
        ctx.response_source
            .clone()
            .unwrap_or_else(|| "service".to_string()),
    )?;
    if let Some(value) = ctx.shared.get("__ngx_ctx_state") {
        globals.set("__persisted_ngx_ctx", lua.to_value(value)?)?;
    } else {
        globals.set("__persisted_ngx_ctx", LuaValue::Nil)?;
    }
    if let Some(value) = ctx.shared.get("__ngx_ctx_global_source") {
        globals.set("__persisted_ngx_ctx_global_source", lua.to_value(value)?)?;
    } else {
        globals.set("__persisted_ngx_ctx_global_source", LuaValue::Nil)?;
    }

    let kong = lua.create_table()?;
    kong.set("version", "3.0.0")?;

    let request = lua.create_table()?;
    request.set(
        "get_method",
        lua.create_function(|lua, _: ()| {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let method: String = data.get("method")?;
            Ok(if method.is_empty() {
                "GET".to_string()
            } else {
                method
            })
        })?,
    )?;
    request.set(
        "get_path",
        lua.create_function(|lua, _: ()| {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let path: String = data.get("path")?;
            Ok(if path.is_empty() {
                "/".to_string()
            } else {
                path
            })
        })?,
    )?;
    request.set(
        "get_scheme",
        lua.create_function(|lua, _: ()| {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let scheme: String = data.get("scheme")?;
            Ok(if scheme.is_empty() {
                "http".to_string()
            } else {
                scheme
            })
        })?,
    )?;
    request.set(
        "get_host",
        lua.create_function(|lua, _: ()| {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let host: String = data.get("host")?;
            Ok(if host.is_empty() {
                "localhost".to_string()
            } else {
                host
            })
        })?,
    )?;
    request.set(
        "get_port",
        lua.create_function(|lua, _: ()| {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let port: i32 = data.get("port")?;
            Ok(if port == 0 { 80 } else { port })
        })?,
    )?;
    request.set(
        "get_header",
        lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let headers: LuaTable = data.get("headers")?;
            headers.get(name.to_lowercase())
        })?,
    )?;
    request.set(
        "get_headers",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("headers")
        })?,
    )?;
    request.set(
        "get_query",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let query_string: String = data.get("query_string")?;
            let query = lua.create_table()?;
            for (key, value) in parse_query_string(&query_string) {
                query.set(key, value)?;
            }
            Ok(query)
        })?,
    )?;
    request.set(
        "get_query_arg",
        lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let query_string: String = data.get("query_string")?;
            Ok(parse_query_string(&query_string).remove(&name))
        })?,
    )?;
    request.set(
        "get_raw_query",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("query_string")
        })?,
    )?;
    request.set(
        "get_raw_body",
        lua.create_function(|lua, _: ()| -> LuaResult<Option<String>> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("body")
        })?,
    )?;
    request.set(
        "get_body",
        lua.create_function(
            |lua, (_mimetype, _max_args, _max_bytes): (Option<String>, Option<i64>, Option<i64>)| {
                let data: LuaTable = lua.globals().get("__kong_req_data")?;
                let body: Option<String> = data.get("body")?;
                let Some(body) = body else {
                    return Ok(LuaValue::Nil);
                };

                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(value) => lua.to_value(&value),
                    Err(_) => Ok(LuaValue::Nil),
                }
            },
        )?,
    )?;
    request.set(
        "get_uri_captures",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let captures = lua.create_table()?;
            captures.set("unnamed", lua.create_table()?)?;
            captures.set("named", lua.create_table()?)?;
            Ok(captures)
        })?,
    )?;
    kong.set("request", request)?;

    let response = lua.create_table()?;
    response.set(
        "exit",
        lua.create_function(
            |lua, (status, body, headers): (u16, Option<LuaValue>, Option<LuaTable>)| -> LuaResult<()> {
                let globals = lua.globals();
                globals.set("__kong_short_circuited", true)?;
                globals.set("__kong_exit_status", status)?;
                if let Some(body) = body {
                    let body = match body {
                        LuaValue::String(value) => value.to_string_lossy().to_string(),
                        LuaValue::Table(value) => serde_json::to_string(
                            &lua.from_value::<serde_json::Value>(LuaValue::Table(value))?,
                        )
                        .map_err(LuaError::external)?,
                        LuaValue::Nil => String::new(),
                        other => lua.coerce_string(other)?.map(|value| value.to_string_lossy().to_string()).ok_or_else(|| {
                            LuaError::external("unsupported kong.response.exit body type")
                        })?,
                    };
                    globals.set("__kong_exit_body", body)?;
                }
                if let Some(headers) = headers {
                    globals.set("__kong_exit_headers", headers)?;
                }
                Ok(())
            },
        )?,
    )?;
    response.set(
        "set_header",
        lua.create_function(|lua, (name, value): (LuaValue, LuaValue)| -> LuaResult<()> {
            let Some(name) = lua_value_to_optional_string(lua, name)? else {
                return Ok(());
            };
            let Some(value) = lua_value_to_optional_string(lua, value)? else {
                return Ok(());
            };
            let headers_set: LuaTable = lua.globals().get("__kong_response_headers_set")?;
            headers_set.set(name, value)?;
            Ok(())
        })?,
    )?;
    response.set(
        "add_header",
        lua.create_function(|lua, (name, value): (LuaValue, LuaValue)| -> LuaResult<()> {
            let Some(name) = lua_value_to_optional_string(lua, name)? else {
                return Ok(());
            };
            let Some(value) = lua_value_to_optional_string(lua, value)? else {
                return Ok(());
            };
            let headers_set: LuaTable = lua.globals().get("__kong_response_headers_set")?;
            headers_set.set(name, value)?;
            Ok(())
        })?,
    )?;
    response.set(
        "get_status",
        lua.create_function(|lua, _: ()| -> LuaResult<Option<i32>> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            Ok(data.get("status").ok())
        })?,
    )?;
    response.set(
        "get_header",
        lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            let headers: LuaTable = data.get("headers")?;
            headers.get(name.to_lowercase())
        })?,
    )?;
    response.set(
        "get_headers",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            data.get("headers")
        })?,
    )?;
    response.set(
        "get_source",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            lua.globals().get("__kong_response_source")
        })?,
    )?;
    response.set(
        "set_header",
        lua.create_function(|lua, (name, value): (LuaValue, LuaValue)| -> LuaResult<()> {
            let Some(name) = lua_value_to_optional_string(lua, name)? else {
                return Ok(());
            };
            let Some(value) = lua_value_to_optional_string(lua, value)? else {
                return Ok(());
            };
            let headers_set: LuaTable = lua.globals().get("__kong_response_headers_set")?;
            headers_set.set(name, value)?;
            Ok(())
        })?,
    )?;
    response.set(
        "clear_header",
        lua.create_function(|lua, name: String| -> LuaResult<()> {
            let headers_remove: LuaTable = lua.globals().get("__kong_response_headers_remove")?;
            let len = headers_remove.len()? + 1;
            headers_remove.set(len, name)?;
            Ok(())
        })?,
    )?;
    kong.set("response", response)?;

    let service = lua.create_table()?;
    let service_request = lua.create_table()?;
    service_request.set(
        "set_header",
        lua.create_function(|lua, (name, value): (LuaValue, LuaValue)| -> LuaResult<()> {
            let Some(name) = lua_value_to_optional_string(lua, name)? else {
                return Ok(());
            };
            let Some(value) = lua_value_to_optional_string(lua, value)? else {
                return Ok(());
            };
            let headers_set: LuaTable = lua.globals().get("__kong_upstream_headers_set")?;
            headers_set.set(name, value)?;
            Ok(())
        })?,
    )?;
    service_request.set(
        "clear_header",
        lua.create_function(|lua, name: String| -> LuaResult<()> {
            let headers_remove: LuaTable = lua.globals().get("__kong_upstream_headers_remove")?;
            let len = headers_remove.len()? + 1;
            headers_remove.set(len, name)?;
            Ok(())
        })?,
    )?;
    service_request.set(
        "set_scheme",
        lua.create_function(|lua, scheme: String| -> LuaResult<()> {
            lua.globals().set("__kong_upstream_scheme", scheme)?;
            Ok(())
        })?,
    )?;
    service_request.set(
        "set_path",
        lua.create_function(|lua, path: String| -> LuaResult<()> {
            lua.globals().set("__kong_upstream_path", path)?;
            Ok(())
        })?,
    )?;
    service_request.set(
        "set_query",
        lua.create_function(|lua, query: LuaTable| -> LuaResult<()> {
            let globals = lua.globals();
            let query_table: LuaTable = globals.get("__kong_upstream_query_set")?;
            query_table.clear()?;
            for pair in query.pairs::<String, LuaValue>() {
                let (name, value) = pair?;
                match value {
                    LuaValue::String(value) => {
                        query_table.set(name, value.to_string_lossy().to_string())?
                    }
                    LuaValue::Integer(value) => query_table.set(name, value.to_string())?,
                    LuaValue::Number(value) => query_table.set(name, value.to_string())?,
                    LuaValue::Boolean(value) => query_table.set(name, value.to_string())?,
                    LuaValue::Nil => {}
                    _ => {}
                }
            }
            Ok(())
        })?,
    )?;
    service_request.set(
        "set_body",
        lua.create_function(
            |lua, (body, _content_type): (LuaValue, Option<String>)| -> LuaResult<()> {
                let string_body = match body {
                    LuaValue::String(value) => value.to_string_lossy().to_string(),
                    LuaValue::Table(value) => serde_json::to_string(
                        &lua.from_value::<serde_json::Value>(LuaValue::Table(value))?,
                    )
                    .map_err(LuaError::external)?,
                    LuaValue::Nil => String::new(),
                    other => format!("{other:?}"),
                };
                lua.globals().set("__kong_upstream_body", string_body)?;
                Ok(())
            },
        )?,
    )?;
    service_request.set(
        "enable_buffering",
        lua.create_function(|lua, _: ()| -> LuaResult<()> {
            lua.globals()
                .set("__kong_request_buffering_enabled", true)?;
            Ok(())
        })?,
    )?;
    service.set("request", service_request)?;
    service.set(
        "set_target",
        lua.create_function(|lua, (host, port): (String, i64)| -> LuaResult<()> {
            let globals = lua.globals();
            globals.set("__kong_upstream_target_host", host)?;
            globals.set("__kong_upstream_target_port", port)?;
            Ok(())
        })?,
    )?;
    service.set(
        "set_target_retry_callback",
        lua.create_function(|lua, _callback: LuaFunction| -> LuaResult<()> {
            lua.globals()
                .set("__kong_retry_callback_registered", true)?;
            Ok(())
        })?,
    )?;

    let service_response = lua.create_table()?;
    service_response.set(
        "get_status",
        lua.create_function(|lua, _: ()| -> LuaResult<Option<i32>> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            Ok(data.get("status").ok())
        })?,
    )?;
    service_response.set(
        "get_header",
        lua.create_function(|lua, name: String| -> LuaResult<Option<String>> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            let headers: LuaTable = data.get("headers")?;
            headers.get(name.to_lowercase())
        })?,
    )?;
    service_response.set(
        "get_headers",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            data.get("headers")
        })?,
    )?;
    service_response.set(
        "get_raw_body",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            let data: LuaTable = lua.globals().get("__kong_resp_data")?;
            let body: Option<String> = data.get("body")?;
            Ok(body.unwrap_or_default())
        })?,
    )?;
    service.set("response", service_response)?;
    kong.set("service", service)?;

    let log = lua.create_table()?;
    for (name, level) in [
        ("trace", "trace"),
        ("debug", "debug"),
        ("info", "info"),
        ("warn", "warn"),
        ("err", "error"),
    ] {
        log.set(
            name,
            lua.create_function(move |_, msg: LuaMultiValue| -> LuaResult<()> {
                let parts: Vec<String> =
                    msg.into_iter().map(|value| format!("{value:?}")).collect();
                let text = parts.join(" ");
                match level {
                    "trace" => tracing::trace!("[lua] {}", text),
                    "debug" => tracing::debug!("[lua] {}", text),
                    "info" => tracing::info!("[lua] {}", text),
                    "warn" => tracing::warn!("[lua] {}", text),
                    _ => tracing::error!("[lua] {}", text),
                }
                Ok(())
            })?,
        )?;
    }
    log.set(
        "serialize",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaValue> {
            lua.globals().get("__kong_log_serialize")
        })?,
    )?;
    kong.set("log", log)?;

    let ctx_table = lua.create_table()?;
    let shared = lua.create_table()?;
    for (key, value) in &ctx.shared {
        if let Ok(lua_value) = lua.to_value(value) {
            shared.set(key.as_str(), lua_value)?;
        }
    }
    ctx_table.set("shared", shared)?;
    ctx_table.set("plugin", lua.create_table()?)?;
    kong.set("ctx", ctx_table)?;

    let client = lua.create_table()?;
    client.set(
        "get_ip",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("client_ip")
        })?,
    )?;
    client.set(
        "get_forwarded_ip",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("client_ip")
        })?,
    )?;
    kong.set("client", client)?;

    let node = lua.create_table()?;
    // Generate stable node_id from hostname using UUID v5 — 基于 hostname 生成稳定的 node_id（UUID v5）
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "localhost".to_string());
    let node_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, hostname.as_bytes()).to_string();
    let hostname_clone = hostname.clone();
    node.set(
        "get_id",
        lua.create_function(move |_, _: ()| Ok(node_id.clone()))?,
    )?;
    node.set(
        "get_hostname",
        lua.create_function(move |_, _: ()| Ok(hostname_clone.clone()))?,
    )?;
    node.set(
        "get_memory_stats",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let result = lua.create_table()?;
            let shms = lua.create_table()?;
            let prometheus = lua.create_table()?;
            prometheus.set("capacity", 1024 * 1024)?;
            prometheus.set("allocated_slabs", 0)?;
            shms.set("prometheus_metrics", prometheus)?;
            result.set("lua_shared_dicts", shms)?;

            let workers = lua.create_table()?;
            let worker = lua.create_table()?;
            worker.set("pid", 1)?;
            worker.set("http_allocated_gc", 0)?;
            workers.set(1, worker)?;
            result.set("workers_lua_vms", workers)?;
            Ok(result)
        })?,
    )?;
    kong.set("node", node)?;

    let nginx = lua.create_table()?;
    nginx.set(
        "get_statistics",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let stats = lua.create_table()?;
            let total_requests = metrics::http_total_requests() as i64;
            for (key, value) in [
                ("connections_accepted", 0),
                ("connections_handled", 0),
                ("total_requests", total_requests),
                ("connections_active", 0),
                ("connections_reading", 0),
                ("connections_writing", 0),
                ("connections_waiting", 0),
            ] {
                stats.set(key, value)?;
            }
            Ok(stats)
        })?,
    )?;
    kong.set("nginx", nginx)?;

    let connector = lua.create_table()?;
    connector.set(
        "connect",
        lua.create_function(|_, _: ()| -> LuaResult<bool> { Ok(true) })?,
    )?;
    let db = lua.create_table()?;
    db.set("connector", connector)?;
    kong.set("db", db)?;

    let configuration = lua.create_table()?;
    configuration.set("role", "traditional")?;
    configuration.set("cluster_cert", "")?;
    configuration.set("stream_listeners", lua.create_table()?)?;
    kong.set("configuration", configuration)?;

    globals.set("kong", kong)?;
    Ok(())
}

/// Sync Lua-side mutations back into the Rust request context. — 将 Lua 侧修改同步回 Rust 请求上下文。
pub fn sync_ctx_from_lua(lua: &Lua, ctx: &mut RequestCtx) -> LuaResult<()> {
    let globals = lua.globals();

    if let Ok(kong) = globals.get::<LuaTable>("kong") {
        if let Ok(ctx_table) = kong.get::<LuaTable>("ctx") {
            if let Ok(shared) = ctx_table.get::<LuaTable>("shared") {
                for pair in shared.pairs::<String, LuaValue>() {
                    if let Ok((key, value)) = pair {
                        if let Ok(json_value) = lua.from_value::<serde_json::Value>(value) {
                            ctx.shared.insert(key, json_value);
                        }
                    }
                }
            }
        }
    }

    if let Ok(ngx) = globals.get::<LuaTable>("ngx") {
        if let Ok(ngx_ctx) = ngx.get::<LuaTable>("ctx") {
            if let Ok(value) = lua.from_value::<serde_json::Value>(LuaValue::Table(ngx_ctx.clone())) {
                ctx.shared.insert("__ngx_ctx_state".to_string(), value);
            }

            if let Ok(source_map) = ngx_ctx.get::<LuaTable>("ai_namespaced_ctx_global_source") {
                if let Ok(value) = lua.from_value::<serde_json::Value>(LuaValue::Table(source_map)) {
                    ctx.shared
                        .insert("__ngx_ctx_global_source".to_string(), value);
                }
            }
        }
    }

    if let Ok(true) = globals.get::<bool>("__kong_short_circuited") {
        ctx.short_circuited = true;
        ctx.exit_status = globals
            .get::<Option<u16>>("__kong_exit_status")
            .ok()
            .flatten();
        ctx.exit_body = globals
            .get::<Option<String>>("__kong_exit_body")
            .ok()
            .flatten();

        if let Ok(exit_headers) = globals.get::<LuaTable>("__kong_exit_headers") {
            let mut headers = std::collections::HashMap::new();
            for pair in exit_headers.pairs::<String, String>() {
                if let Ok((key, value)) = pair {
                    headers.insert(key, value);
                }
            }
            if !headers.is_empty() {
                ctx.exit_headers = Some(headers);
            }
        }
    }

    if let Ok(headers_set) = globals.get::<LuaTable>("__kong_upstream_headers_set") {
        for pair in headers_set.pairs::<String, String>() {
            if let Ok((name, value)) = pair {
                ctx.upstream_headers_to_set.push((name, value));
            }
        }
    }
    if let Ok(headers_remove) = globals.get::<LuaTable>("__kong_upstream_headers_remove") {
        for pair in headers_remove.pairs::<i64, String>() {
            if let Ok((_index, name)) = pair {
                ctx.upstream_headers_to_remove.push(name);
            }
        }
    }
    if let Ok(query_set) = globals.get::<LuaTable>("__kong_upstream_query_set") {
        let mut query = HashMap::new();
        for pair in query_set.pairs::<String, String>() {
            if let Ok((name, value)) = pair {
                query.insert(name, value);
            }
        }
        if !query.is_empty() {
            ctx.upstream_query_to_set = Some(query);
        }
    }
    ctx.upstream_path = globals
        .get::<Option<String>>("__kong_upstream_path")
        .ok()
        .flatten();
    ctx.upstream_scheme = globals
        .get::<Option<String>>("__kong_upstream_scheme")
        .ok()
        .flatten();
    ctx.upstream_target_host = globals
        .get::<Option<String>>("__kong_upstream_target_host")
        .ok()
        .flatten();
    ctx.upstream_target_port = globals
        .get::<Option<i64>>("__kong_upstream_target_port")
        .ok()
        .flatten()
        .map(|port| port as u16);
    ctx.upstream_body = globals
        .get::<Option<String>>("__kong_upstream_body")
        .ok()
        .flatten();
    ctx.request_buffering_enabled = globals
        .get::<bool>("__kong_request_buffering_enabled")
        .unwrap_or(false);
    ctx.upstream_retry_callback_registered = globals
        .get::<bool>("__kong_retry_callback_registered")
        .unwrap_or(false);

    if let Ok(headers_set) = globals.get::<LuaTable>("__kong_response_headers_set") {
        for pair in headers_set.pairs::<String, String>() {
            if let Ok((name, value)) = pair {
                ctx.response_headers_to_set.push((name, value));
            }
        }
    }
    if let Ok(headers_remove) = globals.get::<LuaTable>("__kong_response_headers_remove") {
        for pair in headers_remove.pairs::<i64, String>() {
            if let Ok((_index, name)) = pair {
                ctx.response_headers_to_remove.push(name);
            }
        }
    }

    Ok(())
}
