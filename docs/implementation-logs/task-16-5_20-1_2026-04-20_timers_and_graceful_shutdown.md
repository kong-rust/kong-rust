# Task 16.5 + 20.1 — Timers endpoint + Graceful Shutdown

**Date**: 2026-04-20
**Tasks**: 16.5 `/timers` 端点、20.1 优雅关闭

## 背景

两个任务合并为一个日志：16.5 是纯 Admin API 端点（机械劳动），20.1 虽然涉及生命周期但 Pingora 已有现成 `ShutdownWatch` 和信号处理基础设施，实际 delta 只有配置和 `ServerConf` 构造。两者都属于"小而完整"的收尾型任务，合并记录节省工程上下文。

## 16.5 — `/timers` 端点

### Kong 官方响应形态

读取 `kong/api/routes/kong.lua` 得到：

```json
{
  "worker": { "id": 0, "count": 1 },
  "stats": "<kong.timer:stats({verbose=true, flamegraph=true})>"
}
```

`kong.timer:stats()` 来自 `resty-timer-ng` Lua 库，Rust + tokio 无对等物。返回结构大致为 `sys / timers / flamegraph` 三段。

### 实现策略

选择**结构 100% 兼容 + 值零占位**：

```json
{
  "worker": { "id": 0, "count": 1 },
  "stats": {
    "sys":        { "total": 0, "runs": 0, "running": 0, "pending": 0, "waiting": 0 },
    "timers":     {},
    "flamegraph": { "running": "", "pending": "", "elapsed_time": "" }
  }
}
```

Kong Manager 等消费方能解析结构即可，不依赖数值。后续可对接 `tokio::runtime::Handle::metrics()` 填真实值（`num_alive_tasks` → running、`num_workers_threads` → 单线程池计数）。

### 修改文件

- 新建 `crates/kong-admin/src/handlers/timers.rs`（~55 行）
- `crates/kong-admin/src/handlers/mod.rs`：`pub mod timers;`
- `crates/kong-admin/src/lib.rs`：注册 `/timers` 路由，`is_known_route` 补入 `/timers`
- `crates/kong-admin/tests/admin_api_compat.rs`：`test_timers_endpoint_returns_kong_shape`，验证全部 Kong schema 字段存在

## 20.1 — Graceful Shutdown

### 观察：Pingora 基础设施已到位

- `pingora::server::Server::run_forever()` 内部处理 SIGINT/SIGTERM
- 收到信号后通过 `watch::channel<bool>` 广播 shutdown 到所有注册的 background service
- 本项目内的 Admin / CP `ClusterListenerTask` / DP `DpConnectorTask` 均已实现 `shutdown.changed()` 分支 —— 来自阶段 9 Hybrid 改造时的副产品
- 缺失点：从未告诉 Pingora "多少秒内必须完成"，即 `ServerConf.graceful_shutdown_timeout_seconds` 长期为 `None`

### 实现 delta

1. `KongConfig` 新增字段 `nginx_main_worker_shutdown_timeout: u64`（Kong 同名参数，默认 10s）
   - 解析器支持 `10` 或 `10s` 两种写法（Kong 原版接受带单位）
   - 非法值回落默认 10
2. `init_proxy_and_admin` → `start_gateway` 里用 `Server::new_with_opt_and_conf` 替代 `Server::new(None)`：
   ```rust
   let mut server_conf = ServerConf::default();
   server_conf.grace_period_seconds = Some(0);  // 立即停止 accept 新连接
   server_conf.graceful_shutdown_timeout_seconds =
       Some(config.nginx_main_worker_shutdown_timeout);
   let mut server = Server::new_with_opt_and_conf(None, server_conf);
   ```
3. 映射逻辑：
   - `grace_period_seconds = 0` — 收到 SIGTERM 后立刻停止接受新连接，不再"软着陆"
   - `graceful_shutdown_timeout_seconds = N` — 存量请求的最长完成时间；超时 Pingora 强制 drop

### 修改文件

- `crates/kong-config/src/config.rs`
  - struct 字段 + Default + parser match
  - 新增单元测试 `test_worker_shutdown_timeout_parsing`
- `crates/kong-server/src/main.rs`
  - `Server::new_with_opt_and_conf` 替换

## 测试结果

```
cargo test -p kong-config --lib                   → 26 passed (+1)
cargo test -p kong-admin --tests                  → 29 passed (+1 timers) + 1 ai_proxy_schema
cargo check --workspace --tests                    → Finished, 0 errors
```

## 代码统计

- 新建文件 1 个：handlers/timers.rs（~55 行）
- 修改文件 4 个：config.rs、server/main.rs、handlers/mod.rs、kong-admin/src/lib.rs
- 新增测试 2 个（1 集成 + 1 单元）
- 净增 ~100 行业务 / ~40 行测试

## 后续遗留 / 改进空间

1. **/timers 真实数据**：接 `tokio::runtime::Handle::metrics()` 填充 `sys.running` 等字段（需要 tokio 1.46+ 稳定 API）
2. **graceful shutdown observability**：当前没有 shutdown 进度日志，可在 Pingora `ShutdownWatch` 传播路径上加 `tracing::info!("draining: %d active requests")`
3. **Admin API 的 in-flight drain**：Admin axum 服务目前是 `select!` 立即退出，未对未完成请求 await；对管控面通常无害，但可后续加 `axum::serve::with_graceful_shutdown`
