use std::collections::HashMap;
use std::path::Path;

/// 敏感配置项（日志中应隐藏其值）
const SENSITIVE_KEYS: &[&str] = &[
    "pg_password",
    "pg_ro_password",
    "proxy_server",
    "declarative_config_string",
    "cluster_cert_key",
    "ssl_cert_key",
    "admin_ssl_cert_key",
    "admin_gui_ssl_cert_key",
    "status_ssl_cert_key",
    "client_ssl_cert_key",
];

/// 解析 kong.conf 格式的配置文件
/// 格式: key = value（# 开头为注释，空行忽略）
pub fn parse_conf_file(content: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();

    for line in content.lines() {
        let line = line.trim();

        // 跳过空行和注释
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // 解析 key = value
        if let Some((key, value)) = parse_key_value(line) {
            result.insert(key, value);
        }
    }

    result
}

/// 解析单行 key = value（支持 key=value 和 key = value 两种格式）
fn parse_key_value(line: &str) -> Option<(String, String)> {
    // 找到第一个 = 号
    let eq_pos = line.find('=')?;
    let key = line[..eq_pos].trim().to_string();
    let mut value = line[eq_pos + 1..].trim().to_string();

    // 去除行内注释（# 后面的内容，但需要注意引号内的 # 不是注释）
    if let Some(comment_pos) = find_inline_comment(&value) {
        value = value[..comment_pos].trim().to_string();
    }

    // 去除引号
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value = value[1..value.len() - 1].to_string();
    }

    if key.is_empty() {
        return None;
    }

    Some((key, value))
}

/// 查找行内注释的位置（忽略引号内的 #）
fn find_inline_comment(value: &str) -> Option<usize> {
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for (i, ch) in value.char_indices() {
        match ch {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '#' if !in_single_quote && !in_double_quote => {
                // 确保 # 前面有空格（避免误判 URL 中的 #）
                if i > 0 && value.as_bytes()[i - 1] == b' ' {
                    return Some(i);
                }
            }
            _ => {}
        }
    }

    None
}

/// 从文件路径加载并解析配置
pub fn load_conf_file(path: &Path) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_conf_file(&content))
}

/// 从环境变量收集 KONG_* 配置覆盖
/// 规则: KONG_<UPPERCASE_KEY> -> lowercase_key
/// 例如: KONG_PG_HOST -> pg_host, KONG_LOG_LEVEL -> log_level
pub fn collect_env_overrides() -> HashMap<String, String> {
    let mut result = HashMap::new();

    for (key, value) in std::env::vars() {
        if let Some(kong_key) = key.strip_prefix("KONG_") {
            let config_key = kong_key.to_lowercase();
            result.insert(config_key, value);
        }
    }

    result
}

/// 判断配置项是否为敏感信息
pub fn is_sensitive(key: &str) -> bool {
    SENSITIVE_KEYS.contains(&key)
}

/// 获取用于日志的配置值（敏感信息替换为 ******）
pub fn display_value(key: &str, value: &str) -> String {
    if is_sensitive(key) {
        "******".to_string()
    } else {
        value.to_string()
    }
}

/// 搜索默认配置文件路径
/// Kong 的搜索顺序: /etc/kong/kong.conf -> /etc/kong.conf
pub fn find_default_conf() -> Option<std::path::PathBuf> {
    let paths = [
        Path::new("/etc/kong/kong.conf"),
        Path::new("/etc/kong.conf"),
    ];

    for path in &paths {
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let conf = "pg_host = 127.0.0.1\npg_port = 5432\n";
        let result = parse_conf_file(conf);
        assert_eq!(result.get("pg_host").unwrap(), "127.0.0.1");
        assert_eq!(result.get("pg_port").unwrap(), "5432");
    }

    #[test]
    fn test_parse_with_comments() {
        let conf = r#"
# 这是注释
pg_host = 127.0.0.1
# 另一条注释
pg_port = 5432

database = postgres
"#;
        let result = parse_conf_file(conf);
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("database").unwrap(), "postgres");
    }

    #[test]
    fn test_parse_inline_comment() {
        let conf = "pg_host = 127.0.0.1 # 数据库地址\n";
        let result = parse_conf_file(conf);
        assert_eq!(result.get("pg_host").unwrap(), "127.0.0.1");
    }

    #[test]
    fn test_parse_quoted_value() {
        let conf = "prefix = \"/usr/local/kong/\"\n";
        let result = parse_conf_file(conf);
        assert_eq!(result.get("prefix").unwrap(), "/usr/local/kong/");
    }

    #[test]
    fn test_parse_no_spaces() {
        let conf = "pg_host=localhost\n";
        let result = parse_conf_file(conf);
        assert_eq!(result.get("pg_host").unwrap(), "localhost");
    }

    #[test]
    fn test_parse_listen_multivalue() {
        let conf =
            "proxy_listen = 0.0.0.0:8000 reuseport backlog=16384, 0.0.0.0:8443 http2 ssl reuseport backlog=16384\n";
        let result = parse_conf_file(conf);
        let val = result.get("proxy_listen").unwrap();
        assert!(val.contains("0.0.0.0:8000"));
        assert!(val.contains("0.0.0.0:8443"));
    }

    #[test]
    fn test_sensitive_keys() {
        assert!(is_sensitive("pg_password"));
        assert!(is_sensitive("cluster_cert_key"));
        assert!(!is_sensitive("pg_host"));
    }

    #[test]
    fn test_display_value() {
        assert_eq!(display_value("pg_password", "secret"), "******");
        assert_eq!(display_value("pg_host", "localhost"), "localhost");
    }
}
