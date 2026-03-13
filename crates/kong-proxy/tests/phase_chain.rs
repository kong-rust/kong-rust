//! Phase chain tests — 处理链路阶段测试
//!
//! Verifies correct execution of the Kong phase chain: — 验证 Kong 阶段链的正确执行：
//! - Phase execution order: rewrite → access → header_filter → body_filter → log — 阶段执行顺序：rewrite → access → header_filter → body_filter → log
//! - Short-circuit behavior: header_filter skipped after access short-circuit, but log always executes — 短路行为：access 短路后 header_filter 不执行，但 log 始终执行
//! - Plugins execute in priority order — 插件按优先级排序执行
//! - ctx.shared is passed between phases — ctx.shared 在阶段间传递

mod helpers;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kong_core::traits::RequestCtx;
use kong_proxy::phases::PhaseRunner;

use helpers::{make_resolved_plugin, TestPlugin};

#[tokio::test]
async fn test_rewrite_phase_called() {
    let plugin = TestPlugin::new("test-rewrite", 1000);
    let plugin_arc = Arc::new(plugin.clone());
    let resolved = vec![make_resolved_plugin(plugin_arc)];

    let mut ctx = RequestCtx::new();
    let result = PhaseRunner::run_rewrite(&resolved, &mut ctx).await;

    assert!(result.is_ok());
    assert!(plugin.rewrite_called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_access_phase_called() {
    let plugin = TestPlugin::new("test-access", 1000);
    let plugin_arc = Arc::new(plugin.clone());
    let resolved = vec![make_resolved_plugin(plugin_arc)];

    let mut ctx = RequestCtx::new();
    let result = PhaseRunner::run_access(&resolved, &mut ctx).await;

    assert!(result.is_ok());
    assert!(plugin.access_called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_access_short_circuit_skips_header_filter() {
    // Register a plugin that short-circuits in access phase — 注册一个在 access 阶段短路的插件
    let access_plugin = TestPlugin::with_short_circuit("auth-blocker", 1000, 403);
    let access_arc = Arc::new(access_plugin.clone());

    // Register a header_filter plugin (should not be called) — 注册一个 header_filter 插件（应该不被调用）
    let hf_plugin = TestPlugin::new("header-modifier", 800);
    let hf_arc = Arc::new(hf_plugin.clone());

    let resolved = vec![
        make_resolved_plugin(access_arc),
        make_resolved_plugin(hf_arc),
    ];

    let mut ctx = RequestCtx::new();

    // Execute access phase — 执行 access 阶段
    let _ = PhaseRunner::run_access(&resolved, &mut ctx).await;
    assert!(ctx.is_short_circuited());
    assert_eq!(ctx.exit_status, Some(403));

    // Execute header_filter — should be skipped due to short-circuit — 执行 header_filter — 应该因为短路而跳过
    let _ = PhaseRunner::run_header_filter(&resolved, &mut ctx).await;
    assert!(
        !hf_plugin.header_filter_called.load(Ordering::SeqCst),
        "短路后 header_filter 不应被调用"
    );
}

#[tokio::test]
async fn test_log_phase_always_executes_after_short_circuit() {
    let access_plugin = TestPlugin::with_short_circuit("blocker", 1000, 403);
    let access_arc = Arc::new(access_plugin.clone());

    let log_plugin = TestPlugin::new("logger", 800);
    let log_arc = Arc::new(log_plugin.clone());

    let resolved = vec![
        make_resolved_plugin(access_arc),
        make_resolved_plugin(log_arc),
    ];

    let mut ctx = RequestCtx::new();

    // Access short-circuit — access 短路
    let _ = PhaseRunner::run_access(&resolved, &mut ctx).await;
    assert!(ctx.is_short_circuited());

    // Log phase should still execute — log 阶段仍应执行
    let _ = PhaseRunner::run_log(&resolved, &mut ctx).await;
    assert!(
        log_plugin.log_called.load(Ordering::SeqCst),
        "Log 阶段应始终执行，即使之前短路"
    );
}

#[tokio::test]
async fn test_plugins_execute_in_priority_order() {
    // Higher priority plugins execute first — 高优先级插件先执行
    let high = TestPlugin::new("high-priority", 2000);
    let mut high_p = high.clone();
    high_p.set_shared_in_rewrite = Some((
        "order".to_string(),
        serde_json::Value::String("high".to_string()),
    ));

    let low = TestPlugin::new("low-priority", 100);
    let mut low_p = low.clone();
    low_p.set_shared_in_rewrite = Some((
        "order".to_string(),
        serde_json::Value::String("low".to_string()),
    ));

    // Plugins ordered by descending priority (higher priority executes first) — 插件按优先级降序排列（高优先级先执行）
    let resolved = vec![
        make_resolved_plugin(Arc::new(high_p)),
        make_resolved_plugin(Arc::new(low_p)),
    ];

    let mut ctx = RequestCtx::new();
    let _ = PhaseRunner::run_rewrite(&resolved, &mut ctx).await;

    // Lower priority plugin executes later, overwriting the shared value — 低优先级插件后执行，覆盖了 shared 值
    assert_eq!(
        ctx.shared.get("order").unwrap().as_str().unwrap(),
        "low",
        "低优先级插件后执行，应覆盖高优先级的值"
    );
}

#[tokio::test]
async fn test_ctx_shared_passes_between_phases() {
    let mut rewrite_plugin = TestPlugin::new("shared-writer", 1000);
    rewrite_plugin.set_shared_in_rewrite =
        Some(("auth_passed".to_string(), serde_json::Value::Bool(true)));

    let access_plugin = TestPlugin::new("shared-reader", 900);
    let access_arc = Arc::new(access_plugin.clone());

    let resolved = vec![
        make_resolved_plugin(Arc::new(rewrite_plugin)),
        make_resolved_plugin(access_arc),
    ];

    let mut ctx = RequestCtx::new();

    // Rewrite phase writes to shared — rewrite 阶段写入 shared
    let _ = PhaseRunner::run_rewrite(&resolved, &mut ctx).await;
    assert_eq!(
        ctx.shared.get("auth_passed").unwrap(),
        &serde_json::Value::Bool(true)
    );

    // Access phase can read data written by rewrite — access 阶段可以读到 rewrite 写入的数据
    let _ = PhaseRunner::run_access(&resolved, &mut ctx).await;
    assert!(access_plugin.access_called.load(Ordering::SeqCst));
    // Shared data persists throughout the entire request lifecycle — shared 数据在整个请求生命周期内持续存在
    assert!(ctx.shared.contains_key("auth_passed"));
}

#[tokio::test]
async fn test_header_filter_modifies_response_headers() {
    let plugin = TestPlugin::with_header_modify("cors", 1000, "x-custom-header", "hello-world");
    let plugin_arc = Arc::new(plugin.clone());
    let resolved = vec![make_resolved_plugin(plugin_arc)];

    let mut ctx = RequestCtx::new();
    let _ = PhaseRunner::run_header_filter(&resolved, &mut ctx).await;

    assert!(plugin.header_filter_called.load(Ordering::SeqCst));
    assert_eq!(ctx.response_headers_to_set.len(), 1);
    assert_eq!(ctx.response_headers_to_set[0].0, "x-custom-header");
    assert_eq!(ctx.response_headers_to_set[0].1, "hello-world");
}

#[tokio::test]
async fn test_body_filter_called_with_data() {
    let plugin = TestPlugin::new("body-modifier", 1000);
    let plugin_arc = Arc::new(plugin.clone());
    let resolved = vec![make_resolved_plugin(plugin_arc)];

    let mut ctx = RequestCtx::new();
    let mut body = bytes::Bytes::from("hello world");
    let result = PhaseRunner::run_body_filter(&resolved, &mut ctx, &mut body, false).await;

    assert!(result.is_ok());
    assert!(plugin.body_filter_called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_empty_plugin_chain() {
    let resolved: Vec<kong_plugin_system::ResolvedPlugin> = vec![];
    let mut ctx = RequestCtx::new();

    // Empty plugin chain should not error — 空插件链不应出错
    assert!(PhaseRunner::run_rewrite(&resolved, &mut ctx).await.is_ok());
    assert!(PhaseRunner::run_access(&resolved, &mut ctx).await.is_ok());
    assert!(PhaseRunner::run_header_filter(&resolved, &mut ctx)
        .await
        .is_ok());

    let mut body = bytes::Bytes::from("test");
    assert!(
        PhaseRunner::run_body_filter(&resolved, &mut ctx, &mut body, true)
            .await
            .is_ok()
    );

    assert!(PhaseRunner::run_log(&resolved, &mut ctx).await.is_ok());
}

#[tokio::test]
async fn test_full_phase_chain() {
    // Simulate full Kong processing chain — 模拟完整的 Kong 处理链路
    let plugin = TestPlugin::new("full-chain-test", 1000);
    let plugin_arc = Arc::new(plugin.clone());
    let resolved = vec![make_resolved_plugin(plugin_arc)];

    let mut ctx = RequestCtx::new();

    // rewrite
    let _ = PhaseRunner::run_rewrite(&resolved, &mut ctx).await;
    assert!(plugin.rewrite_called.load(Ordering::SeqCst));

    // access
    let _ = PhaseRunner::run_access(&resolved, &mut ctx).await;
    assert!(plugin.access_called.load(Ordering::SeqCst));
    assert!(!ctx.is_short_circuited());

    // header_filter
    let _ = PhaseRunner::run_header_filter(&resolved, &mut ctx).await;
    assert!(plugin.header_filter_called.load(Ordering::SeqCst));

    // body_filter
    let mut body = bytes::Bytes::from("response body");
    let _ = PhaseRunner::run_body_filter(&resolved, &mut ctx, &mut body, true).await;
    assert!(plugin.body_filter_called.load(Ordering::SeqCst));

    // log
    let _ = PhaseRunner::run_log(&resolved, &mut ctx).await;
    assert!(plugin.log_called.load(Ordering::SeqCst));

    // Verify all phases were called — 验证所有阶段都被调用了
    assert_eq!(plugin.call_count.load(Ordering::SeqCst), 5);
}
