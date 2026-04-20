//! Timers endpoint — Kong 兼容的计时器统计端点
//!
//! Kong OSS exposes `GET /timers` backed by `resty-timer-ng`. — Kong OSS 通过 `resty-timer-ng` 暴露 `GET /timers`。
//! kong-rust runs on Pingora + tokio and has no equivalent timer manager, so this — kong-rust 基于 Pingora + tokio，没有对应的 timer 管理器，
//! endpoint returns a Kong-shaped response with zeroed counters. — 因此本端点返回 Kong 形态的零值统计结构。
//!
//! Response shape matches `kong/api/routes/kong.lua`:
//! ```json
//! {
//!   "worker": { "id": 0, "count": 1 },
//!   "stats": {
//!     "sys":        { "total": 0, "runs": 0, "running": 0, "pending": 0, "waiting": 0 },
//!     "timers":     {},
//!     "flamegraph": { "running": "", "pending": "", "elapsed_time": "" }
//!   }
//! }
//! ```

use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

/// `GET /timers` — return Kong-shaped timer statistics.
/// `GET /timers` — 返回 Kong 形态的计时器统计。
pub async fn get_timers() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "worker": {
                "id": 0,
                "count": 1
            },
            "stats": {
                "sys": {
                    "total": 0,
                    "runs": 0,
                    "running": 0,
                    "pending": 0,
                    "waiting": 0
                },
                "timers": {},
                "flamegraph": {
                    "running": "",
                    "pending": "",
                    "elapsed_time": ""
                }
            }
        })),
    )
}
