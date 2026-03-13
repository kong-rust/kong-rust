use kong_core::traits::RequestCtx;
use mlua::prelude::*;
use mlua::LuaSerdeExt;

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
    globals.set("__kong_resp_data", resp_data)?;

    globals.set("__kong_short_circuited", false)?;
    globals.set("__kong_exit_status", LuaValue::Nil)?;
    globals.set("__kong_exit_body", LuaValue::Nil)?;
    globals.set("__kong_exit_headers", lua.create_table()?)?;
    globals.set("__kong_upstream_headers_set", lua.create_table()?)?;
    globals.set("__kong_upstream_headers_remove", lua.create_table()?)?;
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
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> { lua.create_table() })?,
    )?;
    request.set(
        "get_raw_query",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("query_string")
        })?,
    )?;
    kong.set("request", request)?;

    let response = lua.create_table()?;
    response.set(
        "exit",
        lua.create_function(
            |lua,
             (status, body, headers): (u16, Option<String>, Option<LuaTable>)|
             -> LuaResult<()> {
                let globals = lua.globals();
                globals.set("__kong_short_circuited", true)?;
                globals.set("__kong_exit_status", status)?;
                if let Some(body) = body {
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
        lua.create_function(|lua, (name, value): (String, String)| -> LuaResult<()> {
            let headers_set: LuaTable = lua.globals().get("__kong_response_headers_set")?;
            headers_set.set(name, value)?;
            Ok(())
        })?,
    )?;
    response.set(
        "add_header",
        lua.create_function(|lua, (name, value): (String, String)| -> LuaResult<()> {
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
    kong.set("response", response)?;

    let service = lua.create_table()?;
    let service_request = lua.create_table()?;
    service_request.set(
        "set_header",
        lua.create_function(|lua, (name, value): (String, String)| -> LuaResult<()> {
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
        lua.create_function(|_, _: String| -> LuaResult<()> { Ok(()) })?,
    )?;
    service.set("request", service_request)?;

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
    service.set("response", service_response)?;
    kong.set("service", service)?;

    let log = lua.create_table()?;
    for (name, level) in [
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
    node.set(
        "get_id",
        lua.create_function(|_, _: ()| Ok("00000000-0000-0000-0000-000000000001".to_string()))?,
    )?;
    node.set(
        "get_hostname",
        lua.create_function(|_, _: ()| Ok("localhost".to_string()))?,
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
            for (key, value) in [
                ("connections_accepted", 0),
                ("connections_handled", 0),
                ("total_requests", 0),
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
