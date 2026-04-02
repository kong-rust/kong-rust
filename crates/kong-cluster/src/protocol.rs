//! Clustering protocol — V1 GZIP + V2 JSON-RPC 2.0 + Snappy
//! 集群协议 — V1 全量推送 + V2 增量同步

use serde::{Deserialize, Serialize};
use serde_json::Value;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use flate2::Compression;
use std::io::{Read, Write};
use crate::ClusterError;

// ========== V1 Protocol — V1 协议 ==========

/// V1 reconfigure message from CP to DP — CP 推送给 DP 的 V1 重配置消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconfigurePayload {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub timestamp: f64,
    pub config_table: Value,
    pub config_hash: String,
    #[serde(default)]
    pub hashes: Option<super::ConfigHashes>,
}

/// DP basic_info sent after WebSocket connection — DP 连接后发送的基本信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicInfo {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub plugins: Vec<String>,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
}

impl BasicInfo {
    pub fn new(plugins: Vec<String>) -> Self {
        Self {
            msg_type: "basic_info".to_string(),
            plugins,
            labels: Default::default(),
        }
    }
}

/// GZIP compress data — GZIP 压缩
pub fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, ClusterError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(|e| ClusterError::Protocol(format!("gzip compress: {}", e)))?;
    encoder.finish().map_err(|e| ClusterError::Protocol(format!("gzip finish: {}", e)))
}

/// GZIP decompress data — GZIP 解压
pub fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, ClusterError> {
    let mut decoder = GzDecoder::new(data);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf).map_err(|e| ClusterError::Protocol(format!("gzip decompress: {}", e)))?;
    Ok(buf)
}

/// Build reconfigure payload and GZIP compress — 构建重配置消息并 GZIP 压缩
pub fn build_v1_payload(
    config_table: &Value,
    config_hash: &str,
    hashes: &super::ConfigHashes,
) -> Result<Vec<u8>, ClusterError> {
    let payload = ReconfigurePayload {
        msg_type: "reconfigure".to_string(),
        timestamp: chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
        config_table: config_table.clone(),
        config_hash: config_hash.to_string(),
        hashes: Some(hashes.clone()),
    };
    let json = serde_json::to_vec(&payload)?;
    gzip_compress(&json)
}

/// Parse V1 payload: GZIP decompress + JSON decode — 解析 V1 消息: GZIP 解压 + JSON 解码
pub fn parse_v1_payload(data: &[u8]) -> Result<ReconfigurePayload, ClusterError> {
    let json = gzip_decompress(data)?;
    serde_json::from_slice(&json).map_err(|e| ClusterError::Protocol(format!("json decode: {}", e)))
}

// ========== V2 Protocol — V2 协议 (JSON-RPC 2.0 + Snappy) ==========

/// JSON-RPC 2.0 request — JSON-RPC 2.0 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    pub id: u64,
}

/// JSON-RPC 2.0 response — JSON-RPC 2.0 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: u64,
}

/// JSON-RPC 2.0 error — JSON-RPC 2.0 错误
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 notification (no id) — JSON-RPC 2.0 通知（无 id）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// V2 init handshake params — V2 初始握手参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2InitParams {
    pub rpc_frame_encoding: String,
    pub rpc_version: String,
    pub rpc_frame_encodings: Vec<String>,
}

impl V2InitParams {
    pub fn new() -> Self {
        Self {
            rpc_frame_encoding: "x-snappy-framed".to_string(),
            rpc_version: "kong.sync.v1".to_string(),
            rpc_frame_encodings: vec!["x-snappy-framed".to_string()],
        }
    }
}

impl Default for V2InitParams {
    fn default() -> Self {
        Self::new()
    }
}

// V2 method names — V2 方法名
pub const V2_METHOD_INIT: &str = "kong.sync.v1.init";
pub const V2_METHOD_GET_DELTA: &str = "kong.sync.v1.get_delta";
pub const V2_METHOD_NOTIFY_NEW_VERSION: &str = "kong.sync.v1.notify_new_version";
pub const V2_METHOD_NOTIFY_VALIDATION_ERROR: &str = "kong.sync.v1.notify_validation_error";

/// Build V2 init request — 构建 V2 初始握手请求
pub fn build_v2_init_request() -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: V2_METHOD_INIT.to_string(),
        params: Some(serde_json::to_value(V2InitParams::new()).unwrap()),
        id: 1,
    }
}

/// Build V2 init response — 构建 V2 初始化响应
pub fn build_v2_init_response(request_id: u64) -> Vec<u8> {
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: Some(serde_json::json!({
            "ok": true
        })),
        error: None,
        id: request_id,
    };
    serde_json::to_vec(&response).unwrap_or_default()
}

/// Build V2 get_delta response with full config — 构建 V2 get_delta 响应（全量配置）
pub fn build_v2_delta_response(request_id: u64, config: &Value, version: u64) -> Vec<u8> {
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: Some(serde_json::json!({
            "version": version,
            "config": config,
        })),
        error: None,
        id: request_id,
    };
    serde_json::to_vec(&response).unwrap_or_default()
}

/// Build V2 notify_new_version notification — 构建 V2 新版本通知
pub fn build_v2_notify_new_version(version: u64) -> Vec<u8> {
    let notification = JsonRpcNotification {
        jsonrpc: "2.0".to_string(),
        method: V2_METHOD_NOTIFY_NEW_VERSION.to_string(),
        params: Some(serde_json::json!({
            "version": version,
        })),
    };
    serde_json::to_vec(&notification).unwrap_or_default()
}

/// Build V2 validation error notification — 构建 V2 验证错误通知
pub fn build_v2_notify_validation_error(errors: &[String]) -> Vec<u8> {
    let notification = JsonRpcNotification {
        jsonrpc: "2.0".to_string(),
        method: V2_METHOD_NOTIFY_VALIDATION_ERROR.to_string(),
        params: Some(serde_json::json!({
            "errors": errors,
        })),
    };
    serde_json::to_vec(&notification).unwrap_or_default()
}

/// Snappy compress — Snappy 压缩
pub fn snappy_compress(data: &[u8]) -> Result<Vec<u8>, ClusterError> {
    let mut encoder = snap::raw::Encoder::new();
    encoder.compress_vec(data).map_err(|e| ClusterError::Protocol(format!("snappy compress: {}", e)))
}

/// Snappy decompress — Snappy 解压
pub fn snappy_decompress(data: &[u8]) -> Result<Vec<u8>, ClusterError> {
    let mut decoder = snap::raw::Decoder::new();
    decoder.decompress_vec(data).map_err(|e| ClusterError::Protocol(format!("snappy decompress: {}", e)))
}

/// Encode V2 message: JSON + Snappy — 编码 V2 消息: JSON + Snappy
pub fn encode_v2_message<T: Serialize>(msg: &T) -> Result<Vec<u8>, ClusterError> {
    let json = serde_json::to_vec(msg)?;
    snappy_compress(&json)
}

/// Decode V2 message: Snappy + JSON — 解码 V2 消息: Snappy + JSON
pub fn decode_v2_message<T: for<'de> Deserialize<'de>>(data: &[u8]) -> Result<T, ClusterError> {
    let json = snappy_decompress(data)?;
    serde_json::from_slice(&json).map_err(|e| ClusterError::Protocol(format!("v2 json decode: {}", e)))
}
