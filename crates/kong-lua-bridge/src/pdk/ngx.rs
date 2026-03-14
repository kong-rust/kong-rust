use mlua::prelude::*;
use mlua::LuaSerdeExt;
use regex::RegexBuilder;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::runtime;

#[derive(Clone)]
enum SharedValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

type SharedDictData = HashMap<String, SharedValue>;
type SharedDictRegistry = HashMap<String, SharedDictData>;

fn shared_dict_registry() -> &'static Mutex<SharedDictRegistry> {
    static REGISTRY: OnceLock<Mutex<SharedDictRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lua_value_to_shared(value: LuaValue) -> LuaResult<Option<SharedValue>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(value) => Ok(Some(SharedValue::String(
            value.to_string_lossy().to_string(),
        ))),
        LuaValue::Integer(value) => Ok(Some(SharedValue::Number(value as f64))),
        LuaValue::Number(value) => Ok(Some(SharedValue::Number(value))),
        LuaValue::Boolean(value) => Ok(Some(SharedValue::Boolean(value))),
        _ => Err(LuaError::external(
            "unsupported ngx.shared value type — 不支持的 ngx.shared 值类型",
        )),
    }
}

fn shared_to_lua_value(lua: &Lua, value: &SharedValue) -> LuaResult<LuaValue> {
    match value {
        SharedValue::String(value) => Ok(LuaValue::String(lua.create_string(value)?)),
        SharedValue::Number(value) => Ok(LuaValue::Number(*value)),
        SharedValue::Boolean(value) => Ok(LuaValue::Boolean(*value)),
    }
}

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
            |lua, (dict, key): (LuaTable, String)| -> LuaResult<LuaValue> {
                let dict_name: String = dict.get("__name")?;
                let registry = shared_dict_registry()
                    .lock()
                    .map_err(|_| LuaError::external("shared dict registry poisoned"))?;
                let Some(data) = registry.get(&dict_name) else {
                    return Ok(LuaValue::Nil);
                };
                let Some(value) = data.get(&key) else {
                    return Ok(LuaValue::Nil);
                };
                shared_to_lua_value(lua, value)
            },
        )?,
    )?;
    methods.set(
        "set",
        lua.create_function(
            |_, (dict, key, value): (LuaTable, String, LuaValue)| -> LuaResult<bool> {
                let dict_name: String = dict.get("__name")?;
                let mut registry = shared_dict_registry()
                    .lock()
                    .map_err(|_| LuaError::external("shared dict registry poisoned"))?;
                let data = registry.entry(dict_name).or_default();
                if let Some(value) = lua_value_to_shared(value)? {
                    data.insert(key, value);
                } else {
                    data.remove(&key);
                }
                Ok(true)
            },
        )?,
    )?;
    methods.set(
        "delete",
        lua.create_function(|_, (dict, key): (LuaTable, String)| -> LuaResult<bool> {
            let dict_name: String = dict.get("__name")?;
            let mut registry = shared_dict_registry()
                .lock()
                .map_err(|_| LuaError::external("shared dict registry poisoned"))?;
            if let Some(data) = registry.get_mut(&dict_name) {
                data.remove(&key);
            }
            Ok(true)
        })?,
    )?;
    methods.set(
        "incr",
        lua.create_function(
            |_,
             (dict, key, value, init): (LuaTable, String, f64, Option<f64>)|
             -> LuaResult<(f64, LuaValue, LuaValue)> {
                let dict_name: String = dict.get("__name")?;
                let mut registry = shared_dict_registry()
                    .lock()
                    .map_err(|_| LuaError::external("shared dict registry poisoned"))?;
                let data = registry.entry(dict_name).or_default();
                let current = match data.get(&key) {
                    Some(SharedValue::Number(number)) => *number,
                    Some(SharedValue::String(number)) => {
                        number.parse::<f64>().unwrap_or(init.unwrap_or(0.0))
                    }
                    Some(SharedValue::Boolean(number)) => {
                        if *number {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    None => init.unwrap_or(0.0),
                };
                let next = current + value;
                data.insert(key, SharedValue::Number(next));
                Ok((next, LuaValue::Nil, LuaValue::Nil))
            },
        )?,
    )?;
    methods.set(
        "get_keys",
        lua.create_function(
            |lua, (dict, _max): (LuaTable, i64)| -> LuaResult<LuaTable> {
                let dict_name: String = dict.get("__name")?;
                let registry = shared_dict_registry()
                    .lock()
                    .map_err(|_| LuaError::external("shared dict registry poisoned"))?;
                let keys = lua.create_table()?;
                let mut index = 1;
                if let Some(data) = registry.get(&dict_name) {
                    for key in data.keys() {
                        keys.set(index, key.clone())?;
                        index += 1;
                    }
                }
                Ok(keys)
            },
        )?,
    )?;
    methods.set(
        "flush_all",
        lua.create_function(|_, dict: LuaTable| -> LuaResult<bool> {
            let dict_name: String = dict.get("__name")?;
            let mut registry = shared_dict_registry()
                .lock()
                .map_err(|_| LuaError::external("shared dict registry poisoned"))?;
            registry.insert(dict_name, HashMap::new());
            Ok(true)
        })?,
    )?;
    methods.set(
        "capacity",
        lua.create_function(|_, _: LuaTable| -> LuaResult<i64> { Ok(1024 * 1024) })?,
    )?;
    Ok(methods)
}

/// Seed ngx.arg for body_filter execution. — 为 body_filter 执行预置 ngx.arg。
pub fn set_body_filter_args(lua: &Lua, body: Option<&[u8]>, end_of_stream: bool) -> LuaResult<()> {
    let globals = lua.globals();
    match body {
        Some(body) => globals.set("__ngx_arg_1", lua.create_string(body)?)?,
        None => globals.set("__ngx_arg_1", LuaValue::Nil)?,
    }
    globals.set("__ngx_arg_2", end_of_stream)?;
    Ok(())
}

/// Read ngx.arg after body_filter execution. — 读取 body_filter 执行后的 ngx.arg。
pub fn read_body_filter_args(lua: &Lua) -> LuaResult<(Option<Vec<u8>>, bool)> {
    let globals = lua.globals();
    let body = match globals.get::<LuaValue>("__ngx_arg_1")? {
        LuaValue::Nil => None,
        LuaValue::String(value) => Some(value.as_bytes().to_vec()),
        value => Some(render_lua_values(LuaMultiValue::from_vec(vec![value])).into_bytes()),
    };
    let end_of_stream = globals.get::<bool>("__ngx_arg_2").unwrap_or(false);
    Ok((body, end_of_stream))
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
        "update_time",
        lua.create_function(|_, _: ()| -> LuaResult<()> { Ok(()) })?,
    )?;
    ngx.set(
        "sleep",
        lua.create_function(|_, _: f64| -> LuaResult<()> { Ok(()) })?,
    )?;
    ngx.set(
        "exit",
        lua.create_function(|_, code: i32| -> LuaResult<i32> { Ok(code) })?,
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
    globals.set("__ngx_arg_1", LuaValue::Nil)?;
    globals.set("__ngx_arg_2", false)?;
    let arg_meta = lua.create_table()?;
    arg_meta.set(
        "__index",
        lua.create_function(|lua, (_table, key): (LuaTable, LuaValue)| -> LuaResult<LuaValue> {
            let globals = lua.globals();
            match key {
                LuaValue::Integer(1) | LuaValue::Number(1.0) => globals.get("__ngx_arg_1"),
                LuaValue::Integer(2) | LuaValue::Number(2.0) => globals.get("__ngx_arg_2"),
                _ => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    arg_meta.set(
        "__newindex",
        lua.create_function(|lua, (_table, key, value): (LuaTable, LuaValue, LuaValue)| {
            let globals = lua.globals();
            match key {
                LuaValue::Integer(1) | LuaValue::Number(1.0) => globals.set("__ngx_arg_1", value),
                LuaValue::Integer(2) | LuaValue::Number(2.0) => globals.set("__ngx_arg_2", value),
                _ => Ok(()),
            }
        })?,
    )?;
    let arg = lua.create_table()?;
    arg.set_metatable(Some(arg_meta));
    ngx.set("arg", arg)?;
    let persisted_ctx = match globals.get::<LuaValue>("__persisted_ngx_ctx")? {
        LuaValue::Table(table) => table,
        LuaValue::Nil => lua.create_table()?,
        value => lua
            .from_value::<serde_json::Value>(value)
            .ok()
            .map(|value| lua.to_value(&value))
            .transpose()?
            .and_then(|value| match value {
                LuaValue::Table(table) => Some(table),
                _ => None,
            })
            .unwrap_or(lua.create_table()?),
    };
    ngx.set("ctx", persisted_ctx)?;
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
            dict.set("__name", key.clone())?;
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
