//! 表达式路由引擎 — 支持 Kong ATC 表达式语法
//!
//! 支持的运算符: ==, !=, ~, in, not in, &&, ||
//! 支持的字段: http.method, http.host, http.path, http.headers.*, net.protocol, tls.sni
//!
//! 示例表达式:
//! - `http.method == "GET" && http.path ~ "^/api"`
//! - `http.host in "example.com", "api.example.com"`

use regex::Regex;
use uuid::Uuid;

use crate::{RequestContext, RouteMatch};
use kong_core::models::Route;

/// 表达式路由器
pub struct ExpressionsRouter {
    /// 已编译的表达式路由（按 priority 降序排列）
    routes: Vec<ExpressionRoute>,
}

/// 编译后的表达式路由
struct ExpressionRoute {
    route_id: Uuid,
    service_id: Option<Uuid>,
    route_name: Option<String>,
    priority: i64,
    /// 编译后的表达式
    expression: CompiledExpression,
    strip_path: bool,
    preserve_host: bool,
    path_handling: String,
    protocols: Vec<String>,
}

/// 编译后的表达式节点
enum CompiledExpression {
    /// 与操作
    And(Box<CompiledExpression>, Box<CompiledExpression>),
    /// 或操作
    Or(Box<CompiledExpression>, Box<CompiledExpression>),
    /// 相等比较
    Eq(Field, String),
    /// 不等比较
    Ne(Field, String),
    /// 正则匹配
    Regex(Field, Regex),
    /// 包含检查
    In(Field, Vec<String>),
    /// 不包含检查
    NotIn(Field, Vec<String>),
    /// 始终为真（空表达式）
    True,
}

/// 可匹配的字段
#[derive(Debug, Clone)]
enum Field {
    HttpMethod,
    HttpHost,
    HttpPath,
    HttpHeader(String),
    NetProtocol,
    TlsSni,
}

impl ExpressionsRouter {
    /// 从路由列表构建表达式路由器
    pub fn new(routes: &[Route]) -> Self {
        let mut expr_routes: Vec<ExpressionRoute> = routes
            .iter()
            .filter_map(|route| {
                let expression_str = route.expression.as_deref()?;
                let expression = parse_expression(expression_str).ok()?;

                let service_id = route.service.as_ref().map(|fk| fk.id);
                let protocols: Vec<String> = route.protocols.iter().map(|p| p.to_string()).collect();

                Some(ExpressionRoute {
                    route_id: route.id,
                    service_id,
                    route_name: route.name.clone(),
                    priority: route.priority.unwrap_or(0) as i64,
                    expression,
                    strip_path: route.strip_path,
                    preserve_host: route.preserve_host,
                    path_handling: match &route.path_handling {
                        kong_core::models::PathHandling::V0 => "v0".to_string(),
                        kong_core::models::PathHandling::V1 => "v1".to_string(),
                    },
                    protocols,
                })
            })
            .collect();

        // 按 priority 降序排列
        expr_routes.sort_by(|a, b| b.priority.cmp(&a.priority));

        tracing::info!(
            "表达式路由器初始化完成: {} 条路由",
            expr_routes.len()
        );

        Self {
            routes: expr_routes,
        }
    }

    /// 匹配请求
    pub fn find_route(&self, ctx: &RequestContext) -> Option<RouteMatch> {
        for route in &self.routes {
            if evaluate(&route.expression, ctx) {
                return Some(RouteMatch {
                    route_id: route.route_id,
                    service_id: route.service_id,
                    route_name: route.route_name.clone(),
                    strip_path: route.strip_path,
                    preserve_host: route.preserve_host,
                    path_handling: route.path_handling.clone(),
                    matched_path: None,
                    protocols: route.protocols.clone(),
                });
            }
        }
        None
    }

    /// 路由数量
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

/// 计算表达式
fn evaluate(expr: &CompiledExpression, ctx: &RequestContext) -> bool {
    match expr {
        CompiledExpression::True => true,
        CompiledExpression::And(a, b) => evaluate(a, ctx) && evaluate(b, ctx),
        CompiledExpression::Or(a, b) => evaluate(a, ctx) || evaluate(b, ctx),
        CompiledExpression::Eq(field, value) => {
            let field_val = get_field_value(field, ctx);
            field_val.as_deref() == Some(value.as_str())
        }
        CompiledExpression::Ne(field, value) => {
            let field_val = get_field_value(field, ctx);
            field_val.as_deref() != Some(value.as_str())
        }
        CompiledExpression::Regex(field, regex) => {
            let field_val = get_field_value(field, ctx);
            field_val.map_or(false, |v| regex.is_match(&v))
        }
        CompiledExpression::In(field, values) => {
            let field_val = get_field_value(field, ctx);
            field_val.map_or(false, |v| values.contains(&v))
        }
        CompiledExpression::NotIn(field, values) => {
            let field_val = get_field_value(field, ctx);
            field_val.map_or(true, |v| !values.contains(&v))
        }
    }
}

/// 获取字段值
fn get_field_value(field: &Field, ctx: &RequestContext) -> Option<String> {
    match field {
        Field::HttpMethod => Some(ctx.method.clone()),
        Field::HttpHost => Some(ctx.host.clone()),
        Field::HttpPath => Some(ctx.uri.clone()),
        Field::HttpHeader(name) => ctx.headers.get(name).cloned(),
        Field::NetProtocol => Some(ctx.scheme.clone()),
        Field::TlsSni => ctx.sni.clone(),
    }
}

/// 解析字段名
fn parse_field(name: &str) -> Result<Field, String> {
    let name = name.trim();
    match name {
        "http.method" => Ok(Field::HttpMethod),
        "http.host" => Ok(Field::HttpHost),
        "http.path" => Ok(Field::HttpPath),
        "net.protocol" => Ok(Field::NetProtocol),
        "tls.sni" => Ok(Field::TlsSni),
        _ if name.starts_with("http.headers.") => {
            let header_name = name.strip_prefix("http.headers.").unwrap();
            Ok(Field::HttpHeader(header_name.to_lowercase()))
        }
        _ => Err(format!("未知的字段: {}", name)),
    }
}

/// 解析表达式字符串
///
/// 简化的递归下降解析器，支持:
/// - `field == "value"`
/// - `field != "value"`
/// - `field ~ "regex"`
/// - `field in "val1", "val2"`
/// - `expr && expr`
/// - `expr || expr`
/// - `(expr)`
fn parse_expression(input: &str) -> Result<CompiledExpression, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(CompiledExpression::True);
    }

    // 处理 || （最低优先级）
    if let Some(pos) = find_operator(input, "||") {
        let left = parse_expression(&input[..pos])?;
        let right = parse_expression(&input[pos + 2..])?;
        return Ok(CompiledExpression::Or(Box::new(left), Box::new(right)));
    }

    // 处理 &&
    if let Some(pos) = find_operator(input, "&&") {
        let left = parse_expression(&input[..pos])?;
        let right = parse_expression(&input[pos + 2..])?;
        return Ok(CompiledExpression::And(Box::new(left), Box::new(right)));
    }

    // 处理括号
    if input.starts_with('(') && input.ends_with(')') {
        return parse_expression(&input[1..input.len() - 1]);
    }

    // 解析简单比较: field op value
    parse_comparison(input)
}

/// 查找顶层运算符位置（跳过括号和引号内的内容）
fn find_operator(input: &str, op: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_quote = false;
    let bytes = input.as_bytes();

    for i in 0..bytes.len() {
        match bytes[i] {
            b'"' if depth == 0 => in_quote = !in_quote,
            b'(' if !in_quote => depth += 1,
            b')' if !in_quote => depth -= 1,
            _ if !in_quote && depth == 0 => {
                if input[i..].starts_with(op) {
                    // 确保前后有空格
                    let before_ok = i == 0 || bytes[i - 1] == b' ';
                    let after_ok = i + op.len() >= bytes.len()
                        || bytes[i + op.len()] == b' ';
                    if before_ok && after_ok {
                        return Some(i);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// 解析简单比较表达式
fn parse_comparison(input: &str) -> Result<CompiledExpression, String> {
    let input = input.trim();

    // 尝试匹配各种运算符
    for (op, op_len) in &[("==", 2), ("!=", 2), ("~", 1)] {
        if let Some(pos) = input.find(op) {
            // 确保不是其他运算符的一部分
            if *op == "~" && (input[..pos].ends_with('!') || input[pos + 1..].starts_with('=')) {
                continue;
            }

            let field_str = input[..pos].trim();
            let value_str = input[pos + op_len..].trim();
            let field = parse_field(field_str)?;
            let value = strip_quotes(value_str);

            return match *op {
                "==" => Ok(CompiledExpression::Eq(field, value)),
                "!=" => Ok(CompiledExpression::Ne(field, value)),
                "~" => {
                    let regex = Regex::new(&value)
                        .map_err(|e| format!("无效的正则表达式: {}", e))?;
                    Ok(CompiledExpression::Regex(field, regex))
                }
                _ => unreachable!(),
            };
        }
    }

    // 尝试 "in" 运算符
    if let Some(pos) = input.find(" in ") {
        let field_str = input[..pos].trim();
        let values_str = input[pos + 4..].trim();
        let field = parse_field(field_str)?;
        let values: Vec<String> = values_str
            .split(',')
            .map(|s| strip_quotes(s.trim()))
            .collect();
        return Ok(CompiledExpression::In(field, values));
    }

    // 尝试 "not in" 运算符
    if let Some(pos) = input.find(" not in ") {
        let field_str = input[..pos].trim();
        let values_str = input[pos + 8..].trim();
        let field = parse_field(field_str)?;
        let values: Vec<String> = values_str
            .split(',')
            .map(|s| strip_quotes(s.trim()))
            .collect();
        return Ok(CompiledExpression::NotIn(field, values));
    }

    Err(format!("无法解析表达式: {}", input))
}

/// 去除字符串两端的引号
fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"'))
        || (s.starts_with('\'') && s.ends_with('\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(method: &str, host: &str, path: &str) -> RequestContext {
        RequestContext {
            method: method.to_string(),
            uri: path.to_string(),
            host: host.to_string(),
            scheme: "http".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_parse_eq() {
        let expr = parse_expression("http.method == \"GET\"").unwrap();
        let ctx = make_ctx("GET", "localhost", "/");
        assert!(evaluate(&expr, &ctx));

        let ctx2 = make_ctx("POST", "localhost", "/");
        assert!(!evaluate(&expr, &ctx2));
    }

    #[test]
    fn test_parse_regex() {
        let expr = parse_expression(r#"http.path ~ "^/api/v\d+""#).unwrap();
        let ctx = make_ctx("GET", "localhost", "/api/v1");
        assert!(evaluate(&expr, &ctx));
    }

    #[test]
    fn test_parse_and() {
        let expr =
            parse_expression("http.method == \"GET\" && http.host == \"example.com\"").unwrap();

        let ctx1 = make_ctx("GET", "example.com", "/");
        assert!(evaluate(&expr, &ctx1));

        let ctx2 = make_ctx("POST", "example.com", "/");
        assert!(!evaluate(&expr, &ctx2));
    }

    #[test]
    fn test_parse_or() {
        let expr =
            parse_expression("http.method == \"GET\" || http.method == \"POST\"").unwrap();

        assert!(evaluate(&expr, &make_ctx("GET", "localhost", "/")));
        assert!(evaluate(&expr, &make_ctx("POST", "localhost", "/")));
        assert!(!evaluate(&expr, &make_ctx("PUT", "localhost", "/")));
    }

    #[test]
    fn test_parse_in() {
        let expr =
            parse_expression("http.method in \"GET\", \"POST\", \"PUT\"").unwrap();

        assert!(evaluate(&expr, &make_ctx("GET", "localhost", "/")));
        assert!(evaluate(&expr, &make_ctx("PUT", "localhost", "/")));
        assert!(!evaluate(&expr, &make_ctx("DELETE", "localhost", "/")));
    }
}
