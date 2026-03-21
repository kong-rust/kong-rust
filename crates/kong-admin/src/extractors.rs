//! Flexible body extractor for Admin API — Admin API 灵活请求体提取器
//!
//! Supports three content types — 支持三种 Content-Type:
//! - `application/json` → parse as JSON — 按 JSON 解析
//! - `application/x-www-form-urlencoded` → parse form data, convert to JSON — 解析表单数据，转为 JSON
//! - `multipart/form-data` → parse multipart fields, convert to JSON — 解析 multipart 字段，转为 JSON

use axum::extract::{FromRequest, Multipart, Request};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Flexible body extractor: accepts JSON, form-urlencoded, and multipart — 灵活请求体提取器：接受 JSON、表单编码和 multipart
pub struct FlexibleBody(pub Value);

/// Rejection type for FlexibleBody extraction failures — FlexibleBody 提取失败的拒绝类型
pub struct FlexibleBodyRejection {
    status: StatusCode,
    message: String,
}

impl IntoResponse for FlexibleBodyRejection {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "message": self.message,
            })),
        )
            .into_response()
    }
}

impl<S> FromRequest<S> for FlexibleBody
where
    S: Send + Sync,
{
    type Rejection = FlexibleBodyRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Extract Content-Type header — 提取 Content-Type 请求头
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_lowercase());

        match content_type.as_deref() {
            // JSON content type — JSON 类型
            Some(ct) if ct.contains("application/json") => {
                parse_json(req, state).await
            }
            // Form-urlencoded content type — 表单编码类型
            Some(ct) if ct.contains("application/x-www-form-urlencoded") => {
                parse_form_urlencoded(req).await
            }
            // Multipart form data — Multipart 表单数据
            Some(ct) if ct.contains("multipart/form-data") => {
                parse_multipart(req, state).await
            }
            // Explicit unsupported Content-Type → 415 — 明确的不支持 Content-Type → 415
            Some(_) => {
                Err(FlexibleBodyRejection {
                    status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    message: "Unsupported Content-Type".to_string(),
                })
            }
            // No Content-Type: if body is present, return 415; otherwise treat as empty JSON — 无 Content-Type：有请求体返回 415；否则当空 JSON 处理
            None => {
                // Check if the request has a non-empty body — 检查请求是否有非空请求体
                let content_length = req
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok());
                let has_transfer_encoding = req.headers().contains_key(header::TRANSFER_ENCODING);
                if content_length.unwrap_or(0) > 0 || has_transfer_encoding {
                    return Err(FlexibleBodyRejection {
                        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                        message: "Unsupported Content-Type".to_string(),
                    });
                }
                parse_json(req, state).await
            }
        }
    }
}

/// Parse request body as JSON — 按 JSON 解析请求体
async fn parse_json<S: Send + Sync>(
    req: Request,
    state: &S,
) -> Result<FlexibleBody, FlexibleBodyRejection> {
    // Check content length: if body is empty, return empty JSON object — 检查 content-length：空请求体返回空 JSON 对象
    let content_length = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    if content_length == Some(0) || content_length.is_none() {
        // Empty body or no Content-Length → try parsing, fall back to empty JSON object — 空请求体或无 Content-Length → 尝试解析，失败则当作空 JSON 对象
        // Read body bytes first to check if actually empty — 先读取请求体字节检查是否真的为空
        let body_bytes = Bytes::from_request(req, state)
            .await
            .unwrap_or_default();
        if body_bytes.is_empty() {
            return Ok(FlexibleBody(Value::Object(serde_json::Map::new())));
        }
        // Non-empty body without Content-Length: try parsing as JSON — 有内容但无 Content-Length：尝试按 JSON 解析
        match serde_json::from_slice::<Value>(&body_bytes) {
            Ok(value) => return Ok(FlexibleBody(value)),
            Err(_) => return Err(FlexibleBodyRejection {
                status: StatusCode::BAD_REQUEST,
                message: "Cannot parse JSON body".to_string(),
            }),
        }
    }
    match Json::<Value>::from_request(req, state).await {
        Ok(Json(value)) => Ok(FlexibleBody(value)),
        Err(_) => Err(FlexibleBodyRejection {
            status: StatusCode::BAD_REQUEST,
            message: "Cannot parse JSON body".to_string(),
        }),
    }
}

/// Parse request body as form-urlencoded, convert to JSON Value — 按表单编码解析请求体，转为 JSON Value
async fn parse_form_urlencoded(req: Request) -> Result<FlexibleBody, FlexibleBodyRejection> {
    // Read raw body bytes — 读取原始请求体
    let body_bytes = Bytes::from_request(req, &())
        .await
        .map_err(|e| FlexibleBodyRejection {
            status: StatusCode::BAD_REQUEST,
            message: format!("failed to read request body: {}", e),
        })?;

    let body_str =
        std::str::from_utf8(&body_bytes).map_err(|e| FlexibleBodyRejection {
            status: StatusCode::BAD_REQUEST,
            message: format!("invalid UTF-8 in form body: {}", e),
        })?;

    // Parse form pairs — 解析表单键值对
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(body_str.as_bytes())
        .map(|(k, v): (std::borrow::Cow<str>, std::borrow::Cow<str>)| {
            (k.into_owned(), v.into_owned())
        })
        .collect();

    let value = form_pairs_to_json(&pairs);
    Ok(FlexibleBody(value))
}

/// Parse request body as multipart form data, convert to JSON Value — 按 multipart 解析请求体，转为 JSON Value
async fn parse_multipart<S: Send + Sync>(
    req: Request,
    state: &S,
) -> Result<FlexibleBody, FlexibleBodyRejection> {
    let mut multipart = match Multipart::from_request(req, state).await {
        Ok(m) => m,
        Err(_) => {
            // Multipart 解析失败（如空请求体）— 返回空 JSON 对象以便验证逻辑执行
            return Ok(FlexibleBody(Value::Object(Map::new())));
        }
    };

    let mut pairs: Vec<(String, String)> = Vec::new();

    // Collect all fields — 收集所有字段
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let value = field.text().await.map_err(|e| FlexibleBodyRejection {
            status: StatusCode::BAD_REQUEST,
            message: format!("failed to read multipart field: {}", e),
        })?;
        pairs.push((name, value));
    }

    let value = form_pairs_to_json(&pairs);
    Ok(FlexibleBody(value))
}

/// Convert form key-value pairs to JSON Value — 将表单键值对转为 JSON Value
///
/// Supports array patterns — 支持数组模式:
/// - `key[]=val1&key[]=val2` → `{"key": ["val1", "val2"]}`
/// - `key=val1&key=val2` (duplicate keys) → `{"key": ["val1", "val2"]}`
/// - `key.1=val1&key.2=val2` (dotted index) → `{"key": ["val1", "val2"]}`
///
/// Numeric strings are auto-parsed as numbers — 数字字符串自动解析为数字
fn form_pairs_to_json(pairs: &[(String, String)]) -> Value {
    // First pass: group by normalized key — 第一遍：按归一化的 key 分组
    // Track which keys appear with array markers — 追踪带数组标记的 key
    let mut groups: HashMap<String, Vec<(Option<usize>, String)>> = HashMap::new();
    let mut insertion_order: Vec<String> = Vec::new();

    for (raw_key, value) in pairs {
        let (base_key, index) = normalize_form_key(raw_key);

        if !groups.contains_key(&base_key) {
            insertion_order.push(base_key.clone());
        }
        groups
            .entry(base_key)
            .or_default()
            .push((index, value.clone()));
    }

    // Second pass: build JSON object — 第二遍：构建 JSON 对象
    let mut map = Map::new();

    for key in &insertion_order {
        let entries = &groups[key];

        // Determine if this should be an array — 判断是否应该为数组
        let is_array = entries.len() > 1 || entries.iter().any(|(idx, _)| idx.is_some());

        if is_array {
            // Build array, respecting dotted indices if present — 构建数组，如有点号索引则按索引排列
            let has_indices = entries.iter().any(|(idx, _)| idx.is_some());
            if has_indices {
                // Sort by index — 按索引排序
                let mut indexed: Vec<(usize, &str)> = entries
                    .iter()
                    .map(|(idx, val)| (idx.unwrap_or(0), val.as_str()))
                    .collect();
                indexed.sort_by_key(|(i, _)| *i);
                let arr: Vec<Value> = indexed.iter().map(|(_, v)| smart_parse_value(v)).collect();
                map.insert(key.clone(), Value::Array(arr));
            } else {
                let arr: Vec<Value> = entries.iter().map(|(_, v)| smart_parse_value(&v)).collect();
                map.insert(key.clone(), Value::Array(arr));
            }
        } else {
            // Single value — 单值
            let (_, ref val) = entries[0];
            // Check for nested dot notation like "config.key" — 检查嵌套点号表示法如 "config.key"
            map.insert(key.clone(), smart_parse_value(val));
        }
    }

    // Post-process: handle nested dot-notation keys like "service.id" — 后处理：处理嵌套点号 key 如 "service.id"
    let result = expand_dotted_keys(Value::Object(map));
    result
}

/// Normalize form key: strip array brackets and extract dotted index — 归一化表单 key：去除数组括号并提取点号索引
///
/// Returns (base_key, optional_index) — 返回 (基础 key, 可选索引)
/// - `paths[]` → `("paths", None)` — marks as array — 标记为数组
/// - `paths.1` → `("paths", Some(1))` — dotted index — 点号索引
/// - `paths` → `("paths", None)` — plain key — 普通 key
fn normalize_form_key(raw: &str) -> (String, Option<usize>) {
    // Handle `key[]` pattern — 处理 `key[]` 模式
    if let Some(base) = raw.strip_suffix("[]") {
        return (base.to_string(), Some(0)); // use 0 as placeholder, will be ordered by insertion — 用 0 占位，按插入顺序排列
    }

    // Handle `key.N` pattern (only if N is a digit) — 处理 `key.N` 模式（仅当 N 是数字时）
    if let Some(dot_pos) = raw.rfind('.') {
        let (prefix, suffix) = raw.split_at(dot_pos);
        let suffix = &suffix[1..]; // skip the dot — 跳过点号
        if let Ok(idx) = suffix.parse::<usize>() {
            return (prefix.to_string(), Some(idx));
        }
    }

    (raw.to_string(), None)
}

/// Try to parse string as number or boolean, otherwise keep as string — 尝试将字符串解析为数字或布尔值，否则保持为字符串
fn smart_parse_value(s: &str) -> Value {
    // Try integer first — 优先尝试整数
    if let Ok(n) = s.parse::<i64>() {
        return Value::Number(n.into());
    }
    // Try float — 尝试浮点数
    if let Ok(f) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    // Try boolean — 尝试布尔值
    match s {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    // Keep as string — 保持为字符串
    Value::String(s.to_string())
}

/// Expand dotted keys into nested objects — 将点号分隔的 key 展开为嵌套对象
///
/// e.g. `{"service.id": "xxx"}` → `{"service": {"id": "xxx"}}` — 例如 `{"service.id": "xxx"}` → `{"service": {"id": "xxx"}}`
fn expand_dotted_keys(value: Value) -> Value {
    let Value::Object(map) = value else {
        return value;
    };

    let mut result = Map::new();

    for (key, val) in map {
        // Check if key contains dots that indicate nesting (not array indices, those are already handled) — 检查 key 是否包含表示嵌套的点号（非数组索引，那些已经处理过了）
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() > 1 {
            // Build nested object — 构建嵌套对象
            insert_nested(&mut result, &parts, val);
        } else {
            result.insert(key, val);
        }
    }

    Value::Object(result)
}

/// Insert a value into a nested map structure following the given path — 按给定路径将值插入嵌套的 map 结构
fn insert_nested(map: &mut Map<String, Value>, parts: &[&str], value: Value) {
    if parts.len() == 1 {
        map.insert(parts[0].to_string(), value);
        return;
    }

    let key = parts[0].to_string();
    let rest = &parts[1..];

    let entry = map
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()));

    if let Value::Object(ref mut inner) = entry {
        insert_nested(inner, rest, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_form_pairs_basic() {
        let pairs = vec![
            ("name".to_string(), "my-service".to_string()),
            ("host".to_string(), "example.com".to_string()),
            ("port".to_string(), "8080".to_string()),
        ];
        let val = form_pairs_to_json(&pairs);
        assert_eq!(val["name"], "my-service");
        assert_eq!(val["host"], "example.com");
        assert_eq!(val["port"], 8080); // auto-parsed as number — 自动解析为数字
    }

    #[test]
    fn test_form_pairs_array_brackets() {
        let pairs = vec![
            ("paths[]".to_string(), "/a".to_string()),
            ("paths[]".to_string(), "/b".to_string()),
        ];
        let val = form_pairs_to_json(&pairs);
        let paths = val["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "/a");
        assert_eq!(paths[1], "/b");
    }

    #[test]
    fn test_form_pairs_duplicate_keys() {
        let pairs = vec![
            ("methods".to_string(), "GET".to_string()),
            ("methods".to_string(), "POST".to_string()),
        ];
        let val = form_pairs_to_json(&pairs);
        let methods = val["methods"].as_array().unwrap();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0], "GET");
        assert_eq!(methods[1], "POST");
    }

    #[test]
    fn test_form_pairs_dotted_index() {
        let pairs = vec![
            ("hosts.1".to_string(), "a.com".to_string()),
            ("hosts.2".to_string(), "b.com".to_string()),
        ];
        let val = form_pairs_to_json(&pairs);
        let hosts = val["hosts"].as_array().unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0], "a.com");
        assert_eq!(hosts[1], "b.com");
    }

    #[test]
    fn test_form_pairs_nested_dot_notation() {
        let pairs = vec![
            ("service.id".to_string(), "abc-123".to_string()),
        ];
        let val = form_pairs_to_json(&pairs);
        assert_eq!(val["service"]["id"], "abc-123");
    }

    #[test]
    fn test_smart_parse_value() {
        assert_eq!(smart_parse_value("123"), json!(123));
        assert_eq!(smart_parse_value("3.14"), json!(3.14));
        assert_eq!(smart_parse_value("true"), json!(true));
        assert_eq!(smart_parse_value("false"), json!(false));
        assert_eq!(smart_parse_value("hello"), json!("hello"));
    }

    #[test]
    fn test_form_pairs_boolean_values() {
        let pairs = vec![
            ("enabled".to_string(), "true".to_string()),
            ("strip_path".to_string(), "false".to_string()),
        ];
        let val = form_pairs_to_json(&pairs);
        assert_eq!(val["enabled"], true);
        assert_eq!(val["strip_path"], false);
    }
}
