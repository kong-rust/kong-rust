# Task 16.3 + 16.4 — Cache management + runtime log level

**Date**: 2026-04-19
**Tasks**: 16.3 `/cache` 管理端点、16.4 `/debug/node/log-level` 动态日志级别

## 背景

2026-04-19 审计发现原阶段 6.3 虚报包含 `/cache` 和 `/debug/node/log-level` 端点（代码实际缺失）。审计修正后将这两个能力下放到阶段 16.3 / 16.4 作为正式新任务。本次一次性交付两者，因为二者都需要改 `AdminState` 结构并修改 `server/main.rs` 的初始化链，打包做可复用上下文。

## 设计决策

### 共享 `KongCache` 实例

- `kong-db::KongCache` 原本是孤岛（声明在 lib.rs re-export，但 DAO 层并未接入）。本次由 `server/main.rs` 的 `init_proxy_and_admin` 统一实例化一个 `Arc<KongCache>`，通过 `AdminState.cache` 暴露。
- 容量来自 `KongConfig::mem_cache_size_bytes()`（默认 10MB → 10000 条目），TTL 来自 `db_cache_ttl` / `db_cache_neg_ttl`。
- 当前 DAO / 插件尚未写入此缓存，但 Admin API 已就绪；后续 key-auth、jwt 等插件接入时可直接被 `/cache/{key}` 查询，无需再改 Admin。

### `LogLevelUpdater` 类型擦除闭包

- `tracing_subscriber::reload::Handle<EnvFilter, S>` 的 `S` 是 subscriber 具体类型（含所有 Layer），暴露到 `kong-admin` 会造成强耦合。
- 在 `kong-admin::lib.rs` 定义 `pub type LogLevelUpdater = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>`，由 `kong-server` 在 `init_logging` 里把 reload handle 装进闭包。`kong-admin` 仅依赖闭包签名，无需引入 tracing-subscriber。
- 测试路径下 `log_updater: None` → `PUT /debug/node/log-level/{level}` 返回 `503 Service Unavailable`，明确失败语义。

### Kong 兼容行为

- `/cache/{key}` miss → `404 { "message": "Not found" }`（Kong 语义）
- `DELETE /cache/{key}` → 始终 `204`，幂等
- `DELETE /cache` → 清空（`KongCache::purge` + `run_pending_tasks`）
- `PUT /debug/node/log-level/{level}` 合法级别：`debug/info/notice/warn/error/crit/alert/emerg`（与 `kong_log_level_to_filter` 映射一致）

## 修改文件

### 新建

- `crates/kong-admin/src/handlers/cache.rs` — 3 个 handler（get/delete/purge）
- `crates/kong-admin/src/handlers/debug.rs` — 2 个 handler（get/set log level）

### 修改

- `crates/kong-admin/src/lib.rs`
  - `use kong_db::KongCache;`
  - 新增 `pub type LogLevelUpdater = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;`
  - `AdminState` 新增 3 个字段：`cache: Arc<KongCache>`、`log_updater: Option<LogLevelUpdater>`、`current_log_level: Arc<RwLock<String>>`
  - `build_admin_router`：注册 `/cache`、`/cache/{key}`、`/debug/node/log-level`、`/debug/node/log-level/{level}`
  - `is_known_route`：补入 `/cache`、`/debug/node/log-level` 静态列表；动态匹配新增 `cache` entity 和 `["debug", "node", "log-level", _]`
- `crates/kong-admin/src/handlers/mod.rs`：`pub mod cache;`、`pub mod debug;`
- `crates/kong-server/src/main.rs`
  - `init_logging` 返回 `(LogLevelUpdater, Arc<RwLock<String>>)`，用 `reload::Layer::new(env_filter)` 包裹
  - `start_gateway` + `init_proxy_and_admin` 签名新增 log_updater / current_log_level 参数
  - `init_proxy_and_admin` 实例化 `Arc<KongCache>`，两处 AdminState 构造（db-less / Postgres）同步填充 3 个新字段
- `crates/kong-admin/tests/admin_api_compat.rs`
  - 4 处已有 `create_test_app`/`create_test_status_app` 同步补 3 个字段（`cache` 从 config 构造，`log_updater: None`）
  - 新增 `create_test_app_with_cache()` helper，返回 `(Router, Arc<KongCache>)`
  - 新增 7 个 `#[tokio::test]`：
    - `test_cache_get_miss_returns_404`
    - `test_cache_get_hit_returns_value`
    - `test_cache_delete_entry`
    - `test_cache_purge_all`
    - `test_log_level_get_returns_current`
    - `test_log_level_put_rejects_unknown_level`
    - `test_log_level_put_without_updater_returns_503`
- `crates/kong-admin/tests/ai_proxy_schema.rs`：AdminState 构造补 3 个字段

## 测试结果

```
cargo test -p kong-admin --test admin_api_compat
  → 28 passed; 0 failed  (原 21 + 新增 7)
cargo check --workspace --tests
  → 通过（warnings 均为无关历史遗留）
```

## 代码统计

- 新建文件 2 个：cache.rs (~55 行)、debug.rs (~75 行)
- 修改文件 5 个
- 新增集成测试 7 个
- 净增加约 280 行业务代码 + 180 行测试代码

## 后续遗留

1. `KongCache` 目前没有 DAO / 插件写入方，纯 Admin API 可读可删；需要后续 key-auth / jwt-auth 等插件接入时通过 PDK `kong.cache` 真正产生条目。
2. `current_log_level` 的 Kong 风格字符串与 tracing `EnvFilter` 之间是单向映射（写入 → reload）；从 EnvFilter 反查回 Kong 级别未实现（当前 GET 直接读共享字符串，足够 Kong 兼容）。
3. 运行时改级别不会同步影响 Lua 插件内部的 `ngx.log` 级别（它是一套独立机制，可在将来统一）。
