//! Virtual Key 生命周期预算与强一致账务。

pub mod admin;
pub mod governance;
pub mod metrics;
pub mod model;
pub mod postgres;
pub mod store;

pub use admin::*;
pub use governance::*;
pub use metrics::*;
pub use model::*;
pub use postgres::{PgBudgetCheckpointBatch, PgBudgetStore, PgBudgetStoreConfig};
pub use store::BudgetStore;

#[cfg(test)]
pub(crate) fn postgres_test_url() -> Option<String> {
    if let Ok(url) = std::env::var("KONG_AI_BUDGET_PG_TEST_URL") {
        return Some(url);
    }

    let host = std::env::var("KONG_PG_HOST").ok()?;
    let port = std::env::var("KONG_PG_PORT")
        .ok()?
        .parse::<u16>()
        .expect("KONG_PG_PORT 必须是有效端口");
    let user = std::env::var("KONG_PG_USER").unwrap_or_else(|_| "kong".to_string());
    let password = std::env::var("KONG_PG_PASSWORD").unwrap_or_default();
    let database = std::env::var("KONG_PG_DATABASE").unwrap_or_else(|_| "kong".to_string());

    let mut url = url::Url::parse("postgresql://localhost").expect("固定 PostgreSQL URL 有效");
    url.set_host(Some(&host)).expect("KONG_PG_HOST 必须有效");
    url.set_port(Some(port)).expect("KONG_PG_PORT 必须有效");
    url.set_username(&user).expect("KONG_PG_USER 必须有效");
    url.set_password((!password.is_empty()).then_some(password.as_str()))
        .expect("KONG_PG_PASSWORD 必须有效");
    url.set_path(&database);
    Some(url.to_string())
}
