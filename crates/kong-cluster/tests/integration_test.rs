//! Kong Cluster integration tests — Kong 集群集成测试
//! Tests CP/DP WebSocket communication end-to-end
//! 测试 CP/DP WebSocket 端到端通信

use futures_util::{SinkExt, StreamExt};
use kong_cluster::cp::ControlPlane;
use kong_cluster::dp::DataPlane;
use kong_cluster::protocol;
use kong_cluster::{ConfigHashes, DataPlaneInfo, SyncStatus, EMPTY_CONFIG_HASH};
use std::sync::Arc;
use std::time::Duration;

// ===== CP Unit-level Integration Tests — CP 单元级集成测试 =====

/// Test CP can register and unregister DPs — 测试 CP 可以注册和注销 DP
#[tokio::test]
async fn test_cp_register_unregister_dp() {
    let cp = ControlPlane::new();

    let dp_id = uuid::Uuid::new_v4();
    let dp_info = DataPlaneInfo {
        id: dp_id,
        ip: "127.0.0.1".to_string(),
        hostname: "test-dp".to_string(),
        version: "0.1.0".to_string(),
        sync_status: SyncStatus::Unknown,
        config_hash: EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: std::collections::HashMap::new(),
    };

    // Register — 注册
    let _rx = cp.register_dp(dp_info).await;
    assert_eq!(cp.list_data_planes().await.len(), 1);

    // Unregister — 注销
    cp.unregister_dp(&dp_id).await;
    assert_eq!(cp.list_data_planes().await.len(), 0);
}

/// Test CP push config broadcasts to all DPs — 测试 CP 推送配置广播给所有 DP
#[tokio::test]
async fn test_cp_push_config_broadcast() {
    let cp = ControlPlane::new();

    // Register 2 DPs — 注册 2 个 DP
    let dp1_id = uuid::Uuid::new_v4();
    let dp2_id = uuid::Uuid::new_v4();

    let make_dp = |id: uuid::Uuid, name: &str| DataPlaneInfo {
        id,
        ip: "10.0.0.1".to_string(),
        hostname: name.to_string(),
        version: "0.1.0".to_string(),
        sync_status: SyncStatus::Unknown,
        config_hash: EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: std::collections::HashMap::new(),
    };

    let mut rx1 = cp.register_dp(make_dp(dp1_id, "dp1")).await;
    let mut rx2 = cp.register_dp(make_dp(dp2_id, "dp2")).await;
    assert_eq!(cp.list_data_planes().await.len(), 2);

    // Push config — 推送配置
    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/test"]}],
        "services": [{"id": "s1", "host": "example.com", "port": 80}],
        "plugins": [],
        "upstreams": [],
        "targets": []
    });

    cp.push_config(&config).await.unwrap();

    // Both DPs should receive — 两个 DP 都应该收到
    let payload1 = tokio::time::timeout(Duration::from_secs(1), rx1.recv())
        .await.expect("timeout").expect("recv failed");
    let payload2 = tokio::time::timeout(Duration::from_secs(1), rx2.recv())
        .await.expect("timeout").expect("recv failed");

    // Payloads should be identical (Arc<Vec<u8>> zero-copy) — 载荷应该相同（Arc 零拷贝）
    assert_eq!(payload1.len(), payload2.len());
    assert!(!payload1.is_empty());

    // Verify it's valid GZIP — 验证是有效的 GZIP
    let decompressed = protocol::gzip_decompress(&payload1).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&decompressed).unwrap();
    assert_eq!(parsed["type"], "reconfigure");
    let hash_str = parsed["config_hash"].as_str().unwrap();
    assert_eq!(hash_str.len(), 32);
}

/// Test CP config hash is updated after push — 测试推送后 CP 配置哈希更新
#[tokio::test]
async fn test_cp_config_hash_update() {
    let cp = ControlPlane::new();

    assert_eq!(cp.current_hash().await, EMPTY_CONFIG_HASH);

    let config = serde_json::json!({"routes": [], "services": []});
    cp.push_config(&config).await.unwrap();

    let hash = cp.current_hash().await;
    assert_ne!(hash, EMPTY_CONFIG_HASH);
    assert_eq!(hash.len(), 32);
}

/// Test CP DP status update — 测试 CP 更新 DP 状态
#[tokio::test]
async fn test_cp_dp_status_update() {
    let cp = ControlPlane::new();

    let dp_id = uuid::Uuid::new_v4();
    let dp_info = DataPlaneInfo {
        id: dp_id,
        ip: "127.0.0.1".to_string(),
        hostname: "test-dp".to_string(),
        version: "0.1.0".to_string(),
        sync_status: SyncStatus::Unknown,
        config_hash: EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: std::collections::HashMap::new(),
    };

    let _rx = cp.register_dp(dp_info).await;

    // Push config to set CP hash — 推送配置设置 CP 哈希
    let config = serde_json::json!({"routes": []});
    cp.push_config(&config).await.unwrap();
    let expected_hash = cp.current_hash().await;

    // Update DP status with matching hash — 使用匹配的哈希更新 DP 状态
    cp.update_dp_status(&dp_id, &expected_hash).await;

    // DP should now be "normal" — DP 应该现在是 "normal"
    let dps = cp.list_data_planes().await;
    let dp = dps.iter().find(|d| d.id == dp_id).unwrap();
    assert_eq!(dp.sync_status, SyncStatus::Normal);
    assert_eq!(dp.config_hash, expected_hash);
}

/// Test CP stale DP cleanup — 测试 CP 清理过期 DP
#[tokio::test]
async fn test_cp_stale_dp_cleanup() {
    let cp = ControlPlane::new();

    let dp_id = uuid::Uuid::new_v4();
    let dp_info = DataPlaneInfo {
        id: dp_id,
        ip: "127.0.0.1".to_string(),
        hostname: "stale-dp".to_string(),
        version: "0.1.0".to_string(),
        sync_status: SyncStatus::Unknown,
        config_hash: EMPTY_CONFIG_HASH.to_string(),
        // Set last_seen to 2 minutes ago — 设置 last_seen 为 2 分钟前
        last_seen: chrono::Utc::now() - chrono::Duration::seconds(120),
        labels: std::collections::HashMap::new(),
    };

    let _rx = cp.register_dp(dp_info).await;
    assert_eq!(cp.list_data_planes().await.len(), 1);

    // Cleanup with 60s threshold — 使用 60s 阈值清理
    cp.purge_stale_dps(60).await;
    assert_eq!(cp.list_data_planes().await.len(), 0);
}

// ===== DP Unit-level Integration Tests — DP 单元级集成测试 =====

/// Test DP initial state — 测试 DP 初始状态
#[tokio::test]
async fn test_dp_initial_state() {
    let dp = DataPlane::new(
        "127.0.0.1:9005",
        "/tmp/kong-test-dp",
        vec!["key-auth".to_string()],
        uuid::Uuid::new_v4(),
        "test-host".to_string(),
    );

    assert_eq!(dp.get_current_hash().await, EMPTY_CONFIG_HASH);
    assert!(!dp.is_connected().await);
    assert!(!dp.is_config_ready().await);
}

/// Test DP mark config applied — 测试 DP 标记配置已应用
#[tokio::test]
async fn test_dp_mark_config_applied() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dp = DataPlane::new(
        "127.0.0.1:9005",
        tmp.path().to_str().unwrap(),
        vec![],
        uuid::Uuid::new_v4(),
        "test-host".to_string(),
    );

    let config = serde_json::json!({"routes": [{"id": "r1"}]});
    let hash = "abc123def456abc123def456abc12345".to_string();
    let hashes = ConfigHashes::default();

    dp.mark_config_applied(&config, &hash, hashes).await;

    assert_eq!(dp.get_current_hash().await, hash);
    assert!(dp.is_config_ready().await);

    // Should be saved to disk — 应该已保存到磁盘
    let cached = dp.try_load_from_cache().await;
    assert!(cached.is_some());
}

// ===== Protocol Tests — 协议测试 =====

/// Test protocol config hash is deterministic — 测试协议配置哈希是确定性的
#[test]
fn test_protocol_hash_deterministic() {
    use kong_cluster::cp::calculate_config_hash;

    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/foo"]}],
        "services": [{"id": "s1", "host": "httpbin.org"}],
        "plugins": [{"id": "p1", "name": "key-auth"}],
        "upstreams": [],
        "targets": []
    });

    let hashes1 = calculate_config_hash(&config);
    let hashes2 = calculate_config_hash(&config);

    assert_eq!(hashes1.config, hashes2.config);
    assert_eq!(hashes1.routes, hashes2.routes);
    assert_eq!(hashes1.services, hashes2.services);
    assert_eq!(hashes1.plugins, hashes2.plugins);
}

/// Test protocol GZIP roundtrip preserves reconfigure payload
/// 测试协议 GZIP 往返保留重配置载荷
#[test]
fn test_protocol_gzip_reconfigure_roundtrip() {
    use kong_cluster::cp::calculate_config_hash;

    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/api"]}],
        "services": [{"id": "s1", "host": "backend.local", "port": 8080}],
        "plugins": [],
        "upstreams": [],
        "targets": []
    });

    let hashes = calculate_config_hash(&config);

    let payload = serde_json::json!({
        "type": "reconfigure",
        "timestamp": 1234567890.123_f64,
        "config_table": config,
        "config_hash": hashes.config,
        "hashes": {
            "config": hashes.config,
            "routes": hashes.routes,
            "services": hashes.services,
            "plugins": hashes.plugins,
            "upstreams": hashes.upstreams,
            "targets": hashes.targets,
        }
    });

    let json_bytes = serde_json::to_vec(&payload).unwrap();
    let compressed = protocol::gzip_compress(&json_bytes).unwrap();
    let decompressed = protocol::gzip_decompress(&compressed).unwrap();
    let restored: serde_json::Value = serde_json::from_slice(&decompressed).unwrap();

    assert_eq!(restored["type"], "reconfigure");
    assert_eq!(restored["config_hash"], hashes.config);
    assert_eq!(restored["config_table"]["routes"][0]["id"], "r1");
}

/// Test protocol snappy roundtrip for V2 — 测试 V2 协议 Snappy 往返
#[test]
fn test_protocol_snappy_v2_roundtrip() {
    let rpc_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "kong.sync.v1.init",
        "params": {
            "rpc_frame_encoding": "x-snappy-framed",
            "rpc_version": "kong.sync.v1",
        },
        "id": 1
    });

    let json_bytes = serde_json::to_vec(&rpc_msg).unwrap();
    let compressed = protocol::snappy_compress(&json_bytes).unwrap();
    let decompressed = protocol::snappy_decompress(&compressed).unwrap();
    let restored: serde_json::Value = serde_json::from_slice(&decompressed).unwrap();

    assert_eq!(restored["jsonrpc"], "2.0");
    assert_eq!(restored["method"], "kong.sync.v1.init");
}

// ===== End-to-End WebSocket Test — 端到端 WebSocket 测试 =====

/// Test CP WebSocket server with real axum — 测试 CP 与真实 axum WebSocket 服务器
/// Start CP server, connect DP client, exchange config
/// 启动 CP 服务器，连接 DP 客户端，交换配置
#[tokio::test]
async fn test_cp_dp_websocket_e2e() {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::extract::Query;
    use axum::routing::get;

    let cp = Arc::new(ControlPlane::new());

    // Build axum app with /v1/outlet — 构建带 /v1/outlet 的 axum 应用
    let cp_for_route = cp.clone();
    let app = axum::Router::new().route(
        "/v1/outlet",
        get(move |ws: WebSocketUpgrade, Query(params): Query<std::collections::HashMap<String, String>>| {
            let cp = cp_for_route.clone();
            async move {
                ws.on_upgrade(move |mut socket| async move {
                    let dp_id = params.get("node_id")
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                        .unwrap_or_else(uuid::Uuid::new_v4);

                    let dp_info = DataPlaneInfo {
                        id: dp_id,
                        ip: "127.0.0.1".to_string(),
                        hostname: params.get("node_hostname").cloned().unwrap_or_default(),
                        version: "0.1.0".to_string(),
                        sync_status: SyncStatus::Unknown,
                        config_hash: EMPTY_CONFIG_HASH.to_string(),
                        last_seen: chrono::Utc::now(),
                        labels: std::collections::HashMap::new(),
                    };

                    let mut config_rx = cp.register_dp(dp_info).await;

                    // Read basic_info — 读取 basic_info
                    if let Some(Ok(Message::Binary(_))) = socket.recv().await {}

                    // Wait for config update and send to DP — 等待配置更新并发送给 DP
                    if let Ok(payload) = config_rx.recv().await {
                        let _ = socket.send(Message::Binary(payload.as_ref().to_vec().into())).await;
                    }

                    cp.unregister_dp(&dp_id).await;
                })
            }
        }),
    );

    // Start server on random port — 在随机端口启动服务器
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start — 等服务器启动
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect DP client — 连接 DP 客户端
    let dp_id = uuid::Uuid::new_v4();
    let url = format!("ws://{}/v1/outlet?node_id={}&node_hostname=test-dp", addr, dp_id);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Send basic_info — 发送 basic_info
    let basic_info = serde_json::json!({
        "type": "basic_info",
        "plugins": ["key-auth", "rate-limiting"]
    });
    let msg: tokio_tungstenite::tungstenite::Message = tokio_tungstenite::tungstenite::Message::Binary(
        serde_json::to_vec(&basic_info).unwrap().into(),
    );
    ws_write.send(msg).await.unwrap();

    // Push config from CP side — 从 CP 侧推送配置
    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/test"]}],
        "services": [],
        "plugins": [],
        "upstreams": [],
        "targets": []
    });
    cp.push_config(&config).await.unwrap();

    // DP should receive the config — DP 应该收到配置
    let received = tokio::time::timeout(Duration::from_secs(2), ws_read.next())
        .await
        .expect("timeout waiting for config");

    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) = received {
        let decompressed = protocol::gzip_decompress(&data).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&decompressed).unwrap();
        assert_eq!(parsed["type"], "reconfigure");
        assert_eq!(parsed["config_table"]["routes"][0]["paths"][0], "/test");
    } else {
        panic!("Expected binary message with config — 期望收到二进制配置消息");
    }

    // Close connection — 关闭连接
    let close_msg: tokio_tungstenite::tungstenite::Message = tokio_tungstenite::tungstenite::Message::Close(None);
    ws_write.send(close_msg).await.ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Test multi-level config hash matches across different entity combinations
/// 测试多级配置哈希在不同实体组合下的一致性
#[test]
fn test_config_hash_entity_independence() {
    use kong_cluster::cp::calculate_config_hash;

    // Same routes, different services should produce different hashes
    // 相同路由，不同服务应产生不同哈希
    let config1 = serde_json::json!({
        "routes": [{"id": "r1"}],
        "services": [{"id": "s1", "host": "a.com"}],
        "plugins": [], "upstreams": [], "targets": []
    });
    let config2 = serde_json::json!({
        "routes": [{"id": "r1"}],
        "services": [{"id": "s1", "host": "b.com"}],
        "plugins": [], "upstreams": [], "targets": []
    });

    let hashes1 = calculate_config_hash(&config1);
    let hashes2 = calculate_config_hash(&config2);

    // Overall hashes should differ — 总体哈希应不同
    assert_ne!(hashes1.config, hashes2.config);

    // Routes hashes should be the same — 路由哈希应相同
    assert_eq!(hashes1.routes, hashes2.routes);

    // Services hashes should differ — 服务哈希应不同
    assert_ne!(hashes1.services, hashes2.services);
}

/// Test empty config produces consistent hash — 测试空配置产生一致的哈希
#[test]
fn test_empty_config_hash() {
    use kong_cluster::cp::calculate_config_hash;

    let config = serde_json::json!({});
    let h1 = calculate_config_hash(&config);
    let h2 = calculate_config_hash(&config);
    assert_eq!(h1.config, h2.config);
    assert_eq!(h1.config.len(), 32);
}

// ===== Task 2a: Full DP connection loop E2E — 完整 DP 连接循环端到端测试 =====

/// Helper: start a CP WebSocket server on random port, return (addr, cp, shutdown_tx)
/// 辅助函数: 在随机端口启动 CP WebSocket 服务，返回 (地址, CP, 关闭发送端)
async fn start_cp_server() -> (
    std::net::SocketAddr,
    Arc<ControlPlane>,
    tokio::sync::watch::Sender<bool>,
) {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::extract::Query;
    use axum::routing::get;

    let cp = Arc::new(ControlPlane::new());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let cp_for_route = cp.clone();
    let app = axum::Router::new().route(
        "/v1/outlet",
        get(move |ws: WebSocketUpgrade, Query(params): Query<std::collections::HashMap<String, String>>| {
            let cp = cp_for_route.clone();
            async move {
                ws.on_upgrade(move |socket| async move {
                    handle_dp_ws(socket, cp, params).await;
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let mut shutdown_rx_clone = shutdown_rx.clone();
    tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, app) => {
                if let Err(e) = result { eprintln!("CP server error: {}", e); }
            }
            _ = shutdown_rx_clone.changed() => {}
        }
    });

    // Wait for server to be ready — 等待服务器就绪
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, cp, shutdown_tx)
}

/// Helper: handle a DP WebSocket connection on CP side (full read/write loop)
/// 辅助函数: 在 CP 侧处理 DP WebSocket 连接（完整读写循环）
async fn handle_dp_ws(
    socket: axum::extract::ws::WebSocket,
    cp: Arc<ControlPlane>,
    params: std::collections::HashMap<String, String>,
) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};

    let dp_id = params.get("node_id")
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);

    let dp_info = DataPlaneInfo {
        id: dp_id,
        ip: "127.0.0.1".to_string(),
        hostname: params.get("node_hostname").cloned().unwrap_or_default(),
        version: params.get("node_version").cloned().unwrap_or("0.1.0".to_string()),
        sync_status: SyncStatus::Unknown,
        config_hash: EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: std::collections::HashMap::new(),
    };

    let mut config_rx = cp.register_dp(dp_info).await;
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Wait for basic_info from DP — 等待 DP 发送 basic_info
    if let Some(Ok(Message::Binary(_))) = ws_receiver.next().await {}

    // Read/write loop — 读写循环
    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        // DP sends PING with config hash — DP 发送带哈希的 PING
                        let hash = String::from_utf8_lossy(&data).to_string();
                        cp.update_dp_status(&dp_id, &hash).await;
                        if ws_sender.send(Message::Pong(data)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            payload = config_rx.recv() => {
                match payload {
                    Ok(data) => {
                        let msg = Message::Binary(data.as_ref().to_vec().into());
                        if ws_sender.send(msg).await.is_err() { break; }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    cp.unregister_dp(&dp_id).await;
}

/// Test full DP connection loop: connect → basic_info → receive config → PING with hash → CP marks normal
/// 测试完整 DP 连接循环: 连接 → basic_info → 接收配置 → 带哈希 PING → CP 标记为 normal
#[tokio::test]
async fn test_dp_full_connection_loop() {
    let (addr, cp, _shutdown_tx) = start_cp_server().await;

    let dp_id = uuid::Uuid::new_v4();
    let url = format!("ws://{}/v1/outlet?node_id={}&node_hostname=loop-test-dp", addr, dp_id);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Step 1: Send basic_info — 步骤 1: 发送 basic_info
    let basic_info = serde_json::json!({
        "type": "basic_info",
        "plugins": ["key-auth"]
    });
    ws_write.send(tokio_tungstenite::tungstenite::Message::Binary(
        serde_json::to_vec(&basic_info).unwrap().into(),
    )).await.unwrap();

    // Step 2: CP pushes config — 步骤 2: CP 推送配置
    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/loop-test"]}],
        "services": [{"id": "s1", "host": "loop.test"}],
        "plugins": [], "upstreams": [], "targets": []
    });
    cp.push_config(&config).await.unwrap();
    let expected_hash = cp.current_hash().await;

    // Step 3: DP receives config — 步骤 3: DP 接收配置
    let received = tokio::time::timeout(Duration::from_secs(2), ws_read.next())
        .await.expect("timeout").unwrap().unwrap();
    let data = match received {
        tokio_tungstenite::tungstenite::Message::Binary(d) => d,
        other => panic!("Expected binary, got {:?} — 期望二进制消息，得到 {:?}", other, other),
    };
    let parsed_payload = protocol::parse_v1_payload(&data).unwrap();
    assert_eq!(parsed_payload.msg_type, "reconfigure");
    assert_eq!(parsed_payload.config_hash, expected_hash);
    assert_eq!(parsed_payload.config_table["routes"][0]["paths"][0], "/loop-test");

    // Step 4: DP sends PING with received config hash — 步骤 4: DP 发送带配置哈希的 PING
    let ping = tokio_tungstenite::tungstenite::Message::Ping(
        expected_hash.as_bytes().to_vec().into()
    );
    ws_write.send(ping).await.unwrap();

    // Wait for PONG response — 等待 PONG 响应
    let pong = tokio::time::timeout(Duration::from_secs(2), ws_read.next())
        .await.expect("timeout").unwrap().unwrap();
    match pong {
        tokio_tungstenite::tungstenite::Message::Pong(data) => {
            let hash_str = String::from_utf8_lossy(&data).to_string();
            assert_eq!(hash_str, expected_hash);
        }
        other => panic!("Expected Pong, got {:?} — 期望 Pong，得到 {:?}", other, other),
    }

    // Step 5: Verify CP marks DP as "normal" — 步骤 5: 验证 CP 将 DP 标记为 "normal"
    // Give CP a moment to process — 给 CP 一点处理时间
    tokio::time::sleep(Duration::from_millis(100)).await;
    let dps = cp.list_data_planes().await;
    let dp = dps.iter().find(|d| d.id == dp_id).unwrap();
    assert_eq!(dp.sync_status, SyncStatus::Normal, "DP should be normal after PING with matching hash — DP 在带匹配哈希的 PING 后应为 normal");
    assert_eq!(dp.config_hash, expected_hash);

    // Cleanup — 清理
    ws_write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await.ok();
}

// ===== Task 2b: DP Reconnection Test — DP 重连测试 =====

/// Test DP reconnects after CP restart — 测试 CP 重启后 DP 能重连
#[tokio::test]
async fn test_dp_reconnection_after_cp_restart() {
    // Start CP #1 — 启动 CP #1
    let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener1.local_addr().unwrap();

    let cp1 = Arc::new(ControlPlane::new());
    let cp1_clone = cp1.clone();
    let (shutdown_tx1, mut shutdown_rx1) = tokio::sync::watch::channel(false);

    let app1 = build_cp_app(cp1_clone);
    let server1 = tokio::spawn(async move {
        tokio::select! {
            r = axum::serve(listener1, app1) => { r.ok(); }
            _ = shutdown_rx1.changed() => {}
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect DP to CP #1 — 连接 DP 到 CP #1
    let dp_id = uuid::Uuid::new_v4();
    let url = format!("ws://{}/v1/outlet?node_id={}&node_hostname=reconnect-dp", addr, dp_id);
    let (ws1, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut ws1_write, _ws1_read) = ws1.split();

    // Send basic_info — 发送 basic_info
    let basic_info = serde_json::json!({"type": "basic_info", "plugins": []});
    ws1_write.send(tokio_tungstenite::tungstenite::Message::Binary(
        serde_json::to_vec(&basic_info).unwrap().into(),
    )).await.unwrap();

    assert_eq!(cp1.list_data_planes().await.len(), 1);

    // Shutdown CP #1 — 关闭 CP #1
    shutdown_tx1.send(true).ok();
    server1.await.ok();
    drop(ws1_write);

    // Small delay — 短暂延迟
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Start CP #2 on the SAME port — 在相同端口启动 CP #2
    let listener2 = tokio::net::TcpListener::bind(addr).await.unwrap();
    let cp2 = Arc::new(ControlPlane::new());
    let cp2_clone = cp2.clone();
    let (_shutdown_tx2, mut shutdown_rx2) = tokio::sync::watch::channel(false);

    let app2 = build_cp_app(cp2_clone);
    tokio::spawn(async move {
        tokio::select! {
            r = axum::serve(listener2, app2) => { r.ok(); }
            _ = shutdown_rx2.changed() => {}
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // DP reconnects to CP #2 — DP 重连到 CP #2
    let (ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut ws2_write, _ws2_read) = ws2.split();

    ws2_write.send(tokio_tungstenite::tungstenite::Message::Binary(
        serde_json::to_vec(&basic_info).unwrap().into(),
    )).await.unwrap();

    // Give time for registration — 等待注册
    tokio::time::sleep(Duration::from_millis(100)).await;

    // CP #2 should have the DP registered — CP #2 应该有 DP 注册
    assert_eq!(cp2.list_data_planes().await.len(), 1, "DP should reconnect to new CP — DP 应该重连到新 CP");

    ws2_write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await.ok();
}

/// Helper: build CP axum app — 辅助函数: 构建 CP axum 应用
fn build_cp_app(cp: Arc<ControlPlane>) -> axum::Router {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::extract::Query;
    use axum::routing::get;

    axum::Router::new().route(
        "/v1/outlet",
        get(move |ws: WebSocketUpgrade, Query(params): Query<std::collections::HashMap<String, String>>| {
            let cp = cp.clone();
            async move {
                ws.on_upgrade(move |socket| handle_dp_ws(socket, cp, params))
            }
        }),
    )
}

// ===== Task 2c: Config Hash Kong Lua Compatibility Tests — 配置哈希 Kong Lua 兼容性测试 =====

/// Kong Lua test vectors for calculate_config_hash primitive values
/// Kong Lua 的 calculate_config_hash 原始值测试向量
///
/// Note: Kong Lua's calculate_config_hash hashes a SINGLE value, while our
/// calculate_config_hash takes a config table. The Lua primitive tests correspond
/// to our hash_entity(Some(value)) which does to_sorted_string + MD5.
/// 注意: Kong Lua 的 calculate_config_hash 对单个值哈希，而我们的 calculate_config_hash
/// 接受配置表。Lua 原始值测试对应我们的 hash_entity(Some(value))。
mod kong_lua_hash_compat {
    use kong_cluster::cp::{hash_entity, md5_hex, to_sorted_string};
    use serde_json::{json, Value};

    /// MD5("/null/") = "5bf07a8b7343015026657d1108d8206e" (Kong: ngx.null)
    #[test]
    fn test_hash_null_matches_kong() {
        let hash = hash_entity(Some(&Value::Null));
        assert_eq!(hash, "5bf07a8b7343015026657d1108d8206e",
            "null hash should match Kong Lua — null 哈希应匹配 Kong Lua");

        // Verify via md5 of "/null/" — 通过 "/null/" 的 MD5 验证
        assert_eq!(md5_hex(b"/null/"), "5bf07a8b7343015026657d1108d8206e");
    }

    /// MD5("10") = "d3d9446802a44259755d38e6d163e820" (Kong: number 10)
    #[test]
    fn test_hash_number_matches_kong() {
        let hash = hash_entity(Some(&json!(10)));
        assert_eq!(hash, "d3d9446802a44259755d38e6d163e820",
            "number 10 hash should match Kong Lua — 数字 10 哈希应匹配 Kong Lua");
    }

    /// MD5("0.9") = "a894124cc6d5c5c71afe060d5dde0762" (Kong: double 0.9)
    #[test]
    fn test_hash_double_matches_kong() {
        let hash = hash_entity(Some(&json!(0.9)));
        assert_eq!(hash, "a894124cc6d5c5c71afe060d5dde0762",
            "double 0.9 hash should match Kong Lua — 浮点数 0.9 哈希应匹配 Kong Lua");
    }

    /// MD5("") = "d41d8cd98f00b204e9800998ecf8427e" (Kong: empty string "")
    #[test]
    fn test_hash_empty_string_matches_kong() {
        let hash = hash_entity(Some(&json!("")));
        assert_eq!(hash, "d41d8cd98f00b204e9800998ecf8427e",
            "empty string hash should match Kong Lua — 空字符串哈希应匹配 Kong Lua");
    }

    /// MD5("hello") = "5d41402abc4b2a76b9719d911017c592" (Kong: string "hello")
    #[test]
    fn test_hash_string_matches_kong() {
        let hash = hash_entity(Some(&json!("hello")));
        assert_eq!(hash, "5d41402abc4b2a76b9719d911017c592",
            "string 'hello' hash should match Kong Lua — 字符串 'hello' 哈希应匹配 Kong Lua");
    }

    /// MD5("false") = "68934a3e9455fa72420237eb05902327" (Kong: boolean false)
    #[test]
    fn test_hash_bool_false_matches_kong() {
        let hash = hash_entity(Some(&json!(false)));
        assert_eq!(hash, "68934a3e9455fa72420237eb05902327",
            "boolean false hash should match Kong Lua — 布尔 false 哈希应匹配 Kong Lua");
    }

    /// MD5("true") = "b326b5062b2f0e69046810717534cb09" (Kong: boolean true)
    #[test]
    fn test_hash_bool_true_matches_kong() {
        let hash = hash_entity(Some(&json!(true)));
        assert_eq!(hash, "b326b5062b2f0e69046810717534cb09",
            "boolean true hash should match Kong Lua — 布尔 true 哈希应匹配 Kong Lua");
    }

    /// MD5("{}") = "99914b932bd37a50b983c5e7c90ae93b" (Kong: empty table {})
    /// Note: Kong treats empty table `{}` as empty array, serialized to "{}"
    /// 注意: Kong 将空表 {} 视为空数组，序列化为 "{}"
    #[test]
    fn test_hash_empty_table_matches_kong() {
        // Empty JSON array [] maps to Lua empty table {} — 空 JSON 数组 [] 对应 Lua 空表 {}
        let hash = hash_entity(Some(&json!([])));
        assert_eq!(hash, "99914b932bd37a50b983c5e7c90ae93b",
            "empty array hash should match Kong Lua empty table — 空数组哈希应匹配 Kong Lua 空表");

        // Empty JSON object {} also maps to "{}" — 空 JSON 对象 {} 也映射为 "{}"
        let hash2 = hash_entity(Some(&json!({})));
        assert_eq!(hash2, "99914b932bd37a50b983c5e7c90ae93b",
            "empty object hash should also match — 空对象哈希也应匹配");
    }

    /// to_sorted_string should produce consistent output for nested structures
    /// to_sorted_string 对嵌套结构应产生一致输出
    #[test]
    fn test_sorted_string_nested_object() {
        let mut buf = String::new();
        to_sorted_string(&json!({"z": {"b": 2, "a": 1}, "a": [1, 2]}), &mut buf);
        // "a" sorts before "z" — "a" 排在 "z" 前面
        // array [1,2] => "1;2;" — 数组 [1,2] => "1;2;"
        // object {"a":1,"b":2} => "a:1;b:2;" — 对象 {"a":1,"b":2} => "a:1;b:2;"
        assert_eq!(buf, "a:1;2;;z:a:1;b:2;;");
    }

    /// to_sorted_string handles mixed types correctly — 正确处理混合类型
    #[test]
    fn test_sorted_string_mixed_types() {
        let mut buf = String::new();
        to_sorted_string(&json!({
            "bool1": true,
            "bool2": false,
            "number": 1,
            "double": 1.1,
            "empty": {},
            "null": null,
            "string": "test",
            "hash": {"k": "v"},
            "array": ["v1", "v2"]
        }), &mut buf);
        // Keys sorted alphabetically — 键按字母排序
        assert!(buf.contains("array:v1;v2;;"));
        assert!(buf.contains("bool1:true;"));
        assert!(buf.contains("bool2:false;"));
        assert!(buf.contains("double:1.1;"));
        assert!(buf.contains("empty:{};"));
        assert!(buf.contains("null:/null/;"));
        assert!(buf.contains("number:1;"));
        assert!(buf.contains("string:test;"));
        assert!(buf.contains("hash:k:v;;"));
    }

    /// Granular hashes: empty config {} should have EMPTY_CONFIG_HASH for missing entity fields
    /// 粒度哈希: 空配置 {} 对于缺失的实体字段应使用 EMPTY_CONFIG_HASH
    #[test]
    fn test_granular_hashes_empty_config() {
        use kong_cluster::cp::calculate_config_hash;
        let hashes = calculate_config_hash(&json!({}));

        // Missing fields hash to null hash — 缺失字段哈希为 null 哈希
        let null_hash = hash_entity(Some(&Value::Null));
        assert_eq!(hashes.routes, null_hash, "routes should hash as null — routes 应哈希为 null");
        assert_eq!(hashes.services, null_hash, "services should hash as null — services 应哈希为 null");
        assert_eq!(hashes.plugins, null_hash, "plugins should hash as null — plugins 应哈希为 null");
        assert_eq!(hashes.upstreams, null_hash, "upstreams should hash as null — upstreams 应哈希为 null");
        assert_eq!(hashes.targets, null_hash, "targets should hash as null — targets 应哈希为 null");
    }

    /// Granular hashes: config with empty entity arrays — 粒度哈希: 带空实体数组的配置
    #[test]
    fn test_granular_hashes_empty_entities() {
        use kong_cluster::cp::calculate_config_hash;

        let hashes = calculate_config_hash(&json!({
            "routes": [],
            "services": [],
            "plugins": [],
        }));

        let empty_array_hash = hash_entity(Some(&json!([])));
        assert_eq!(empty_array_hash, "99914b932bd37a50b983c5e7c90ae93b");

        assert_eq!(hashes.routes, empty_array_hash);
        assert_eq!(hashes.services, empty_array_hash);
        assert_eq!(hashes.plugins, empty_array_hash);

        // upstreams/targets not provided => null hash — upstreams/targets 未提供 => null 哈希
        let null_hash = hash_entity(Some(&Value::Null));
        assert_eq!(hashes.upstreams, null_hash);
        assert_eq!(hashes.targets, null_hash);
    }
}

// ===== Task 2d: TLS Config Validation Tests — TLS 配置验证测试 =====

mod tls_config_tests {
    use kong_cluster::tls::{ClusterTlsConfig, ClusterTlsMode};

    /// Test TLS mode parsing — 测试 TLS 模式解析
    #[test]
    fn test_tls_mode_parsing() {
        assert_eq!(ClusterTlsMode::from_str("shared"), ClusterTlsMode::Shared);
        assert_eq!(ClusterTlsMode::from_str("pki"), ClusterTlsMode::Pki);
        assert_eq!(ClusterTlsMode::from_str("unknown"), ClusterTlsMode::Shared, "unknown mode defaults to shared — 未知模式默认为 shared");
        assert_eq!(ClusterTlsMode::from_str(""), ClusterTlsMode::Shared, "empty mode defaults to shared — 空模式默认为 shared");
    }

    /// Test effective server name — 测试有效服务器名
    #[test]
    fn test_effective_server_name() {
        let config = ClusterTlsConfig {
            mode: ClusterTlsMode::Shared,
            cert_path: "/tmp/cert.pem".to_string(),
            key_path: "/tmp/key.pem".to_string(),
            ca_cert_path: None,
            server_name: None,
        };
        assert_eq!(config.effective_server_name(), "kong_clustering",
            "default server name should be 'kong_clustering' — 默认服务器名应为 'kong_clustering'");

        let config_with_name = ClusterTlsConfig {
            server_name: Some("my-cluster.example.com".to_string()),
            ..config.clone()
        };
        assert_eq!(config_with_name.effective_server_name(), "my-cluster.example.com");

        let config_empty_name = ClusterTlsConfig {
            server_name: Some("".to_string()),
            ..config
        };
        assert_eq!(config_empty_name.effective_server_name(), "kong_clustering",
            "empty server name should fallback to default — 空服务器名应回退到默认值");
    }

    /// Test from_kong_config fails with missing cert — 测试缺少证书时 from_kong_config 失败
    #[test]
    fn test_tls_config_missing_cert() {
        let mut kong_config = kong_config::KongConfig::default();
        kong_config.cluster_cert = None;
        kong_config.cluster_cert_key = None;

        let result = ClusterTlsConfig::from_kong_config(&kong_config);
        assert!(result.is_err(), "should fail without cert — 无证书时应失败");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cluster_cert"), "error should mention cluster_cert — 错误应提及 cluster_cert");
    }

    /// Test from_kong_config fails with nonexistent cert file — 测试证书文件不存在时失败
    #[test]
    fn test_tls_config_nonexistent_cert_file() {
        let mut kong_config = kong_config::KongConfig::default();
        kong_config.cluster_cert = Some("/nonexistent/path/cert.pem".to_string());
        kong_config.cluster_cert_key = Some("/nonexistent/path/key.pem".to_string());

        let result = ClusterTlsConfig::from_kong_config(&kong_config);
        assert!(result.is_err(), "should fail with nonexistent cert — 证书不存在时应失败");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "error should say not found — 错误应说未找到");
    }

    /// Test from_kong_config succeeds with real temp files — 测试使用真实临时文件时成功
    #[test]
    fn test_tls_config_with_real_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cert_path = tmp.path().join("cert.pem");
        let key_path = tmp.path().join("key.pem");
        std::fs::write(&cert_path, "fake cert").unwrap();
        std::fs::write(&key_path, "fake key").unwrap();

        let mut kong_config = kong_config::KongConfig::default();
        kong_config.cluster_cert = Some(cert_path.to_str().unwrap().to_string());
        kong_config.cluster_cert_key = Some(key_path.to_str().unwrap().to_string());
        kong_config.cluster_mtls = "shared".to_string();

        let result = ClusterTlsConfig::from_kong_config(&kong_config);
        assert!(result.is_ok(), "should succeed with valid files — 使用有效文件应成功");
        let tls = result.unwrap();
        assert_eq!(tls.mode, ClusterTlsMode::Shared);
    }

    /// Test PKI mode requires ca_cert — 测试 PKI 模式需要 ca_cert
    #[test]
    fn test_pki_mode_missing_ca_cert() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cert_path = tmp.path().join("cert.pem");
        let key_path = tmp.path().join("key.pem");
        std::fs::write(&cert_path, "fake cert").unwrap();
        std::fs::write(&key_path, "fake key").unwrap();

        let mut kong_config = kong_config::KongConfig::default();
        kong_config.cluster_cert = Some(cert_path.to_str().unwrap().to_string());
        kong_config.cluster_cert_key = Some(key_path.to_str().unwrap().to_string());
        kong_config.cluster_mtls = "pki".to_string();
        kong_config.cluster_ca_cert = Some("/nonexistent/ca.pem".to_string());

        let result = ClusterTlsConfig::from_kong_config(&kong_config);
        assert!(result.is_err(), "PKI mode with missing CA should fail — PKI 模式缺少 CA 应失败");
    }

    /// Test PKI mode with all files present — 测试 PKI 模式所有文件都存在
    #[test]
    fn test_pki_mode_with_all_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cert_path = tmp.path().join("cert.pem");
        let key_path = tmp.path().join("key.pem");
        let ca_path = tmp.path().join("ca.pem");
        std::fs::write(&cert_path, "fake cert").unwrap();
        std::fs::write(&key_path, "fake key").unwrap();
        std::fs::write(&ca_path, "fake ca").unwrap();

        let mut kong_config = kong_config::KongConfig::default();
        kong_config.cluster_cert = Some(cert_path.to_str().unwrap().to_string());
        kong_config.cluster_cert_key = Some(key_path.to_str().unwrap().to_string());
        kong_config.cluster_mtls = "pki".to_string();
        kong_config.cluster_ca_cert = Some(ca_path.to_str().unwrap().to_string());

        let result = ClusterTlsConfig::from_kong_config(&kong_config);
        assert!(result.is_ok(), "PKI mode with all files should succeed — PKI 模式所有文件存在应成功");
        let tls = result.unwrap();
        assert_eq!(tls.mode, ClusterTlsMode::Pki);
        assert!(tls.ca_cert_path.is_some());
    }
}

// ===== Task 2e: Protocol Parse Tests — 协议解析测试 =====

/// Test V1 payload build + parse roundtrip — 测试 V1 载荷构建+解析往返
#[test]
fn test_v1_payload_build_and_parse() {
    use kong_cluster::cp::calculate_config_hash;

    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/api"]}],
        "services": [{"id": "s1", "host": "backend"}],
        "plugins": [], "upstreams": [], "targets": []
    });
    let hashes = calculate_config_hash(&config);

    let payload_bytes = protocol::build_v1_payload(&config, &hashes.config, &hashes).unwrap();

    let parsed = protocol::parse_v1_payload(&payload_bytes).unwrap();
    assert_eq!(parsed.msg_type, "reconfigure");
    assert_eq!(parsed.config_hash, hashes.config);
    assert_eq!(parsed.config_table["routes"][0]["id"], "r1");
    assert!(parsed.hashes.is_some());
    let h = parsed.hashes.unwrap();
    assert_eq!(h.routes, hashes.routes);
    assert_eq!(h.services, hashes.services);
}

/// Test V2 JSON-RPC encode/decode — 测试 V2 JSON-RPC 编解码
#[test]
fn test_v2_jsonrpc_encode_decode() {
    let req = protocol::build_v2_init_request();
    assert_eq!(req.method, "kong.sync.v1.init");

    let encoded = protocol::encode_v2_message(&req).unwrap();
    let decoded: protocol::JsonRpcRequest = protocol::decode_v2_message(&encoded).unwrap();
    assert_eq!(decoded.jsonrpc, "2.0");
    assert_eq!(decoded.method, "kong.sync.v1.init");
    assert_eq!(decoded.id, 1);
}

/// Test BasicInfo serialization — 测试 BasicInfo 序列化
#[test]
fn test_basic_info_serialization() {
    let info = protocol::BasicInfo::new(vec!["key-auth".to_string(), "rate-limiting".to_string()]);
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["type"], "basic_info");
    assert_eq!(json["plugins"][0], "key-auth");
    assert_eq!(json["plugins"][1], "rate-limiting");
}

/// Test DP ping message contains hash — 测试 DP ping 消息包含哈希
#[tokio::test]
async fn test_dp_ping_message_contains_hash() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dp = DataPlane::new(
        "127.0.0.1:9005",
        tmp.path().to_str().unwrap(),
        vec![],
        uuid::Uuid::new_v4(),
        "test-host".to_string(),
    );

    // Before config applied, ping should contain EMPTY_CONFIG_HASH
    // 配置应用前，ping 应包含 EMPTY_CONFIG_HASH
    let ping = dp.ping_message().await;
    match ping {
        tokio_tungstenite::tungstenite::Message::Ping(data) => {
            let hash = String::from_utf8_lossy(&data).to_string();
            assert_eq!(hash, EMPTY_CONFIG_HASH);
        }
        _ => panic!("Expected Ping message — 期望 Ping 消息"),
    }

    // After config applied, ping should contain new hash
    // 配置应用后，ping 应包含新哈希
    let new_hash = "abcdef1234567890abcdef1234567890";
    dp.mark_config_applied(&serde_json::json!({}), new_hash, ConfigHashes::default()).await;

    let ping = dp.ping_message().await;
    match ping {
        tokio_tungstenite::tungstenite::Message::Ping(data) => {
            let hash = String::from_utf8_lossy(&data).to_string();
            assert_eq!(hash, new_hash);
        }
        _ => panic!("Expected Ping message — 期望 Ping 消息"),
    }
}

/// Test CP with mismatched hash marks DP as Unknown — 测试哈希不匹配时 CP 将 DP 标记为 Unknown
#[tokio::test]
async fn test_cp_mismatched_hash_status() {
    let cp = ControlPlane::new();
    let dp_id = uuid::Uuid::new_v4();

    let dp_info = DataPlaneInfo {
        id: dp_id,
        ip: "127.0.0.1".to_string(),
        hostname: "mismatch-dp".to_string(),
        version: "0.1.0".to_string(),
        sync_status: SyncStatus::Unknown,
        config_hash: EMPTY_CONFIG_HASH.to_string(),
        last_seen: chrono::Utc::now(),
        labels: std::collections::HashMap::new(),
    };
    let _rx = cp.register_dp(dp_info).await;

    // Push config — 推送配置
    cp.push_config(&serde_json::json!({"routes": [{"id": "r1"}]})).await.unwrap();
    let current_hash = cp.current_hash().await;

    // DP reports a WRONG hash — DP 报告错误哈希
    cp.update_dp_status(&dp_id, "ffffffffffffffffffffffffffffffff").await;

    let dps = cp.list_data_planes().await;
    let dp = dps.iter().find(|d| d.id == dp_id).unwrap();
    assert_eq!(dp.sync_status, SyncStatus::Unknown,
        "mismatched hash should leave DP as Unknown — 哈希不匹配应使 DP 保持 Unknown");
    assert_ne!(dp.config_hash, current_hash);
}

// ===== V2 Protocol Tests — V2 协议测试 =====

/// Test V2 init response format — 测试 V2 初始化响应格式
#[test]
fn test_v2_init_response_format() {
    let resp_bytes = protocol::build_v2_init_response(42);
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes)
        .expect("should be valid JSON — 应为有效 JSON");

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 42);
    assert_eq!(resp["result"]["ok"], true);
    assert!(resp.get("error").is_none() || resp["error"].is_null(),
        "error should be absent — error 应为空");
}

/// Test V2 delta response format — 测试 V2 delta 响应格式
#[test]
fn test_v2_delta_response_format() {
    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/foo"]}],
        "services": [{"id": "s1", "host": "example.com"}],
    });
    let resp_bytes = protocol::build_v2_delta_response(7, &config, 3);
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes)
        .expect("should be valid JSON — 应为有效 JSON");

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 7);
    assert_eq!(resp["result"]["version"], 3);
    assert_eq!(resp["result"]["config"]["routes"][0]["id"], "r1");
    assert_eq!(resp["result"]["config"]["services"][0]["host"], "example.com");
}

/// Test V2 notify_new_version format — 测试 V2 新版本通知格式
#[test]
fn test_v2_notify_new_version_format() {
    let notif_bytes = protocol::build_v2_notify_new_version(5);
    let notif: serde_json::Value = serde_json::from_slice(&notif_bytes)
        .expect("should be valid JSON — 应为有效 JSON");

    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], "kong.sync.v1.notify_new_version");
    assert_eq!(notif["params"]["version"], 5);
    // Notification has no id — 通知没有 id
    assert!(notif.get("id").is_none() || notif["id"].is_null(),
        "notification should have no id — 通知不应有 id");
}

/// Test V2 validation error notification format — 测试 V2 验证错误通知格式
#[test]
fn test_v2_notify_validation_error_format() {
    let errors = vec!["field 'name' is required".to_string(), "port out of range".to_string()];
    let notif_bytes = protocol::build_v2_notify_validation_error(&errors);
    let notif: serde_json::Value = serde_json::from_slice(&notif_bytes)
        .expect("should be valid JSON — 应为有效 JSON");

    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], "kong.sync.v1.notify_validation_error");
    let err_arr = notif["params"]["errors"].as_array()
        .expect("errors should be array — errors 应为数组");
    assert_eq!(err_arr.len(), 2);
    assert_eq!(err_arr[0], "field 'name' is required");
    assert_eq!(err_arr[1], "port out of range");
}

/// Test V2 method constants match expected values — 测试 V2 方法常量匹配预期值
#[test]
fn test_v2_method_constants() {
    assert_eq!(protocol::V2_METHOD_INIT, "kong.sync.v1.init");
    assert_eq!(protocol::V2_METHOD_GET_DELTA, "kong.sync.v1.get_delta");
    assert_eq!(protocol::V2_METHOD_NOTIFY_NEW_VERSION, "kong.sync.v1.notify_new_version");
    assert_eq!(protocol::V2_METHOD_NOTIFY_VALIDATION_ERROR, "kong.sync.v1.notify_validation_error");
}

/// Test CP current_payload stores and retrieves correctly — 测试 CP current_payload 正确存储和获取
#[tokio::test]
async fn test_cp_current_payload() {
    let cp = ControlPlane::new();

    // Before push, payload should be None — 推送前 payload 应为 None
    assert!(cp.current_payload().await.is_none(),
        "payload should be None before push — 推送前 payload 应为 None");

    // Push config — 推送配置
    let config = serde_json::json!({
        "routes": [{"id": "r1"}],
        "services": [],
        "plugins": [],
        "upstreams": [],
        "targets": []
    });
    cp.push_config(&config).await.unwrap();

    // After push, payload should be Some — 推送后 payload 应为 Some
    let payload = cp.current_payload().await;
    assert!(payload.is_some(), "payload should be Some after push — 推送后 payload 应为 Some");

    // Payload should decompress to valid V1 config — 负载应能解压为有效 V1 配置
    let data = payload.unwrap();
    let parsed = protocol::parse_v1_payload(&data)
        .expect("should parse V1 payload — 应能解析 V1 负载");
    assert_eq!(parsed.config_table["routes"][0]["id"], "r1");
}

/// Test V2 full flow: init → get_delta → notify_new_version via CP/WS
/// 测试 V2 完整流程：init → get_delta → notify_new_version 通过 CP/WS
#[tokio::test]
async fn test_v2_endpoint_full_flow() {
    use axum::extract::ws::Message;

    let cp = Arc::new(ControlPlane::new());

    // Push initial config so get_delta has data — 推送初始配置以便 get_delta 有数据
    let config = serde_json::json!({
        "routes": [{"id": "r1", "paths": ["/v2-test"]}],
        "services": [{"id": "s1", "host": "v2.example.com"}],
        "plugins": [],
        "upstreams": [],
        "targets": []
    });
    cp.push_config(&config).await.unwrap();

    // Build axum app with V2 endpoint — 构建包含 V2 端点的 axum 应用
    let cp_clone = Arc::clone(&cp);
    let app = axum::Router::new()
        .route("/v2/outlet", axum::routing::get({
            let cp = Arc::clone(&cp_clone);
            move |ws: axum::extract::WebSocketUpgrade,
                  query: axum::extract::Query<std::collections::HashMap<String, String>>| {
                let cp = Arc::clone(&cp);
                async move {
                    ws.on_upgrade(move |socket| async move {
                        use futures_util::{SinkExt, StreamExt};
                        use kong_cluster::protocol::{
                            self as proto, JsonRpcRequest, V2_METHOD_INIT, V2_METHOD_GET_DELTA,
                            build_v2_init_response, build_v2_delta_response, build_v2_notify_new_version,
                        };

                        let dp_id = query.0.get("node_id")
                            .and_then(|s| uuid::Uuid::parse_str(s).ok())
                            .unwrap_or_else(uuid::Uuid::new_v4);

                        let dp_info = kong_cluster::DataPlaneInfo {
                            id: dp_id,
                            ip: String::new(),
                            hostname: query.0.get("node_hostname").cloned().unwrap_or_default(),
                            version: query.0.get("node_version").cloned().unwrap_or_default(),
                            sync_status: kong_cluster::SyncStatus::Unknown,
                            config_hash: kong_cluster::EMPTY_CONFIG_HASH.to_string(),
                            last_seen: chrono::Utc::now(),
                            labels: std::collections::HashMap::new(),
                        };

                        let _rx = cp.register_dp(dp_info).await;
                        let mut config_rx = cp.subscribe_config();
                        let mut config_version: u64 = 0;
                        let (mut ws_sender, mut ws_receiver) = socket.split();

                        loop {
                            tokio::select! {
                                msg = ws_receiver.next() => {
                                    match msg {
                                        Some(Ok(Message::Text(text))) => {
                                            if let Ok(req) = serde_json::from_str::<JsonRpcRequest>(&text) {
                                                match req.method.as_str() {
                                                    V2_METHOD_INIT => {
                                                        let resp = build_v2_init_response(req.id);
                                                        let _ = ws_sender.send(Message::Text(
                                                            String::from_utf8_lossy(&resp).into_owned().into()
                                                        )).await;
                                                    }
                                                    V2_METHOD_GET_DELTA => {
                                                        if let Some(payload) = cp.current_payload().await {
                                                            if let Ok(parsed) = proto::parse_v1_payload(&payload) {
                                                                config_version += 1;
                                                                let resp = build_v2_delta_response(
                                                                    req.id, &parsed.config_table, config_version,
                                                                );
                                                                let _ = ws_sender.send(Message::Text(
                                                                    String::from_utf8_lossy(&resp).into_owned().into()
                                                                )).await;
                                                            }
                                                        }
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        Some(Ok(Message::Close(_))) | None => break,
                                        _ => {}
                                    }
                                }
                                Ok(_) = config_rx.recv() => {
                                    config_version += 1;
                                    let notif = build_v2_notify_new_version(config_version);
                                    if ws_sender.send(Message::Text(
                                        String::from_utf8_lossy(&notif).into_owned().into()
                                    )).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }

                        cp.unregister_dp(&dp_id).await;
                    })
                }
            }
        }));

    // Bind on random port — 绑定随机端口
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Spawn the server — 启动服务
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start — 等待服务启动
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect as V2 DP via tokio-tungstenite — 作为 V2 DP 通过 tokio-tungstenite 连接
    let url = format!(
        "ws://{}/v2/outlet?node_id={}&node_hostname=test-dp&node_version=0.1.0",
        addr, uuid::Uuid::new_v4()
    );
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url).await
        .expect("WS connect failed — WS 连接失败");

    // Step 1: Send init request — 步骤 1：发送 init 请求
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "kong.sync.v1.init",
        "params": {"x-snappy-framed": true, "services": ["kong.sync.v1"]},
        "id": 1
    });
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&init_req).unwrap().into()
    )).await.unwrap();

    // Receive init response — 接收 init 响应
    let resp_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await.expect("timeout waiting for init response — 等待 init 响应超时")
        .expect("stream ended — 流已结束")
        .expect("ws error — ws 错误");

    let resp_text = match resp_msg {
        tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
        other => panic!("expected text message, got {:?} — 期望文本消息，得到 {:?}", other, other),
    };
    let init_resp: serde_json::Value = serde_json::from_str(&resp_text).unwrap();
    assert_eq!(init_resp["jsonrpc"], "2.0");
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["ok"], true);

    // Step 2: Send get_delta request — 步骤 2：发送 get_delta 请求
    let delta_req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "kong.sync.v1.get_delta",
        "params": {"version": 0},
        "id": 2
    });
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&delta_req).unwrap().into()
    )).await.unwrap();

    // Receive delta response — 接收 delta 响应
    let resp_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await.expect("timeout waiting for delta response — 等待 delta 响应超时")
        .expect("stream ended — 流已结束")
        .expect("ws error — ws 错误");

    let resp_text = match resp_msg {
        tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
        other => panic!("expected text message, got {:?} — 期望文本消息，得到 {:?}", other, other),
    };
    let delta_resp: serde_json::Value = serde_json::from_str(&resp_text).unwrap();
    assert_eq!(delta_resp["jsonrpc"], "2.0");
    assert_eq!(delta_resp["id"], 2);
    assert_eq!(delta_resp["result"]["version"], 1);
    assert_eq!(delta_resp["result"]["config"]["routes"][0]["id"], "r1");

    // Step 3: Push new config and expect notify_new_version — 步骤 3：推送新配置并期望收到新版本通知
    let new_config = serde_json::json!({
        "routes": [{"id": "r2", "paths": ["/v2-updated"]}],
        "services": [],
        "plugins": [],
        "upstreams": [],
        "targets": []
    });
    cp.push_config(&new_config).await.unwrap();

    // Receive notification — 接收通知
    let notif_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await.expect("timeout waiting for notification — 等待通知超时")
        .expect("stream ended — 流已结束")
        .expect("ws error — ws 错误");

    let notif_text = match notif_msg {
        tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
        other => panic!("expected text message, got {:?} — 期望文本消息，得到 {:?}", other, other),
    };
    let notif: serde_json::Value = serde_json::from_str(&notif_text).unwrap();
    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], "kong.sync.v1.notify_new_version");
    assert!(notif["params"]["version"].as_u64().unwrap() > 0,
        "version should be positive — version 应为正数");

    // Close connection — 关闭连接
    ws.close(None).await.ok();
}
