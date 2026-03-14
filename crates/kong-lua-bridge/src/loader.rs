//! Lua plugin loader — scans and loads Kong Lua plugins — Lua 插件加载器 — 扫描并加载 Kong Lua 插件
//!
//! Loads Lua plugins from Kong's plugin directories: — 从 Kong 的插件目录加载 Lua 插件:
//! - Reads handler.lua for priority and version — 读取 handler.lua 获取优先级和版本
//! - Detects supported phases — 检测插件支持的阶段
//! - Creates LuaPluginHandler instances — 创建 LuaPluginHandler 实例

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kong_core::error::{KongError, Result};
use kong_core::traits::{Phase, PluginHandler};
use mlua::prelude::*;
use serde_json::{Map, Number, Value};

use crate::{runtime, LuaPluginHandler};

/// Resolve candidate plugin directories for the current runtime. — 解析当前运行时可用的插件目录候选列表。
pub fn resolve_plugin_dirs(kong_prefix: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(kong_prefix).join("plugins"),
        PathBuf::from(kong_prefix).join("kong/plugins"),
        PathBuf::from("/usr/local/kong/plugins"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../kong-lua-bridge/kong/plugins"),
    ];

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("crates/kong-lua-bridge/kong/plugins"));
    }

    let mut resolved = Vec::new();
    for candidate in candidates {
        if candidate.exists() && !resolved.contains(&candidate) {
            resolved.push(candidate);
        }
    }

    resolved
}

/// Scan and load Kong Lua plugins from the specified directories — 扫描并加载指定目录下的 Kong Lua 插件
pub fn load_lua_plugins(
    plugin_dirs: &[PathBuf],
    plugin_names: &[String],
) -> Result<Vec<LuaPluginHandler>> {
    let mut handlers = Vec::new();

    for name in plugin_names {
        for dir in plugin_dirs {
            let plugin_path = dir.join(name);
            let handler_file = plugin_path.join("handler.lua");

            if handler_file.exists() {
                match load_single_plugin(name, &plugin_path) {
                    Ok(handler) => {
                        tracing::info!("加载 Lua 插件: {} (优先级: {})", name, handler.priority());
                        handlers.push(handler);
                        break; // Found it, stop searching other directories — 找到就不继续搜索其他目录
                    }
                    Err(e) => {
                        tracing::warn!("加载 Lua 插件 {} 失败: {}", name, e);
                    }
                }
            }
        }
    }

    Ok(handlers)
}

/// Load a plugin schema.lua and convert it into JSON. — 加载插件 schema.lua 并将其转换为 JSON。
pub fn load_plugin_schema(plugin_dirs: &[PathBuf], plugin_name: &str) -> Result<Value> {
    let plugin_path =
        find_plugin_path(plugin_dirs, plugin_name).ok_or_else(|| KongError::NotFound {
            entity_type: "plugin schema".to_string(),
            id: plugin_name.to_string(),
        })?;

    let schema_file = plugin_path.join("schema.lua");
    let schema_code = std::fs::read_to_string(&schema_file).map_err(|e| {
        KongError::LuaError(format!(
            "Failed to read {}: {} — 读取 {} 失败: {}",
            schema_file.display(),
            e,
            schema_file.display(),
            e
        ))
    })?;

    let lua = unsafe { Lua::unsafe_new() };
    runtime::configure_package_path(&lua, &plugin_path)
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::set_phase(&lua, "init").map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::install(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;
    crate::pdk::inject_ngx_compat(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;

    let schema_value: LuaValue = lua.load(&schema_code).eval().map_err(|e| {
        KongError::LuaError(format!(
            "Failed to load schema.lua: {} — 加载 schema.lua 失败: {}",
            e, e
        ))
    })?;

    lua_value_to_json(schema_value).map_err(|e| {
        KongError::LuaError(format!(
            "Failed to convert plugin schema to JSON: {} — 将插件 schema 转为 JSON 失败: {}",
            e, e
        ))
    })
}

fn find_plugin_path(plugin_dirs: &[PathBuf], plugin_name: &str) -> Option<PathBuf> {
    for dir in plugin_dirs {
        let plugin_path = dir.join(plugin_name);
        if plugin_path.join("schema.lua").exists() {
            return Some(plugin_path);
        }
    }
    None
}

fn lua_value_to_json(value: LuaValue) -> LuaResult<Value> {
    match value {
        LuaValue::Nil => Ok(Value::Null),
        LuaValue::Boolean(value) => Ok(Value::Bool(value)),
        LuaValue::Integer(value) => Ok(Value::Number(Number::from(value))),
        LuaValue::Number(value) => Ok(Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null)),
        LuaValue::String(value) => Ok(Value::String(value.to_string_lossy().to_string())),
        LuaValue::Table(table) => table_to_json(table),
        LuaValue::Function(_) => Ok(Value::String("[function]".to_string())),
        LuaValue::Thread(_) => Ok(Value::String("[thread]".to_string())),
        LuaValue::UserData(_)
        | LuaValue::LightUserData(_)
        | LuaValue::Error(_)
        | LuaValue::Other(_) => Ok(Value::String("[userdata]".to_string())),
    }
}

fn table_to_json(table: LuaTable) -> LuaResult<Value> {
    let len = table.raw_len();
    let mut is_array = len > 0;

    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        match key {
            LuaValue::Integer(index) if index >= 1 && (index as usize) <= len => {}
            LuaValue::Number(index)
                if index.fract() == 0.0 && index >= 1.0 && (index as usize) <= len => {}
            _ => {
                is_array = false;
                break;
            }
        }
    }

    if is_array {
        let mut items = Vec::with_capacity(len);
        for index in 1..=len {
            items.push(lua_value_to_json(table.raw_get(index)?)?);
        }
        return Ok(Value::Array(items));
    }

    let mut object = Map::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        let key = match key {
            LuaValue::String(value) => value.to_string_lossy().to_string(),
            LuaValue::Integer(value) => value.to_string(),
            LuaValue::Number(value) => value.to_string(),
            LuaValue::Boolean(value) => value.to_string(),
            _ => continue,
        };
        object.insert(key, lua_value_to_json(value)?);
    }

    Ok(Value::Object(object))
}

/// Load a single Lua plugin — 加载单个 Lua 插件
fn load_single_plugin(name: &str, plugin_path: &Path) -> Result<LuaPluginHandler> {
    let handler_file = plugin_path.join("handler.lua");
    let handler_code = std::fs::read_to_string(&handler_file).map_err(|e| {
        KongError::LuaError(format!(
            "Failed to read {}: {} — 读取 {} 失败: {}",
            handler_file.display(),
            e,
            handler_file.display(),
            e
        ))
    })?;

    // Extract priority and version from handler.lua — 从 handler.lua 中提取优先级和版本
    let priority = extract_priority(&handler_code).unwrap_or(1000);
    let version = extract_version(&handler_code).unwrap_or_else(|| "0.1.0".to_string());

    // Detect supported phases. Prefer evaluating handler.lua so meta-plugins like ai-proxy
    // expose their generated lifecycle methods correctly. — 检测支持的阶段。优先执行 handler.lua，
    // 这样 ai-proxy 这类元插件动态生成的生命周期方法也能被正确识别。
    let phases = detect_runtime_phases(plugin_path, &handler_code).unwrap_or_else(|err| {
        tracing::warn!(
            "runtime phase detection failed for plugin {}: {} — 插件 {} 的运行时 phase 检测失败: {}",
            name,
            err,
            name,
            err
        );
        detect_phases(&handler_code)
    });

    Ok(LuaPluginHandler::new(
        name.to_string(),
        priority,
        version,
        plugin_path.to_path_buf(),
        phases,
    ))
}

fn detect_runtime_phases(plugin_path: &Path, handler_code: &str) -> Result<HashMap<Phase, bool>> {
    let lua = unsafe { Lua::unsafe_new() };
    runtime::configure_package_path(&lua, &plugin_path.to_path_buf())
        .map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::set_phase(&lua, "init").map_err(|e| KongError::LuaError(e.to_string()))?;
    runtime::install(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;
    crate::pdk::inject_ngx_compat(&lua).map_err(|e| KongError::LuaError(e.to_string()))?;

    let handler_table: LuaTable = lua.load(handler_code).eval().map_err(|e| {
        KongError::LuaError(format!(
            "Failed to load handler.lua for runtime phase detection: {} — 运行时 phase 检测加载 handler.lua 失败: {}",
            e, e
        ))
    })?;

    let mut phases = HashMap::new();
    for (phase, key) in [
        (Phase::InitWorker, "init_worker"),
        (Phase::Certificate, "certificate"),
        (Phase::Rewrite, "rewrite"),
        (Phase::Access, "access"),
        (Phase::Response, "response"),
        (Phase::HeaderFilter, "header_filter"),
        (Phase::BodyFilter, "body_filter"),
        (Phase::Log, "log"),
    ] {
        phases.insert(
            phase,
            matches!(
                handler_table.get::<LuaValue>(key),
                Ok(LuaValue::Function(_))
            ),
        );
    }

    Ok(phases)
}

/// Extract plugin priority from handler.lua source code — 从 handler.lua 源码中提取插件优先级
///
/// Kong plugins are typically defined as: — Kong 插件通常定义为:
/// ```lua
/// local MyPlugin = {
///     PRIORITY = 1000,
///     VERSION  = "1.0.0",
/// }
/// ```
pub fn extract_priority(code: &str) -> Option<i32> {
    // Match PRIORITY = number or PRIORITY=number — 匹配 PRIORITY = 数字 或 PRIORITY=数字
    for line in code.lines() {
        let line = line.trim();
        if let Some(pos) = line.find("PRIORITY") {
            let rest = &line[pos + 8..];
            let rest = rest.trim_start_matches(|c: char| c == ' ' || c == '=');
            if let Some(end) = rest.find(|c: char| !c.is_ascii_digit() && c != '-') {
                if let Ok(p) = rest[..end].parse::<i32>() {
                    return Some(p);
                }
            } else if let Ok(p) = rest.trim_end_matches(',').trim().parse::<i32>() {
                return Some(p);
            }
        }
    }
    None
}

/// Extract plugin version from handler.lua source code — 从 handler.lua 源码中提取插件版本
pub fn extract_version(code: &str) -> Option<String> {
    for line in code.lines() {
        let line = line.trim();
        if let Some(pos) = line.find("VERSION") {
            let rest = &line[pos + 7..];
            // Find the version string within quotes — 查找引号内的版本号
            if let Some(start) = rest.find('"') {
                let version_part = &rest[start + 1..];
                if let Some(end) = version_part.find('"') {
                    return Some(version_part[..end].to_string());
                }
            }
        }
    }
    None
}

/// Detect phases supported by a Lua plugin — 检测 Lua 插件支持的阶段
pub fn detect_phases(code: &str) -> HashMap<Phase, bool> {
    let mut phases = HashMap::new();

    let phase_map = [
        ("function%s*%w+[.:]access", Phase::Access),
        ("function%s*%w+[.:]rewrite", Phase::Rewrite),
        ("function%s*%w+[.:]header_filter", Phase::HeaderFilter),
        ("function%s*%w+[.:]body_filter", Phase::BodyFilter),
        ("function%s*%w+[.:]log", Phase::Log),
        ("function%s*%w+[.:]certificate", Phase::Certificate),
        ("function%s*%w+[.:]init_worker", Phase::InitWorker),
        ("function%s*%w+[.:]response", Phase::Response),
    ];

    // Simplified detection: check if function name appears in the code — 简化检测：检查函数名是否出现在代码中
    for (_pattern_key, phase) in &phase_map {
        // Simple match: check if "function" and phase name are on the same line — 简单匹配：检查 "function" 和阶段名是否在同一行
        let phase_name = match phase {
            Phase::Access => "access",
            Phase::Rewrite => "rewrite",
            Phase::HeaderFilter => "header_filter",
            Phase::BodyFilter => "body_filter",
            Phase::Log => "log",
            Phase::Certificate => "certificate",
            Phase::InitWorker => "init_worker",
            Phase::Response => "response",
        };

        for line in code.lines() {
            if line.contains("function") && line.contains(phase_name) {
                phases.insert(*phase, true);
                break;
            }
        }
    }

    phases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_priority() {
        let code = r#"
local MyPlugin = {
    PRIORITY = 1000,
    VERSION  = "2.1.0",
}
"#;
        assert_eq!(extract_priority(code), Some(1000));
    }

    #[test]
    fn test_extract_version() {
        let code = r#"
local MyPlugin = {
    PRIORITY = 1000,
    VERSION  = "2.1.0",
}
"#;
        assert_eq!(extract_version(code), Some("2.1.0".to_string()));
    }

    #[test]
    fn test_detect_phases() {
        let code = r#"
function MyPlugin:access(conf)
end

function MyPlugin:log(conf)
end
"#;
        let phases = detect_phases(code);
        assert!(phases.get(&Phase::Access).copied().unwrap_or(false));
        assert!(phases.get(&Phase::Log).copied().unwrap_or(false));
        assert!(!phases.get(&Phase::Rewrite).copied().unwrap_or(false));
    }
}
