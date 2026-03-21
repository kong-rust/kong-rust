//! Schema endpoints for entities and plugins. — 实体和插件的 schema 端点。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::AdminState;

/// GET /schemas/{entity_name} — Return a minimal entity schema (Kong-compatible). — GET /schemas/{entity_name} — 返回最小化的实体 schema（Kong 兼容）。
pub async fn get_entity_schema(
    Path(entity_name): Path<String>,
) -> impl IntoResponse {
    // Return a minimal but valid schema object for known entity types — 对已知实体类型返回最小但有效的 schema 对象
    let schema = match entity_name.as_str() {
        "services" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "unique": true}},
                {"host": {"type": "string", "required": true}},
                {"port": {"type": "integer", "default": 80, "between": [0, 65535]}},
                {"protocol": {"type": "string", "default": "http", "one_of": ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp"]}},
                {"path": {"type": "string"}},
                {"retries": {"type": "integer", "default": 5}},
                {"connect_timeout": {"type": "integer", "default": 60000}},
                {"read_timeout": {"type": "integer", "default": 60000}},
                {"write_timeout": {"type": "integer", "default": 60000}},
                {"enabled": {"type": "boolean", "default": true}},
                {"ca_certificates": {"type": "array", "elements": {"type": "string", "uuid": true}}},
                {"client_certificate": {"type": "foreign", "reference": "certificates"}},
                {"tls_verify": {"type": "boolean"}},
                {"tls_verify_depth": {"type": "integer"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "routes" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "unique": true}},
                {"protocols": {"type": "set", "elements": {"type": "string", "one_of": ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp", "ws", "wss"]}, "default": ["http", "https"]}},
                {"methods": {"type": "set", "elements": {"type": "string"}}},
                {"hosts": {"type": "array", "elements": {"type": "string"}}},
                {"paths": {"type": "array", "elements": {"type": "string"}}},
                {"headers": {"type": "map", "keys": {"type": "string"}, "values": {"type": "array", "elements": {"type": "string"}}}},
                {"https_redirect_status_code": {"type": "integer", "default": 426, "one_of": [426, 301, 302, 307, 308]}},
                {"regex_priority": {"type": "integer", "default": 0}},
                {"strip_path": {"type": "boolean", "default": true}},
                {"path_handling": {"type": "string", "default": "v0", "one_of": ["v0", "v1"]}},
                {"preserve_host": {"type": "boolean", "default": false}},
                {"request_buffering": {"type": "boolean", "default": true}},
                {"response_buffering": {"type": "boolean", "default": true}},
                {"snis": {"type": "set", "elements": {"type": "string"}}},
                {"sources": {"type": "set", "elements": {"type": "record"}}},
                {"destinations": {"type": "set", "elements": {"type": "record"}}},
                {"service": {"type": "foreign", "reference": "services"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [
                {"at_least_one_of": ["methods", "hosts", "paths", "headers", "snis", "sources", "destinations"]},
            ],
        }),
        "consumers" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"username": {"type": "string", "unique": true}},
                {"custom_id": {"type": "string", "unique": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [
                {"at_least_one_of": ["custom_id", "username"]},
            ],
        }),
        "plugins" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true}},
                {"config": {"type": "record"}},
                {"enabled": {"type": "boolean", "default": true}},
                {"instance_name": {"type": "string"}},
                {"service": {"type": "foreign", "reference": "services"}},
                {"route": {"type": "foreign", "reference": "routes"}},
                {"consumer": {"type": "foreign", "reference": "consumers"}},
                {"protocols": {"type": "set", "elements": {"type": "string", "one_of": ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp", "ws", "wss"]}, "default": ["grpc", "grpcs", "http", "https"]}},
                {"ordering": {"type": "record"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "upstreams" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true, "unique": true}},
                {"algorithm": {"type": "string", "default": "round-robin", "one_of": ["consistent-hashing", "least-connections", "round-robin", "latency"]}},
                {"hash_on": {"type": "string", "default": "none", "one_of": ["none", "consumer", "ip", "header", "cookie", "path", "query_arg", "uri_capture"]}},
                {"hash_fallback": {"type": "string", "default": "none"}},
                {"hash_on_header": {"type": "string"}},
                {"hash_fallback_header": {"type": "string"}},
                {"hash_on_cookie": {"type": "string"}},
                {"hash_on_cookie_path": {"type": "string", "default": "/"}},
                {"hash_on_query_arg": {"type": "string"}},
                {"hash_fallback_query_arg": {"type": "string"}},
                {"hash_on_uri_capture": {"type": "string"}},
                {"hash_fallback_uri_capture": {"type": "string"}},
                {"slots": {"type": "integer", "default": 10000, "between": [10, 65536]}},
                {"healthchecks": {"type": "record"}},
                {"host_header": {"type": "string"}},
                {"client_certificate": {"type": "foreign", "reference": "certificates"}},
                {"use_srv_name": {"type": "boolean", "default": false}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "certificates" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"cert": {"type": "string", "required": true}},
                {"key": {"type": "string", "required": true}},
                {"cert_alt": {"type": "string"}},
                {"key_alt": {"type": "string"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "snis" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true, "unique": true}},
                {"certificate": {"type": "foreign", "reference": "certificates", "required": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "ca_certificates" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"cert": {"type": "string", "required": true}},
                {"cert_digest": {"type": "string"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "targets" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"target": {"type": "string", "required": true}},
                {"weight": {"type": "integer", "default": 100, "between": [0, 65535]}},
                {"upstream": {"type": "foreign", "reference": "upstreams", "required": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        "vaults" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true}},
                {"prefix": {"type": "string", "required": true, "unique": true}},
                {"description": {"type": "string"}},
                {"config": {"type": "record", "required": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ],
            "entity_checks": [],
        }),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("No entity named '{}'", entity_name),
                })),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(schema)).into_response()
}

/// All known Kong bundled plugin names — 所有已知的 Kong 内置插件名
const BUNDLED_PLUGINS: &[&str] = &[
    "key-auth", "basic-auth", "rate-limiting", "cors",
    "tcp-log", "file-log", "http-log", "udp-log",
    "ip-restriction", "request-transformer", "response-transformer",
    "pre-function", "post-function",
    "acl", "bot-detection", "correlation-id", "jwt", "hmac-auth",
    "oauth2", "ldap-auth", "session",
    "request-size-limiting", "request-termination", "response-ratelimiting",
    "syslog", "loggly", "datadog", "statsd", "prometheus",
    "zipkin", "opentelemetry", "grpc-gateway", "grpc-web",
    "aws-lambda", "azure-functions", "proxy-cache", "request-debug",
    // Test/dev plugins — 测试/开发插件
    "rewriter", "dummy", "error-generator-last", "short-circuit",
    "ctx-checker", "ctx-checker-last", "enable-buffering", "mocking",
];

/// Return a detailed plugin schema for known plugins, or minimal stub for others — 返回已知插件的详细 schema，或其他插件的最小占位
fn minimal_plugin_schema(name: &str) -> serde_json::Value {
    let config_fields = get_plugin_config_schema(name);
    json!({
        "fields": [
            {"consumer": {"type": "foreign", "reference": "consumers", "eq": null}},
            {"protocols": {"type": "set", "elements": {"type": "string", "one_of": ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp", "ws", "wss"]}, "default": ["grpc", "grpcs", "http", "https"]}},
            {"config": {"type": "record", "required": true, "fields": config_fields}},
        ],
        "entity_checks": [],
        "name": name,
    })
}

/// Return detailed config schema fields for known plugins — 返回已知插件的详细 config schema 字段
fn get_plugin_config_schema(name: &str) -> serde_json::Value {
    match name {
        "key-auth" => json!([
            {"key_names": {"type": "array", "elements": {"type": "string"}, "default": ["apikey"]}},
            {"key_in_body": {"type": "boolean", "default": false}},
            {"key_in_header": {"type": "boolean", "default": true}},
            {"key_in_query": {"type": "boolean", "default": true}},
            {"hide_credentials": {"type": "boolean", "default": false}},
            {"anonymous": {"type": "string"}},
            {"run_on_preflight": {"type": "boolean", "default": true}},
        ]),
        "basic-auth" => json!([
            {"hide_credentials": {"type": "boolean", "default": false}},
            {"anonymous": {"type": "string"}},
        ]),
        "rate-limiting" => json!([
            {"second": {"type": "number"}},
            {"minute": {"type": "number"}},
            {"hour": {"type": "number"}},
            {"day": {"type": "number"}},
            {"month": {"type": "number"}},
            {"year": {"type": "number"}},
            {"limit_by": {"type": "string", "default": "consumer", "one_of": ["consumer", "credential", "ip", "service", "header", "path"]}},
            {"policy": {"type": "string", "default": "local", "one_of": ["local", "cluster", "redis"]}},
            {"fault_tolerant": {"type": "boolean", "default": true}},
            {"hide_client_headers": {"type": "boolean", "default": false}},
            {"redis_host": {"type": "string"}},
            {"redis_port": {"type": "integer", "default": 6379}},
            {"redis_password": {"type": "string"}},
            {"redis_timeout": {"type": "number", "default": 2000}},
            {"redis_database": {"type": "integer", "default": 0}},
            {"header_name": {"type": "string"}},
            {"path": {"type": "string"}},
            {"redis_ssl": {"type": "boolean", "default": false}},
            {"redis_ssl_verify": {"type": "boolean", "default": false}},
            {"redis_server_name": {"type": "string"}},
            {"error_code": {"type": "number", "default": 429}},
            {"error_message": {"type": "string", "default": "API rate limit exceeded"}},
            {"sync_rate": {"type": "number", "default": -1}},
        ]),
        "cors" => json!([
            {"origins": {"type": "array", "elements": {"type": "string"}}},
            {"methods": {"type": "array", "elements": {"type": "string"}, "default": ["GET", "HEAD", "PUT", "PATCH", "POST", "DELETE", "OPTIONS", "TRACE", "CONNECT"]}},
            {"headers": {"type": "array", "elements": {"type": "string"}}},
            {"exposed_headers": {"type": "array", "elements": {"type": "string"}}},
            {"credentials": {"type": "boolean", "default": false}},
            {"max_age": {"type": "number"}},
            {"preflight_continue": {"type": "boolean", "default": false}},
            {"private_network": {"type": "boolean", "default": false}},
        ]),
        "request-transformer" => json!([
            {"http_method": {"type": "string"}},
            {"remove": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"querystring": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"body": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"rename": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"querystring": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"body": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"replace": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"querystring": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"body": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"uri": {"type": "string"}},
            ]}},
            {"add": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"querystring": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"body": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"append": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"querystring": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"body": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
        ]),
        "response-transformer" => json!([
            {"remove": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"rename": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"replace": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json_types": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"add": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json_types": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
            {"append": {"type": "record", "fields": [
                {"headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json": {"type": "array", "elements": {"type": "string"}, "default": []}},
                {"json_types": {"type": "array", "elements": {"type": "string"}, "default": []}},
            ]}},
        ]),
        "tcp-log" => json!([
            {"host": {"type": "string", "required": true}},
            {"port": {"type": "integer", "required": true, "between": [0, 65535]}},
            {"timeout": {"type": "number", "default": 10000}},
            {"keepalive": {"type": "number", "default": 60000}},
            {"tls": {"type": "boolean", "default": false}},
            {"tls_sni": {"type": "string"}},
        ]),
        "udp-log" => json!([
            {"host": {"type": "string", "required": true}},
            {"port": {"type": "integer", "required": true, "between": [0, 65535]}},
            {"timeout": {"type": "number", "default": 10000}},
        ]),
        "http-log" => json!([
            {"http_endpoint": {"type": "string", "required": true}},
            {"method": {"type": "string", "default": "POST", "one_of": ["POST", "PUT", "PATCH"]}},
            {"content_type": {"type": "string", "default": "application/json", "one_of": ["application/json"]}},
            {"timeout": {"type": "number", "default": 10000}},
            {"keepalive": {"type": "number", "default": 60000}},
            {"flush_timeout": {"type": "number", "default": 2}},
            {"retry_count": {"type": "integer", "default": 10}},
            {"queue_size": {"type": "integer", "default": 1}},
        ]),
        "file-log" => json!([
            {"path": {"type": "string", "required": true}},
            {"reopen": {"type": "boolean", "default": false}},
        ]),
        "ip-restriction" => json!([
            {"allow": {"type": "array", "elements": {"type": "string"}}},
            {"deny": {"type": "array", "elements": {"type": "string"}}},
            {"status": {"type": "number"}},
            {"message": {"type": "string"}},
        ]),
        "acl" => json!([
            {"allow": {"type": "array", "elements": {"type": "string"}}},
            {"deny": {"type": "array", "elements": {"type": "string"}}},
            {"hide_groups_header": {"type": "boolean", "default": false}},
        ]),
        "hmac-auth" => json!([
            {"hide_credentials": {"type": "boolean", "default": false}},
            {"clock_skew": {"type": "number", "default": 300}},
            {"algorithms": {"type": "array", "elements": {"type": "string"}, "default": ["hmac-sha1", "hmac-sha256", "hmac-sha384", "hmac-sha512"]}},
            {"enforce_headers": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"validate_request_body": {"type": "boolean", "default": false}},
        ]),
        "jwt" => json!([
            {"uri_param_names": {"type": "array", "elements": {"type": "string"}, "default": ["jwt"]}},
            {"cookie_names": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"header_names": {"type": "array", "elements": {"type": "string"}, "default": ["authorization"]}},
            {"key_claim_name": {"type": "string", "default": "iss"}},
            {"secret_is_base64": {"type": "boolean", "default": false}},
            {"claims_to_verify": {"type": "set", "elements": {"type": "string"}}},
            {"anonymous": {"type": "string"}},
            {"run_on_preflight": {"type": "boolean", "default": true}},
            {"maximum_expiration": {"type": "number", "default": 0}},
        ]),
        "request-size-limiting" => json!([
            {"allowed_payload_size": {"type": "integer", "default": 128}},
            {"size_unit": {"type": "string", "default": "megabytes", "one_of": ["megabytes", "kilobytes", "bytes"]}},
            {"require_content_length": {"type": "boolean", "default": false}},
        ]),
        "request-termination" => json!([
            {"status_code": {"type": "integer", "default": 503}},
            {"message": {"type": "string"}},
            {"body": {"type": "string"}},
            {"content_type": {"type": "string"}},
            {"trigger": {"type": "string"}},
            {"echo": {"type": "boolean", "default": false}},
        ]),
        "bot-detection" => json!([
            {"allow": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"deny": {"type": "array", "elements": {"type": "string"}, "default": []}},
        ]),
        "correlation-id" => json!([
            {"header_name": {"type": "string", "default": "Kong-Request-ID"}},
            {"generator": {"type": "string", "default": "uuid#counter"}},
            {"echo_downstream": {"type": "boolean", "default": false}},
        ]),
        "prometheus" => json!([
            {"per_consumer": {"type": "boolean", "default": false}},
            {"status_code_metrics": {"type": "boolean", "default": false}},
            {"latency_metrics": {"type": "boolean", "default": false}},
            {"bandwidth_metrics": {"type": "boolean", "default": false}},
            {"upstream_health_metrics": {"type": "boolean", "default": false}},
        ]),
        "oauth2" => json!([
            {"scopes": {"type": "array", "elements": {"type": "string"}}},
            {"mandatory_scope": {"type": "boolean", "default": false}},
            {"provision_key": {"type": "string", "unique": true}},
            {"token_expiration": {"type": "number", "default": 7200}},
            {"enable_authorization_code": {"type": "boolean", "default": false}},
            {"enable_client_credentials": {"type": "boolean", "default": false}},
            {"enable_implicit_grant": {"type": "boolean", "default": false}},
            {"enable_password_grant": {"type": "boolean", "default": false}},
            {"hide_credentials": {"type": "boolean", "default": false}},
            {"accept_http_if_already_terminated": {"type": "boolean", "default": false}},
            {"anonymous": {"type": "string"}},
            {"global_credentials": {"type": "boolean", "default": false}},
            {"auth_header_name": {"type": "string", "default": "authorization"}},
            {"refresh_token_ttl": {"type": "number", "default": 1209600}},
            {"reuse_refresh_token": {"type": "boolean", "default": false}},
            {"persistent_refresh_token": {"type": "boolean", "default": false}},
        ]),
        "ldap-auth" => json!([
            {"ldap_host": {"type": "string", "required": true}},
            {"ldap_port": {"type": "integer", "required": true, "default": 389}},
            {"start_tls": {"type": "boolean", "default": false}},
            {"verify_ldap_host": {"type": "boolean", "default": false}},
            {"base_dn": {"type": "string", "required": true}},
            {"attribute": {"type": "string", "required": true}},
            {"cache_ttl": {"type": "number", "default": 60}},
            {"timeout": {"type": "number", "default": 10000}},
            {"keepalive": {"type": "number", "default": 60000}},
            {"anonymous": {"type": "string"}},
            {"header_type": {"type": "string", "default": "ldap"}},
            {"hide_credentials": {"type": "boolean", "default": false}},
        ]),
        "session" => json!([
            {"secret": {"type": "string"}},
            {"cookie_name": {"type": "string", "default": "session"}},
            {"cookie_lifetime": {"type": "number", "default": 3600}},
            {"cookie_path": {"type": "string", "default": "/"}},
            {"cookie_domain": {"type": "string"}},
            {"cookie_samesite": {"type": "string", "default": "Strict"}},
            {"cookie_httponly": {"type": "boolean", "default": true}},
            {"cookie_secure": {"type": "boolean", "default": true}},
            {"storage": {"type": "string", "default": "cookie", "one_of": ["cookie", "kong"]}},
        ]),
        "response-ratelimiting" => json!([
            {"header_name": {"type": "string", "default": "x-kong-limit"}},
            {"limit_by": {"type": "string", "default": "consumer"}},
            {"policy": {"type": "string", "default": "local"}},
            {"fault_tolerant": {"type": "boolean", "default": true}},
            {"hide_client_headers": {"type": "boolean", "default": false}},
            {"redis_host": {"type": "string"}},
            {"redis_port": {"type": "integer", "default": 6379}},
            {"redis_password": {"type": "string"}},
            {"redis_timeout": {"type": "number", "default": 2000}},
            {"redis_database": {"type": "integer", "default": 0}},
            {"block_on_first_violation": {"type": "boolean", "default": false}},
            {"limits": {"type": "map", "required": true, "keys": {"type": "string"}, "values": {"type": "record"}}},
        ]),
        "syslog" => json!([
            {"successful_severity": {"type": "string", "default": "info"}},
            {"client_errors_severity": {"type": "string", "default": "info"}},
            {"server_errors_severity": {"type": "string", "default": "info"}},
            {"log_level": {"type": "string", "default": "info"}},
        ]),
        "loggly" => json!([
            {"host": {"type": "string", "default": "logs-01.loggly.com"}},
            {"port": {"type": "integer", "default": 514}},
            {"key": {"type": "string", "required": true}},
            {"tags": {"type": "set", "elements": {"type": "string"}, "default": ["kong"]}},
            {"timeout": {"type": "number", "default": 10000}},
            {"successful_severity": {"type": "string", "default": "info"}},
            {"client_errors_severity": {"type": "string", "default": "info"}},
            {"server_errors_severity": {"type": "string", "default": "info"}},
            {"log_level": {"type": "string", "default": "info"}},
        ]),
        "datadog" => json!([
            {"host": {"type": "string", "default": "localhost"}},
            {"port": {"type": "integer", "default": 8125}},
            {"prefix": {"type": "string", "default": "kong"}},
            {"metrics": {"type": "array", "elements": {"type": "record"}}},
        ]),
        "statsd" => json!([
            {"host": {"type": "string", "default": "localhost"}},
            {"port": {"type": "integer", "default": 8125}},
            {"prefix": {"type": "string", "default": "kong"}},
            {"metrics": {"type": "array", "elements": {"type": "record"}}},
        ]),
        "zipkin" => json!([
            {"http_endpoint": {"type": "string"}},
            {"sample_ratio": {"type": "number", "default": 0.001}},
            {"default_service_name": {"type": "string"}},
            {"include_credential": {"type": "boolean", "default": true}},
            {"traceid_byte_count": {"type": "integer", "default": 16}},
            {"header_type": {"type": "string", "default": "preserve"}},
            {"default_header_type": {"type": "string", "default": "b3"}},
            {"tags_header": {"type": "string", "default": "Zipkin-Tags"}},
            {"static_tags": {"type": "array", "elements": {"type": "record"}}},
        ]),
        "grpc-gateway" => json!([
            {"proto": {"type": "string"}},
        ]),
        "grpc-web" => json!([
            {"proto": {"type": "string"}},
            {"pass_stripped_path": {"type": "boolean"}},
            {"allow_origin_header": {"type": "string", "default": "*"}},
        ]),
        "aws-lambda" => json!([
            {"aws_key": {"type": "string"}},
            {"aws_secret": {"type": "string"}},
            {"aws_region": {"type": "string"}},
            {"function_name": {"type": "string", "required": true}},
            {"qualifier": {"type": "string"}},
            {"invocation_type": {"type": "string", "default": "RequestResponse"}},
            {"log_type": {"type": "string", "default": "Tail"}},
            {"timeout": {"type": "number", "default": 60000}},
            {"port": {"type": "integer", "default": 443}},
            {"keepalive": {"type": "number", "default": 60000}},
            {"forward_request_method": {"type": "boolean", "default": false}},
            {"forward_request_headers": {"type": "boolean", "default": false}},
            {"forward_request_body": {"type": "boolean", "default": false}},
            {"forward_request_uri": {"type": "boolean", "default": false}},
            {"is_proxy_integration": {"type": "boolean", "default": false}},
            {"unhandled_status": {"type": "integer"}},
            {"skip_large_bodies": {"type": "boolean", "default": true}},
            {"base64_encode_body": {"type": "boolean", "default": true}},
        ]),
        "proxy-cache" => json!([
            {"response_code": {"type": "array", "elements": {"type": "integer"}, "default": [200, 301, 404]}},
            {"request_method": {"type": "array", "elements": {"type": "string"}, "default": ["GET", "HEAD"]}},
            {"content_type": {"type": "array", "elements": {"type": "string"}, "default": ["text/plain", "application/json"]}},
            {"cache_ttl": {"type": "integer", "default": 300}},
            {"strategy": {"type": "string", "required": true, "one_of": ["memory"]}},
            {"cache_control": {"type": "boolean", "default": false}},
            {"storage_ttl": {"type": "integer"}},
            {"memory": {"type": "record", "fields": [
                {"dictionary_name": {"type": "string", "default": "kong_db_cache"}},
            ]}},
            {"vary_headers": {"type": "array", "elements": {"type": "string"}}},
            {"vary_query_params": {"type": "array", "elements": {"type": "string"}}},
        ]),
        "pre-function" | "post-function" => json!([
            {"certificate": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"rewrite": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"access": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"header_filter": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"body_filter": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"log": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"ws_handshake": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"ws_client_frame": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"ws_upstream_frame": {"type": "array", "elements": {"type": "string"}, "default": []}},
            {"ws_close": {"type": "array", "elements": {"type": "string"}, "default": []}},
        ]),
        "dummy" => json!([
            {"resp_header_value": {"type": "string", "default": "1"}},
            {"resp_code": {"type": "number", "default": 200}},
            {"append_body": {"type": "string", "default": ""}},
            {"resp_headers": {"type": "map", "keys": {"type": "string"}, "values": {"type": "string"}}},
            {"old_field": {"type": "number", "default": 10, "deprecation": {
                "message": "dummy: old_field is deprecated, please use new_field instead",
                "old_default": 10,
                "removal_in_version": "4.0",
            }}},
            {"new_field": {"type": "number", "default": 10}},
        ]),
        "short-circuit" => json!([
            {"status": {"type": "integer", "default": 503}},
            {"message": {"type": "string", "default": "short-circuited"}},
        ]),
        "error-generator-last" => json!([
            {"access": {"type": "boolean", "default": false}},
            {"header_filter": {"type": "boolean", "default": false}},
            {"log": {"type": "boolean", "default": false}},
            {"rewrite": {"type": "boolean", "default": false}},
        ]),
        "ctx-checker" | "ctx-checker-last" => json!([
            {"ctx_kind": {"type": "string", "default": "kong.ctx.shared"}},
            {"ctx_set_field": {"type": "string", "default": ""}},
            {"ctx_set_value": {"type": "string", "default": ""}},
            {"ctx_check_field": {"type": "string", "default": ""}},
            {"ctx_check_value": {"type": "string", "default": ""}},
            {"ctx_throw_error": {"type": "boolean", "default": false}},
        ]),
        "enable-buffering" => json!([
            {"phase": {"type": "string", "default": "access"}},
            {"mode": {"type": "string", "default": "full"}},
        ]),
        "mocking" => json!([
            {"api_specification": {"type": "string"}},
        ]),
        "rewriter" => json!([
            {"value": {"type": "string", "default": ""}},
        ]),
        _ => json!([]),
    }
}

/// GET /schemas/plugins/{name} — Return plugin schema loaded from schema.lua. — GET /schemas/plugins/{name} — 返回从 schema.lua 加载的插件 schema。
pub async fn get_plugin_schema(
    Path(name): Path<String>,
    State(state): State<AdminState>,
) -> impl IntoResponse {
    let plugin_dirs = kong_lua_bridge::loader::resolve_plugin_dirs(&state.config.prefix);

    match kong_lua_bridge::loader::load_plugin_schema(&plugin_dirs, &name) {
        Ok(schema) => {
            // Ensure entity_checks is always present — 确保 entity_checks 始终存在
            let mut schema_json = schema;
            if !schema_json.get("entity_checks").is_some() {
                schema_json["entity_checks"] = json!([]);
            }
            (StatusCode::OK, Json(schema_json)).into_response()
        }
        Err(_err) => {
            // Fall back to minimal schema for known bundled plugins — 对已知内置插件回退到最小 schema
            if BUNDLED_PLUGINS.contains(&name.as_str()) {
                (StatusCode::OK, Json(minimal_plugin_schema(&name))).into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "message": format!("No plugin named '{}'", name),
                    })),
                )
                    .into_response()
            }
        }
    }
}

/// GET /schemas/vaults/{name} — Return vault schema — GET /schemas/vaults/{name} — 返回 vault schema
pub async fn get_vault_schema(
    Path(name): Path<String>,
) -> impl IntoResponse {
    match name.as_str() {
        "env" => {
            (StatusCode::OK, Json(json!({
                "fields": [
                    {"config": {"type": "record", "fields": [
                        {"prefix": {"type": "string", "description": "Environment variable prefix"}}
                    ]}}
                ],
                "name": "env",
            }))).into_response()
        }
        _ => {
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("No vault named '{}'", name),
                })),
            ).into_response()
        }
    }
}

/// POST /schemas/vaults/validate — Validate a vault config — POST /schemas/vaults/validate — 验证 vault 配置
pub async fn validate_vault_schema(
    body: Option<axum::Json<serde_json::Value>>,
) -> impl IntoResponse {
    let body = match body {
        Some(axum::Json(v)) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation (name: required field missing)",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": "required field missing"},
                })),
            ).into_response();
        }
    };

    // Validate vault name is present — 验证 vault name 字段是否存在
    let vault_name = match body.get("name").and_then(|v| v.as_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation (name: required field missing)",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": "required field missing"},
                })),
            ).into_response();
        }
    };

    // Check known vault types — 检查已知的 vault 类型
    match vault_name.as_str() {
        "env" => {
            // Validate prefix is present — 验证 prefix 字段
            match body.get("prefix").and_then(|v| v.as_str()) {
                Some(p) if !p.is_empty() => {
                    (
                        StatusCode::OK,
                        Json(json!({"message": "schema validation successful"})),
                    ).into_response()
                }
                _ => {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (prefix: required field missing)",
                            "name": "schema violation",
                            "code": 2,
                            "fields": {"prefix": "required field missing"},
                        })),
                    ).into_response()
                }
            }
        }
        _ => {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("No vault named '{}'", vault_name),
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": format!("No vault named '{}'", vault_name)},
                })),
            ).into_response()
        }
    }
}

/// POST /schemas/plugins/validate — Validate a plugin schema definition — POST /schemas/plugins/validate — 验证插件 schema 定义
pub async fn validate_plugin_schema(
    State(state): State<AdminState>,
    body: Option<axum::Json<serde_json::Value>>,
) -> impl IntoResponse {
    let body = match body {
        Some(axum::Json(v)) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation (name: required field missing)",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": "required field missing"},
                })),
            ).into_response();
        }
    };

    // Validate plugin name is present — 验证插件 name 字段是否存在
    let plugin_name = match body.get("name").and_then(|v| v.as_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation (name: required field missing)",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": "required field missing"},
                })),
            ).into_response();
        }
    };

    // Validate config fields against known schema — 验证 config 字段是否符合已知 schema
    let is_known = BUNDLED_PLUGINS.contains(&plugin_name.as_str());
    let plugin_dirs = kong_lua_bridge::loader::resolve_plugin_dirs(&state.config.prefix);
    let lua_loaded = kong_lua_bridge::loader::load_plugin_schema(&plugin_dirs, &plugin_name).ok();

    if !is_known && lua_loaded.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": format!("No plugin named '{}'", plugin_name),
                "name": "schema violation",
                "code": 2,
                "fields": {"name": format!("No plugin named '{}'", plugin_name)},
            })),
        ).into_response();
    }

    // If config is provided, validate its fields against known schema — 如果提供了 config，验证其字段是否在已知 schema 中
    if let Some(config) = body.get("config").and_then(|v| v.as_object()) {
        // Get known config fields from our schema definitions — 从 schema 定义中获取已知 config 字段
        let known_config_fields = get_known_config_fields(&plugin_name);
        if !known_config_fields.is_empty() {
            for key in config.keys() {
                // custom_fields_by_lua is always allowed — custom_fields_by_lua 始终允许
                if key == "custom_fields_by_lua" {
                    continue;
                }
                if !known_config_fields.contains(&key.as_str()) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": format!("schema violation (config.{}: unknown field)", key),
                            "name": "schema violation",
                            "code": 2,
                            "fields": {"config": {key.clone(): "unknown field"}},
                        })),
                    ).into_response();
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(json!({"message": "schema validation successful"})),
    ).into_response()
}

/// Extract known config field names from a plugin's schema definition — 从插件 schema 定义中提取已知 config 字段名
fn get_known_config_fields(plugin_name: &str) -> Vec<&'static str> {
    match plugin_name {
        "key-auth" => vec!["key_names", "key_in_body", "key_in_header", "key_in_query", "hide_credentials", "anonymous", "run_on_preflight"],
        "basic-auth" => vec!["hide_credentials", "anonymous"],
        "rate-limiting" => vec!["second", "minute", "hour", "day", "month", "year", "limit_by", "policy", "fault_tolerant", "hide_client_headers", "redis_host", "redis_port", "redis_password", "redis_timeout", "redis_database", "header_name", "path", "redis_ssl", "redis_ssl_verify", "redis_server_name", "error_code", "error_message", "sync_rate"],
        "cors" => vec!["origins", "methods", "headers", "exposed_headers", "credentials", "max_age", "preflight_continue", "private_network"],
        "tcp-log" => vec!["host", "port", "timeout", "keepalive", "tls", "tls_sni"],
        "udp-log" => vec!["host", "port", "timeout"],
        "http-log" => vec!["http_endpoint", "method", "content_type", "timeout", "keepalive", "flush_timeout", "retry_count", "queue_size", "custom_fields_by_lua"],
        "file-log" => vec!["path", "reopen", "custom_fields_by_lua"],
        "ip-restriction" => vec!["allow", "deny", "status", "message"],
        "request-transformer" => vec!["http_method", "remove", "rename", "replace", "add", "append"],
        "response-transformer" => vec!["remove", "rename", "replace", "add", "append"],
        "acl" => vec!["allow", "deny", "hide_groups_header"],
        "hmac-auth" => vec!["hide_credentials", "clock_skew", "algorithms", "enforce_headers", "validate_request_body"],
        "jwt" => vec!["uri_param_names", "cookie_names", "header_names", "key_claim_name", "secret_is_base64", "claims_to_verify", "anonymous", "run_on_preflight", "maximum_expiration"],
        "request-size-limiting" => vec!["allowed_payload_size", "size_unit", "require_content_length"],
        "request-termination" => vec!["status_code", "message", "body", "content_type", "trigger", "echo"],
        "bot-detection" => vec!["allow", "deny"],
        "correlation-id" => vec!["header_name", "generator", "echo_downstream"],
        "prometheus" => vec!["per_consumer", "status_code_metrics", "latency_metrics", "bandwidth_metrics", "upstream_health_metrics"],
        "oauth2" => vec!["scopes", "mandatory_scope", "provision_key", "token_expiration", "enable_authorization_code", "enable_client_credentials", "enable_implicit_grant", "enable_password_grant", "hide_credentials", "accept_http_if_already_terminated", "anonymous", "global_credentials", "auth_header_name", "refresh_token_ttl", "reuse_refresh_token", "persistent_refresh_token"],
        "ldap-auth" => vec!["ldap_host", "ldap_port", "start_tls", "verify_ldap_host", "base_dn", "attribute", "cache_ttl", "timeout", "keepalive", "anonymous", "header_type", "hide_credentials"],
        "session" => vec!["secret", "cookie_name", "cookie_lifetime", "cookie_path", "cookie_domain", "cookie_samesite", "cookie_httponly", "cookie_secure", "storage"],
        "response-ratelimiting" => vec!["header_name", "limit_by", "policy", "fault_tolerant", "hide_client_headers", "redis_host", "redis_port", "redis_password", "redis_timeout", "redis_database", "block_on_first_violation", "limits"],
        "syslog" => vec!["successful_severity", "client_errors_severity", "server_errors_severity", "log_level"],
        "loggly" => vec!["host", "port", "key", "tags", "timeout", "successful_severity", "client_errors_severity", "server_errors_severity", "log_level"],
        "datadog" => vec!["host", "port", "prefix", "metrics"],
        "statsd" => vec!["host", "port", "prefix", "metrics"],
        "zipkin" => vec!["http_endpoint", "sample_ratio", "default_service_name", "include_credential", "traceid_byte_count", "header_type", "default_header_type", "tags_header", "static_tags"],
        "grpc-gateway" => vec!["proto"],
        "grpc-web" => vec!["proto", "pass_stripped_path", "allow_origin_header"],
        "aws-lambda" => vec!["aws_key", "aws_secret", "aws_region", "function_name", "qualifier", "invocation_type", "log_type", "timeout", "port", "keepalive", "forward_request_method", "forward_request_headers", "forward_request_body", "forward_request_uri", "is_proxy_integration", "unhandled_status", "skip_large_bodies", "base64_encode_body"],
        "proxy-cache" => vec!["response_code", "request_method", "content_type", "cache_ttl", "strategy", "cache_control", "storage_ttl", "memory", "vary_headers", "vary_query_params"],
        "pre-function" | "post-function" => vec!["certificate", "rewrite", "access", "header_filter", "body_filter", "log", "ws_handshake", "ws_client_frame", "ws_upstream_frame", "ws_close"],
        "dummy" => vec!["resp_header_value", "resp_code", "append_body", "resp_headers", "old_field", "new_field"],
        "short-circuit" => vec!["status", "message"],
        "error-generator-last" => vec!["access", "header_filter", "log", "rewrite"],
        "ctx-checker" | "ctx-checker-last" => vec!["ctx_kind", "ctx_set_field", "ctx_set_value", "ctx_check_field", "ctx_check_value", "ctx_throw_error"],
        "enable-buffering" => vec!["phase", "mode"],
        "mocking" => vec!["api_specification"],
        "rewriter" => vec!["value"],
        _ => vec![], // Unknown plugin: skip config field validation — 未知插件：跳过 config 字段验证
    }
}
