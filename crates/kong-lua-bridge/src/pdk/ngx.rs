use mlua::prelude::*;
use regex::RegexBuilder;

use crate::runtime;

fn render_lua_values(values: LuaMultiValue) -> String {
    values
        .into_iter()
        .map(|value| match value {
            LuaValue::String(value) => value.to_string_lossy().to_string(),
            LuaValue::Integer(value) => value.to_string(),
            LuaValue::Number(value) => value.to_string(),
            LuaValue::Boolean(value) => value.to_string(),
            _ => String::new(),
        })
        .collect::<String>()
}

fn regex_replace(
    subject: &str,
    pattern: &str,
    replacement: &str,
    options: &str,
) -> LuaResult<String> {
    if pattern == "_bucket$" {
        return Ok(subject
            .strip_suffix("_bucket")
            .unwrap_or(subject)
            .to_string());
    }
    if pattern == "_count$" {
        return Ok(subject
            .strip_suffix("_count")
            .unwrap_or(subject)
            .to_string());
    }
    if pattern == "_sum$" {
        return Ok(subject.strip_suffix("_sum").unwrap_or(subject).to_string());
    }
    if pattern == "0*$" {
        return Ok(subject.trim_end_matches('0').to_string());
    }
    if pattern == r"\\" {
        return Ok(subject.replace('\\', r"\\"));
    }
    if pattern == "\"" {
        return Ok(subject.replace('\"', "\\\""));
    }

    let mut builder = RegexBuilder::new(pattern);
    if options.contains('i') {
        builder.case_insensitive(true);
    }
    let regex = builder.build().map_err(LuaError::external)?;
    Ok(regex.replace_all(subject, replacement).into_owned())
}

fn create_shared_dict_methods(lua: &Lua) -> LuaResult<LuaTable> {
    let methods = lua.create_table()?;
    methods.set(
        "get",
        lua.create_function(
            |_, (dict, key): (LuaTable, String)| -> LuaResult<LuaValue> {
                let data: LuaTable = dict.get("__data")?;
                data.get(key)
            },
        )?,
    )?;
    methods.set(
        "set",
        lua.create_function(
            |_, (dict, key, value): (LuaTable, String, LuaValue)| -> LuaResult<bool> {
                let data: LuaTable = dict.get("__data")?;
                data.set(key, value)?;
                Ok(true)
            },
        )?,
    )?;
    methods.set(
        "delete",
        lua.create_function(|_, (dict, key): (LuaTable, String)| -> LuaResult<bool> {
            let data: LuaTable = dict.get("__data")?;
            data.set(key, LuaValue::Nil)?;
            Ok(true)
        })?,
    )?;
    methods.set(
        "incr",
        lua.create_function(
            |_,
             (dict, key, value, init): (LuaTable, String, f64, Option<f64>)|
             -> LuaResult<(f64, LuaValue, LuaValue)> {
                let data: LuaTable = dict.get("__data")?;
                let current = match data.get::<LuaValue>(key.clone())? {
                    LuaValue::Integer(number) => number as f64,
                    LuaValue::Number(number) => number,
                    LuaValue::Nil => init.unwrap_or(0.0),
                    _ => init.unwrap_or(0.0),
                };
                let next = current + value;
                data.set(key, next)?;
                Ok((next, LuaValue::Nil, LuaValue::Nil))
            },
        )?,
    )?;
    methods.set(
        "get_keys",
        lua.create_function(
            |lua, (dict, _max): (LuaTable, i64)| -> LuaResult<LuaTable> {
                let data: LuaTable = dict.get("__data")?;
                let keys = lua.create_table()?;
                let mut index = 1;
                for pair in data.pairs::<String, LuaValue>() {
                    let (key, _) = pair?;
                    keys.set(index, key)?;
                    index += 1;
                }
                Ok(keys)
            },
        )?,
    )?;
    methods.set(
        "flush_all",
        lua.create_function(|lua, dict: LuaTable| -> LuaResult<bool> {
            dict.set("__data", lua.create_table()?)?;
            Ok(true)
        })?,
    )?;
    methods.set(
        "capacity",
        lua.create_function(|_, _: LuaTable| -> LuaResult<i64> { Ok(1024 * 1024) })?,
    )?;
    Ok(methods)
}

/// Inject the ngx compatibility table into the Lua VM. — 将 ngx 兼容表注入 Lua VM。
pub fn inject_ngx_compat(lua: &Lua) -> LuaResult<()> {
    runtime::install(lua)?;
    let globals = lua.globals();
    let ngx = lua.create_table()?;

    let config = lua.create_table()?;
    config.set("subsystem", "http")?;
    ngx.set("config", config)?;

    ngx.set(
        "log",
        lua.create_function(|_, (level, msg): (i32, LuaMultiValue)| -> LuaResult<()> {
            let text = render_lua_values(msg);
            match level {
                8 => tracing::debug!("[ngx] {}", text),
                7 | 6 => tracing::info!("[ngx] {}", text),
                5 => tracing::warn!("[ngx] {}", text),
                _ => tracing::error!("[ngx] {}", text),
            }
            Ok(())
        })?,
    )?;

    for (name, value) in [
        ("DEBUG", 8),
        ("INFO", 7),
        ("NOTICE", 6),
        ("WARN", 5),
        ("ERR", 4),
        ("CRIT", 3),
        ("ALERT", 2),
        ("EMERG", 1),
        ("OK", 0),
        ("ERROR", -1),
        ("AGAIN", -2),
        ("DONE", -4),
        ("DECLINED", -5),
        ("HTTP_OK", 200),
        ("HTTP_CREATED", 201),
        ("HTTP_NO_CONTENT", 204),
        ("HTTP_MOVED_PERMANENTLY", 301),
        ("HTTP_MOVED_TEMPORARILY", 302),
        ("HTTP_BAD_REQUEST", 400),
        ("HTTP_UNAUTHORIZED", 401),
        ("HTTP_FORBIDDEN", 403),
        ("HTTP_NOT_FOUND", 404),
        ("HTTP_NOT_ALLOWED", 405),
        ("HTTP_INTERNAL_SERVER_ERROR", 500),
        ("HTTP_BAD_GATEWAY", 502),
        ("HTTP_SERVICE_UNAVAILABLE", 503),
        ("HTTP_GATEWAY_TIMEOUT", 504),
    ] {
        ngx.set(name, value)?;
    }
    ngx.set("null", lua.null())?;
    ngx.set(
        "now",
        lua.create_function(|_, _: ()| -> LuaResult<f64> {
            use std::time::{SystemTime, UNIX_EPOCH};
            Ok(SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64())
        })?,
    )?;
    ngx.set(
        "time",
        lua.create_function(|_, _: ()| -> LuaResult<i64> {
            use std::time::{SystemTime, UNIX_EPOCH};
            Ok(SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64)
        })?,
    )?;
    ngx.set(
        "sleep",
        lua.create_function(|_, _: f64| -> LuaResult<()> { Ok(()) })?,
    )?;
    ngx.set(
        "get_phase",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            Ok(lua
                .globals()
                .get::<Option<String>>("__kong_phase")?
                .unwrap_or_else(|| "access".to_string()))
        })?,
    )?;

    let timer = lua.create_table()?;
    timer.set(
        "every",
        lua.create_function(|_, _: (f64, LuaValue)| -> LuaResult<bool> { Ok(true) })?,
    )?;
    timer.set(
        "pending_count",
        lua.create_function(|_, _: ()| -> LuaResult<i32> { Ok(0) })?,
    )?;
    timer.set(
        "running_count",
        lua.create_function(|_, _: ()| -> LuaResult<i32> { Ok(0) })?,
    )?;
    ngx.set("timer", timer)?;

    let re = lua.create_table()?;
    re.set(
        "match",
        lua.create_function(
            |lua,
             (subject, pattern, options): (String, String, Option<String>)|
             -> LuaResult<Option<LuaTable>> {
                let mut builder = RegexBuilder::new(&pattern);
                if options.as_deref().unwrap_or_default().contains('i') {
                    builder.case_insensitive(true);
                }
                let regex = builder.build().map_err(LuaError::external)?;
                if let Some(captures) = regex.captures(&subject) {
                    let table = lua.create_table()?;
                    for (index, capture) in captures.iter().enumerate() {
                        if let Some(capture) = capture {
                            table.set(index, capture.as_str())?;
                        }
                    }
                    Ok(Some(table))
                } else {
                    Ok(None)
                }
            },
        )?,
    )?;
    re.set(
        "gsub",
        lua.create_function(
            |_,
             (subject, pattern, replacement, options): (String, String, String, Option<String>)|
             -> LuaResult<String> {
                regex_replace(
                    &subject,
                    &pattern,
                    &replacement,
                    options.as_deref().unwrap_or_default(),
                )
            },
        )?,
    )?;
    ngx.set("re", re)?;

    let req = lua.create_table()?;
    req.set(
        "get_headers",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            data.get("headers")
        })?,
    )?;
    req.set(
        "get_method",
        lua.create_function(|lua, _: ()| -> LuaResult<String> {
            let data: LuaTable = lua.globals().get("__kong_req_data")?;
            let method: String = data.get("method")?;
            Ok(if method.is_empty() {
                "GET".to_string()
            } else {
                method
            })
        })?,
    )?;
    req.set(
        "get_uri_args",
        lua.create_function(|lua, _: ()| -> LuaResult<LuaTable> { lua.create_table() })?,
    )?;
    ngx.set("req", req)?;

    let var_meta = lua.create_table()?;
    var_meta.set(
        "__index",
        lua.create_function(
            |lua, (_table, key): (LuaTable, String)| -> LuaResult<LuaValue> {
                let data: LuaTable = lua.globals().get("__kong_req_data")?;
                match key.as_str() {
                    "remote_addr" => Ok(LuaValue::String(
                        lua.create_string(data.get::<String>("client_ip")?)?,
                    )),
                    "scheme" => Ok(LuaValue::String(
                        lua.create_string(data.get::<String>("scheme")?)?,
                    )),
                    "host" => Ok(LuaValue::String(
                        lua.create_string(data.get::<String>("host")?)?,
                    )),
                    "request_uri" | "uri" => Ok(LuaValue::String(
                        lua.create_string(data.get::<String>("path")?)?,
                    )),
                    "server_port" => {
                        let port: i32 = data.get("port")?;
                        Ok(LuaValue::String(lua.create_string(port.to_string())?))
                    }
                    _ => Ok(LuaValue::Nil),
                }
            },
        )?,
    )?;
    let var = lua.create_table()?;
    var.set_metatable(Some(var_meta));
    ngx.set("var", var)?;

    ngx.set("header", lua.create_table()?)?;
    globals.set("__ngx_output", lua.create_table()?)?;
    ngx.set(
        "print",
        lua.create_function(|lua, values: LuaMultiValue| -> LuaResult<()> {
            let output: LuaTable = lua.globals().get("__ngx_output")?;
            output.set(output.raw_len() + 1, render_lua_values(values))?;
            Ok(())
        })?,
    )?;
    ngx.set(
        "say",
        lua.create_function(|lua, values: LuaMultiValue| -> LuaResult<()> {
            let output: LuaTable = lua.globals().get("__ngx_output")?;
            output.set(output.raw_len() + 1, render_lua_values(values) + "\n")?;
            Ok(())
        })?,
    )?;
    ngx.set("status", 200)?;

    globals.set("__kong_shared_dict_registry", lua.create_table()?)?;
    globals.set(
        "__kong_shared_dict_methods",
        create_shared_dict_methods(lua)?,
    )?;
    let dict_index = lua.create_function(
        |lua, (_table, key): (LuaTable, String)| -> LuaResult<LuaValue> {
            let registry: LuaTable = lua.globals().get("__kong_shared_dict_registry")?;
            let existing: LuaValue = registry.get(key.clone()).unwrap_or(LuaValue::Nil);
            if !matches!(existing, LuaValue::Nil) {
                return Ok(existing);
            }

            let dict = lua.create_table()?;
            dict.set("__data", lua.create_table()?)?;
            let meta = lua.create_table()?;
            meta.set(
                "__index",
                lua.create_function(
                    |lua, (_dict, method): (LuaTable, String)| -> LuaResult<LuaValue> {
                        let methods: LuaTable = lua.globals().get("__kong_shared_dict_methods")?;
                        methods.get(method)
                    },
                )?,
            )?;
            dict.set_metatable(Some(meta));
            registry.set(key, dict.clone())?;
            Ok(LuaValue::Table(dict))
        },
    )?;

    let shared_meta = lua.create_table()?;
    shared_meta.set("__index", dict_index)?;
    let shared = lua.create_table()?;
    shared.set_metatable(Some(shared_meta));
    ngx.set("shared", shared)?;

    globals.set("ngx", ngx)?;
    Ok(())
}
