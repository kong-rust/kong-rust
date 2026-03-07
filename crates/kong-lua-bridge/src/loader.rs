//! Lua 插件加载器 — 扫描并加载 Kong Lua 插件
//!
//! 从 Kong 的插件目录加载 Lua 插件:
//! - 读取 handler.lua 获取优先级和版本
//! - 检测插件支持的阶段
//! - 创建 LuaPluginHandler 实例

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kong_core::error::{KongError, Result};
use kong_core::traits::{Phase, PluginHandler};

use crate::LuaPluginHandler;

/// 扫描并加载指定目录下的 Kong Lua 插件
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
                        break; // 找到就不继续搜索其他目录
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

/// 加载单个 Lua 插件
fn load_single_plugin(name: &str, plugin_path: &Path) -> Result<LuaPluginHandler> {
    let handler_file = plugin_path.join("handler.lua");
    let handler_code = std::fs::read_to_string(&handler_file)
        .map_err(|e| KongError::LuaError(format!("读取 {} 失败: {}", handler_file.display(), e)))?;

    // 从 handler.lua 中提取优先级和版本
    let priority = extract_priority(&handler_code).unwrap_or(1000);
    let version = extract_version(&handler_code).unwrap_or_else(|| "0.1.0".to_string());

    // 检测支持的阶段
    let phases = detect_phases(&handler_code);

    Ok(LuaPluginHandler::new(
        name.to_string(),
        priority,
        version,
        plugin_path.to_path_buf(),
        phases,
    ))
}

/// 从 handler.lua 源码中提取插件优先级
///
/// Kong 插件通常定义为:
/// ```lua
/// local MyPlugin = {
///     PRIORITY = 1000,
///     VERSION  = "1.0.0",
/// }
/// ```
pub fn extract_priority(code: &str) -> Option<i32> {
    // 匹配 PRIORITY = 数字 或 PRIORITY=数字
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

/// 从 handler.lua 源码中提取插件版本
pub fn extract_version(code: &str) -> Option<String> {
    for line in code.lines() {
        let line = line.trim();
        if let Some(pos) = line.find("VERSION") {
            let rest = &line[pos + 7..];
            // 查找引号内的版本号
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

/// 检测 Lua 插件支持的阶段
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

    // 简化检测：检查函数名是否出现在代码中
    for (_pattern_key, phase) in &phase_map {
        // 简单匹配：检查 "function" 和阶段名是否在同一行
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
