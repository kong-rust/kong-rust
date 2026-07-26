//! AI Virtual Key Admin API handlers — AI Virtual Key 管理 API 处理器

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use num_bigint::BigInt;
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use kong_ai::budget::{
    BudgetAdminScope, BudgetErrorKind, BudgetLedgerCursor, BudgetLedgerEntry, BudgetLedgerQuery,
    BudgetLedgerStatus, BudgetLimitMutation, BudgetOptionalMutation,
    BudgetReconciliationResolution, BudgetStoreError, CreateBudgetAccount, DeleteBudgetAccount,
    RebuildBudgetAccount, ReconcileBudgetIntent, UpdateBudgetAccount,
};
use kong_ai::models::{parse_budget_amount, AiVirtualKey};
use kong_ai::usage::model::decimal_12;
use kong_core::traits::{Entity, PrimaryKey};

use super::{do_get, do_list, do_update, ListParams};
use crate::extractors::FlexibleBody;
use crate::AdminState;

const MAX_VIRTUAL_KEY_LIMIT: u64 = i32::MAX as u64;
const JSON_SAFE_INTEGER_MAX: i128 = 9_007_199_254_740_991;

const SERVER_OWNED_FIELDS: &[&str] = &[
    "budget_used",
    "budget_used_decimal",
    "budget_pending_count",
    "budget_unresolved_count",
    "pending_intent_count",
    "unresolved_intent_count",
    "budget_accounting_revision",
    "budget_checkpoint_tail_events",
    "budget_state_updated_at",
    "budget_accounting_state",
    "key_hash",
    "key_prefix",
    "ws_id",
];

type MutationError = (StatusCode, Json<Value>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationKind {
    Create,
    Patch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OptionalBudgetInput {
    Missing,
    Null,
    Value(Decimal),
}

#[derive(Debug, Clone, Copy)]
struct ProjectionCapability {
    quota: &'static str,
    budget: &'static str,
    quota_backend: Option<&'static str>,
    budget_backend: Option<&'static str>,
}

impl ProjectionCapability {
    fn from_state(state: &AdminState) -> Self {
        use kong_ai::enforcement::{BudgetCapability, QuotaCapability};

        let (quota, quota_backend) = match state.ai_enforcement.capability.quota {
            QuotaCapability::LocalMemory => ("local_memory", Some("memory")),
            QuotaCapability::LocalMemoryEphemeral => ("local_memory_ephemeral", Some("memory")),
            QuotaCapability::UnsupportedHybrid => ("unsupported", None),
        };
        let (budget, budget_backend) = match state.ai_enforcement.effective_budget_capability() {
            BudgetCapability::PostgresAuthoritative => ("postgres_authoritative", Some("postgres")),
            BudgetCapability::AccountingUnavailable => ("accounting_unavailable", Some("postgres")),
            BudgetCapability::UnsupportedDbLess | BudgetCapability::UnsupportedHybrid => {
                ("unsupported", None)
            }
        };
        Self {
            quota,
            budget,
            quota_backend,
            budget_backend,
        }
    }
}

fn schema_violation(field: &str, message: impl Into<String>) -> MutationError {
    let message = message.into();
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "message": format!("schema violation ({}: {})", field, message),
            "name": "schema violation",
            "code": 2,
            "fields": {field.to_string(): message},
        })),
    )
}

fn budget_admin_unavailable() -> MutationError {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "message": "Budget accounting is not supported in this deployment mode.",
            "name": "not implemented",
            "code": "budget_accounting_unsupported",
        })),
    )
}

fn budget_store_error(error: BudgetStoreError) -> MutationError {
    let (status, name, code) = match error.kind() {
        BudgetErrorKind::NotFound => (StatusCode::NOT_FOUND, "not found", "not_found"),
        BudgetErrorKind::ReconciliationRequired => (
            StatusCode::CONFLICT,
            "conflict",
            "budget_reconciliation_required",
        ),
        BudgetErrorKind::IntentActive => (StatusCode::CONFLICT, "conflict", "budget_intent_active"),
        BudgetErrorKind::Conflict => (StatusCode::CONFLICT, "conflict", "idempotency_conflict"),
        BudgetErrorKind::AlreadyReconciled => (
            StatusCode::CONFLICT,
            "conflict",
            "budget_intent_already_reconciled",
        ),
        BudgetErrorKind::AccountBusy => (StatusCode::CONFLICT, "conflict", "budget_account_busy"),
        BudgetErrorKind::Unsupported => (
            StatusCode::NOT_IMPLEMENTED,
            "not implemented",
            "budget_accounting_unsupported",
        ),
        BudgetErrorKind::AccountingUnavailable | BudgetErrorKind::OutcomeUnknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            "service unavailable",
            "budget_accounting_unavailable",
        ),
        BudgetErrorKind::AccountingUnresolved => (
            StatusCode::SERVICE_UNAVAILABLE,
            "service unavailable",
            "budget_accounting_unresolved",
        ),
        BudgetErrorKind::PricingUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "service unavailable",
            "budget_pricing_unavailable",
        ),
        BudgetErrorKind::Exhausted => (StatusCode::FORBIDDEN, "forbidden", "budget_exhausted"),
        BudgetErrorKind::Corrupt | BudgetErrorKind::NumericOverflow => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal server error",
            "budget_accounting_state_invalid",
        ),
    };
    (
        status,
        Json(json!({
            "message": error.message(),
            "name": name,
            "code": code,
        })),
    )
}

fn admin_scope(state: &AdminState) -> BudgetAdminScope {
    BudgetAdminScope {
        workspace_id: state.default_workspace_id,
        default_workspace_id: state.default_workspace_id,
    }
}

fn parse_optional_mutation<T: DeserializeOwned>(
    object: &Map<String, Value>,
    field: &str,
) -> Result<BudgetOptionalMutation<T>, MutationError> {
    match object.get(field) {
        None => Ok(BudgetOptionalMutation::Unchanged),
        Some(Value::Null) => Ok(BudgetOptionalMutation::Clear),
        Some(value) => serde_json::from_value(value.clone())
            .map(BudgetOptionalMutation::Set)
            .map_err(|error| schema_violation(field, error.to_string())),
    }
}

fn parse_expires_at_mutation(
    object: &Map<String, Value>,
) -> Result<BudgetOptionalMutation<DateTime<Utc>>, MutationError> {
    match object.get("expires_at") {
        None => Ok(BudgetOptionalMutation::Unchanged),
        Some(Value::Null) => Ok(BudgetOptionalMutation::Clear),
        Some(value) => {
            let timestamp = value
                .as_i64()
                .ok_or_else(|| schema_violation("expires_at", "expected an integer or null"))?;
            DateTime::from_timestamp(timestamp, 0)
                .map(BudgetOptionalMutation::Set)
                .ok_or_else(|| schema_violation("expires_at", "timestamp is out of range"))
        }
    }
}

fn parse_budget_mutation(
    object: &Map<String, Value>,
) -> Result<BudgetLimitMutation, MutationError> {
    match object.get("budget_limit") {
        None => Ok(BudgetLimitMutation::Unchanged),
        Some(Value::Null) => Ok(BudgetLimitMutation::Clear),
        Some(Value::String(value)) => parse_budget_amount(value)
            .map(BudgetLimitMutation::Set)
            .map_err(|error| schema_violation("budget_limit_decimal", error)),
        Some(_) => Err(schema_violation(
            "budget_limit_decimal",
            "expected a decimal string or null",
        )),
    }
}

async fn select_virtual_key(
    state: &AdminState,
    id_or_name: &str,
) -> Result<AiVirtualKey, MutationError> {
    let primary_key = PrimaryKey::from_str_or_uuid(id_or_name);
    match state.ai_virtual_keys.select(&primary_key).await {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": "ai_virtual_keys not found",
                "name": "not found",
                "code": 3,
            })),
        )),
        Err(error) => Err((
            StatusCode::from_u16(error.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({
                "message": error.to_string(),
                "name": error.error_name(),
                "code": error.error_code(),
            })),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct BudgetLedgerParams {
    pub status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cursor: Option<String>,
    pub size: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct BudgetReconciliationBody {
    pub intent_id: Uuid,
    pub operation_id: Uuid,
    /// 兼容旧版 Manager；新客户端应使用 cost_usd_decimal / waive。
    pub action: Option<String>,
    pub cost_usd_decimal: Option<String>,
    pub waive: Option<bool>,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct BudgetRebuildBody {
    pub operation_id: Uuid,
    pub reason: String,
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

fn parse_reconciliation_resolution(
    body: &BudgetReconciliationBody,
) -> Result<BudgetReconciliationResolution, MutationError> {
    match body.action.as_deref() {
        Some("settle") => {
            if body.waive == Some(true) {
                return Err(schema_violation(
                    "waive",
                    "must not be true when action is settle",
                ));
            }
            let Some(cost) = body.cost_usd_decimal.as_deref() else {
                return Err(schema_violation(
                    "cost_usd_decimal",
                    "required when action is settle",
                ));
            };
            parse_budget_amount(cost)
                .map(|cost_usd| BudgetReconciliationResolution::Settle { cost_usd })
                .map_err(|error| schema_violation("cost_usd_decimal", error))
        }
        Some("waive") => {
            if body.waive == Some(false) {
                return Err(schema_violation(
                    "waive",
                    "must be true or omitted when action is waive",
                ));
            }
            if body.cost_usd_decimal.is_some() {
                return Err(schema_violation(
                    "cost_usd_decimal",
                    "must be omitted when action is waive",
                ));
            }
            Ok(BudgetReconciliationResolution::Waive)
        }
        Some(_) => Err(schema_violation("action", "expected settle or waive")),
        None => match (body.cost_usd_decimal.as_deref(), body.waive) {
            (Some(_), Some(true)) => Err(schema_violation(
                "waive",
                "cost_usd_decimal and waive=true are mutually exclusive",
            )),
            (Some(cost), _) => parse_budget_amount(cost)
                .map(|cost_usd| BudgetReconciliationResolution::Settle { cost_usd })
                .map_err(|error| schema_violation("cost_usd_decimal", error)),
            (None, Some(true)) => Ok(BudgetReconciliationResolution::Waive),
            (None, _) => Err(schema_violation(
                "cost_usd_decimal",
                "provide cost_usd_decimal or waive=true",
            )),
        },
    }
}

fn parse_query_time(
    field: &str,
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, MutationError> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| schema_violation(field, "expected an RFC3339 timestamp"))
        })
        .transpose()
}

fn parse_ledger_cursor(value: Option<&str>) -> Result<Option<BudgetLedgerCursor>, MutationError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let (created_at, id) = value
        .split_once('|')
        .ok_or_else(|| schema_violation("cursor", "invalid cursor"))?;
    let created_at = DateTime::parse_from_rfc3339(created_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| schema_violation("cursor", "invalid cursor timestamp"))?;
    let id = Uuid::parse_str(id).map_err(|_| schema_violation("cursor", "invalid cursor id"))?;
    Ok(Some(BudgetLedgerCursor { created_at, id }))
}

fn encode_ledger_cursor(cursor: &BudgetLedgerCursor) -> String {
    format!("{}|{}", cursor.created_at.to_rfc3339(), cursor.id)
}

fn parse_ledger_statuses(value: Option<&str>) -> Result<Vec<BudgetLedgerStatus>, MutationError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .split(',')
        .map(|status| match status.trim() {
            "pending" => Ok(BudgetLedgerStatus::Pending),
            "unresolved" => Ok(BudgetLedgerStatus::Unresolved),
            "settled" => Ok(BudgetLedgerStatus::Settled),
            "resolved" => Ok(BudgetLedgerStatus::Resolved),
            "waived" => Ok(BudgetLedgerStatus::Waived),
            _ => Err(schema_violation(
                "status",
                "expected pending, unresolved, settled, resolved, or waived",
            )),
        })
        .collect()
}

fn budget_account_json(account: &kong_ai::budget::BudgetAccountSnapshot) -> Value {
    json!({
        "virtual_key_id": account.virtual_key_id,
        "virtual_key_name": account.virtual_key_name,
        "virtual_key_prefix": account.virtual_key_prefix,
        "workspace_id": account.workspace_id,
        "budget_limit_decimal": account.limit_usd.map(decimal_12),
        "budget_used_decimal": decimal_12(account.used_usd),
        "pending_intent_count": account.pending_count,
        "unresolved_intent_count": account.unresolved_count,
        "accounting_revision": account.accounting_revision,
        "checkpoint_tail_events": account.checkpoint_tail_events,
        "accounting_state": account.state.as_str(),
        "state_updated_at": account.state_updated_at.to_rfc3339(),
    })
}

fn budget_ledger_entry_json(entry: &BudgetLedgerEntry) -> Value {
    json!({
        "id": entry.id,
        "virtual_key_id": entry.virtual_key_id,
        "virtual_key_name": entry.virtual_key_name,
        "virtual_key_prefix": entry.virtual_key_prefix,
        "workspace_id": entry.workspace_id,
        "kind": entry.kind.as_str(),
        "status": entry.status.as_str(),
        "request_id": entry.request_id.as_deref(),
        "operation_id": entry.operation_id.as_ref(),
        "dispatch_operation_id": entry.dispatch_operation_id.as_deref(),
        "terminal_operation_id": entry.terminal_operation_id.as_deref(),
        "last_account_revision": entry.last_account_revision,
        "parent_intent_id": entry.parent_intent_id,
        "usage_fact_id": entry.usage_fact_id,
        "observed_cost_usd_decimal": entry.observed_cost_usd.map(decimal_12),
        "accounted_cost_usd_decimal": entry.accounted_cost_usd.map(decimal_12),
        "cost_status": entry.cost_status.map(|status| status.as_str()),
        "cost_reasons": entry.cost_reasons,
        "pricing_fingerprint": entry.pricing_fingerprint.as_deref(),
        "pricing_snapshot": entry.pricing_snapshot,
        "dispatch_state": entry.dispatch_state.as_deref(),
        "stale_not_before": entry.stale_not_before.map(|value| value.to_rfc3339()),
        "resolution_reason": entry.resolution_reason,
        "resolution_actor": entry.resolution_actor,
        "resolution_entry_id": entry.resolution_entry_id,
        "created_at": entry.created_at.to_rfc3339(),
        "updated_at": entry.updated_at.to_rfc3339(),
        "settled_at": entry.settled_at.map(|value| value.to_rfc3339()),
        "resolved_at": entry.resolved_at.map(|value| value.to_rfc3339()),
    })
}

fn normalize_mutation_input(mut body: Value, kind: MutationKind) -> Result<Value, MutationError> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| schema_violation("@entity", "expected a record"))?;

    for field in SERVER_OWNED_FIELDS {
        if object.contains_key(*field) {
            return Err(schema_violation(field, "field is read-only"));
        }
    }

    match object.get("name") {
        Some(Value::String(name)) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(schema_violation("name", "required field missing"));
            }
            object.insert("name".to_string(), Value::String(trimmed.to_string()));
        }
        Some(_) => return Err(schema_violation("name", "expected a string")),
        None if kind == MutationKind::Create => {
            return Err(schema_violation("name", "required field missing"));
        }
        None => {}
    }

    for field in ["tpm_limit", "rpm_limit"] {
        validate_quota_limit(object, field)?;
    }

    normalize_budget_limit(object)?;
    Ok(body)
}

fn validate_quota_limit(object: &Map<String, Value>, field: &str) -> Result<(), MutationError> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let valid = value
        .as_u64()
        .map(|limit| (1..=MAX_VIRTUAL_KEY_LIMIT).contains(&limit))
        .unwrap_or(false);
    if valid {
        Ok(())
    } else {
        Err(schema_violation(
            field,
            format!(
                "expected an integer between 1 and {} or null",
                MAX_VIRTUAL_KEY_LIMIT
            ),
        ))
    }
}

fn normalize_budget_limit(object: &mut Map<String, Value>) -> Result<(), MutationError> {
    let decimal = parse_decimal_input(object.get("budget_limit_decimal"))?;
    let legacy = parse_legacy_budget_input(object.get("budget_limit"))?;
    let normalized = match (decimal, legacy) {
        (OptionalBudgetInput::Missing, OptionalBudgetInput::Missing) => return Ok(()),
        (OptionalBudgetInput::Missing, value) | (value, OptionalBudgetInput::Missing) => value,
        (OptionalBudgetInput::Null, OptionalBudgetInput::Null) => OptionalBudgetInput::Null,
        (OptionalBudgetInput::Value(left), OptionalBudgetInput::Value(right)) if left == right => {
            OptionalBudgetInput::Value(left)
        }
        _ => {
            return Err(schema_violation(
                "budget_limit_decimal",
                "budget_limit_decimal and budget_limit must represent the same value",
            ));
        }
    };

    object.remove("budget_limit_decimal");
    match normalized {
        OptionalBudgetInput::Missing => {}
        OptionalBudgetInput::Null => {
            object.insert("budget_limit".to_string(), Value::Null);
        }
        OptionalBudgetInput::Value(value) => {
            object.insert("budget_limit".to_string(), Value::String(decimal_12(value)));
        }
    }
    Ok(())
}

fn parse_decimal_input(value: Option<&Value>) -> Result<OptionalBudgetInput, MutationError> {
    match value {
        None => Ok(OptionalBudgetInput::Missing),
        Some(Value::Null) => Ok(OptionalBudgetInput::Null),
        Some(Value::String(value)) => parse_budget_amount(value)
            .map(OptionalBudgetInput::Value)
            .map_err(|message| schema_violation("budget_limit_decimal", message)),
        Some(_) => Err(schema_violation(
            "budget_limit_decimal",
            "expected a decimal string or null",
        )),
    }
}

fn parse_legacy_budget_input(value: Option<&Value>) -> Result<OptionalBudgetInput, MutationError> {
    match value {
        None => Ok(OptionalBudgetInput::Missing),
        Some(Value::Null) => Ok(OptionalBudgetInput::Null),
        Some(Value::Number(value)) => parse_budget_amount(&value.to_string())
            .map(OptionalBudgetInput::Value)
            .map_err(|message| schema_violation("budget_limit", message)),
        Some(_) => Err(schema_violation(
            "budget_limit",
            "expected a number or null; use budget_limit_decimal for exact string input",
        )),
    }
}

fn project_response(state: &AdminState, status: StatusCode, mut response: Value) -> Value {
    if !status.is_success() {
        return response;
    }

    if let Some(data) = response
        .as_object_mut()
        .and_then(|object| object.get_mut("data"))
        .and_then(Value::as_array_mut)
    {
        for item in data {
            project_virtual_key(state, item);
        }
    } else {
        project_virtual_key(state, &mut response);
    }

    response
}

fn project_virtual_key(state: &AdminState, value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    object.remove("key_hash");

    let budget_limit = object.get("budget_limit").and_then(response_budget_decimal);
    let budget_used = object
        .get("budget_used")
        .and_then(response_budget_decimal)
        .unwrap_or(Decimal::ZERO);
    let pending_count = object
        .get("budget_pending_count")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let unresolved_count = object
        .get("budget_unresolved_count")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let accounting_unresolved = unresolved_count > 0
        || (budget_limit.is_none() && pending_count > 0)
        || object
            .get("budget_accounting_state")
            .and_then(Value::as_str)
            .is_some_and(|state| state == "unresolved");

    object.insert(
        "budget_limit_decimal".to_string(),
        budget_limit
            .map(decimal_12)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "budget_used_decimal".to_string(),
        Value::String(decimal_12(budget_used)),
    );
    object.insert(
        "budget_limit".to_string(),
        budget_limit
            .and_then(legacy_budget_number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "budget_used".to_string(),
        legacy_budget_number(budget_used)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    );
    object.insert("pending_intent_count".to_string(), json!(pending_count));
    object.insert(
        "unresolved_intent_count".to_string(),
        json!(unresolved_count),
    );

    let capability = ProjectionCapability::from_state(state);
    object.insert(
        "capability".to_string(),
        json!({
            "quota": capability.quota,
            "budget": capability.budget,
        }),
    );

    let workspace_id = object
        .get("ws_id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok());
    let coverage = state
        .ai_policy_coverage
        .read()
        .map(|index| index.coverage_for(workspace_id))
        .unwrap_or_default();
    let coverage_available = coverage.coverage_available;
    object.insert(
        "coverage_available".to_string(),
        Value::Bool(coverage_available),
    );
    let coverage_value = |count| {
        coverage_available
            .then_some(json!(count))
            .unwrap_or(Value::Null)
    };
    object.insert(
        "auth_endpoint_count".to_string(),
        coverage_value(coverage.auth_endpoint_count),
    );
    object.insert(
        "enforced_endpoint_count".to_string(),
        coverage_value(coverage.enforced_endpoint_count),
    );
    object.insert(
        "policy_error_count".to_string(),
        coverage_value(coverage.policy_error_count),
    );

    let quota_configured = ["rpm_limit", "tpm_limit"]
        .iter()
        .any(|field| object.get(*field).is_some_and(|value| !value.is_null()));
    let quota_enforcement = if capability.quota == "unsupported" {
        "unsupported"
    } else if !quota_configured {
        "unconfigured"
    } else if !coverage_available || coverage.enforced_endpoint_count == 0 {
        "awaiting_plugin"
    } else if coverage.enforced_endpoint_count < coverage.auth_endpoint_count {
        "configured_local_partial"
    } else {
        "configured_local"
    };
    object.insert(
        "quota_enforcement".to_string(),
        Value::String(quota_enforcement.to_string()),
    );
    object.insert(
        "quota_backend".to_string(),
        capability
            .quota_backend
            .map(|backend| Value::String(backend.to_string()))
            .unwrap_or(Value::Null),
    );
    object.insert(
        "quota_scope".to_string(),
        capability
            .quota_backend
            .map(|_| Value::String("node".to_string()))
            .unwrap_or(Value::Null),
    );
    object.insert(
        "quota_window_seconds".to_string(),
        capability
            .quota_backend
            .map(|_| json!(60))
            .unwrap_or(Value::Null),
    );

    let financial_status =
        budget_financial_status(budget_limit, budget_used, accounting_unresolved);
    let budget_status = if capability.budget == "unsupported" {
        "unsupported"
    } else if accounting_unresolved {
        "unresolved"
    } else if capability.budget == "accounting_unavailable" {
        "unavailable"
    } else if budget_limit.is_none() {
        financial_status
    } else if !coverage_available || coverage.enforced_endpoint_count == 0 {
        "awaiting_plugin"
    } else {
        financial_status
    };
    object.insert(
        "budget_status".to_string(),
        Value::String(budget_status.to_string()),
    );
    object.insert(
        "budget_financial_status".to_string(),
        Value::String(financial_status.to_string()),
    );
    object.insert(
        "budget_backend".to_string(),
        capability
            .budget_backend
            .map(|backend| Value::String(backend.to_string()))
            .unwrap_or(Value::Null),
    );
    object.insert(
        "budget_percentage_decimal".to_string(),
        budget_limit
            .map(|limit| Value::String(budget_percentage(budget_used, limit)))
            .unwrap_or(Value::Null),
    );
}

fn response_budget_decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::String(value) => parse_budget_amount(value).ok(),
        Value::Number(value) => parse_budget_amount(&value.to_string()).ok(),
        _ => None,
    }
}

fn legacy_budget_number(value: Decimal) -> Option<serde_json::Number> {
    let safe_integer_max = Decimal::from_i128_with_scale(JSON_SAFE_INTEGER_MAX, 0);
    if value < Decimal::ZERO || value > safe_integer_max {
        return None;
    }

    let projected = value.to_string().parse::<f64>().ok()?;
    if !projected.is_finite() {
        return None;
    }
    let round_trip = parse_budget_amount(&projected.to_string()).ok()?;
    if round_trip != value {
        return None;
    }

    serde_json::Number::from_f64(projected)
}

fn budget_financial_status(
    budget_limit: Option<Decimal>,
    budget_used: Decimal,
    accounting_unresolved: bool,
) -> &'static str {
    if accounting_unresolved {
        return "unresolved";
    }

    let Some(limit) = budget_limit else {
        return if budget_used.is_zero() {
            "unconfigured"
        } else {
            "paused"
        };
    };

    if budget_used >= limit {
        "exhausted"
    } else if ratio_at_least(budget_used, limit, 4, 5) {
        "warning"
    } else {
        "active"
    }
}

fn ratio_at_least(value: Decimal, limit: Decimal, numerator: u8, denominator: u8) -> bool {
    let left = BigInt::from(value.mantissa()) * BigInt::from(denominator) * ten_pow(limit.scale());
    let right = BigInt::from(limit.mantissa()) * BigInt::from(numerator) * ten_pow(value.scale());
    left >= right
}

fn budget_percentage(used: Decimal, limit: Decimal) -> String {
    if limit.is_zero() {
        return "100.000000000000".to_string();
    }

    let numerator =
        BigInt::from(used.mantissa()) * BigInt::from(100u8) * ten_pow(12 + limit.scale());
    let denominator = BigInt::from(limit.mantissa()) * ten_pow(used.scale());
    let mut scaled = &numerator / &denominator;
    let remainder = numerator % &denominator;
    if remainder * BigInt::from(2u8) >= denominator {
        scaled += 1;
    }

    format_scaled_12(scaled)
}

fn ten_pow(exponent: u32) -> BigInt {
    BigInt::from(10u8).pow(exponent)
}

fn format_scaled_12(value: BigInt) -> String {
    let digits = value.to_string();
    if digits.len() <= 12 {
        return format!("0.{:0>12}", digits);
    }
    let split = digits.len() - 12;
    format!("{}.{}", &digits[..split], &digits[split..])
}

/// Generate a new virtual key — 生成新的虚拟密钥
fn generate_key() -> (String, String, String) {
    let raw_key = format!(
        "sk-kr-{}",
        uuid::Uuid::new_v4().to_string().replace("-", "")
    );
    let key_hash = format!("{:x}", Sha256::digest(raw_key.as_bytes()));
    let key_prefix = raw_key[..8].to_string();
    (raw_key, key_hash, key_prefix)
}

/// GET /ai-virtual-keys — 列出所有 AI Virtual Key
pub async fn list(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_list::<AiVirtualKey>(&state.ai_virtual_keys, &params).await;
    let resp = project_response(&state, status, resp);
    (status, Json(resp))
}

/// GET /ai-virtual-keys/:id_or_name — 获取单个 AI Virtual Key
pub async fn get_one(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_get::<AiVirtualKey>(&state.ai_virtual_keys, &id_or_name).await;
    let resp = project_response(&state, status, resp);
    (status, Json(resp))
}

/// POST /ai-virtual-keys — 创建 AI Virtual Key（生成密钥，存储哈希，一次性返回原始密钥）
pub async fn create(
    State(state): State<AdminState>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let mut body = match normalize_mutation_input(body, MutationKind::Create) {
        Ok(body) => body,
        Err(error) => return error,
    };
    let (raw_key, key_hash, key_prefix) = generate_key();
    let Some(governance) = state.ai_budget_governance.as_ref().map(Arc::clone) else {
        return budget_admin_unavailable();
    };

    // 服务端生成不可伪造的身份字段与主键。
    if let Some(obj) = body.as_object_mut() {
        obj.insert("key_hash".to_string(), json!(key_hash));
        obj.insert("key_prefix".to_string(), json!(key_prefix));
        if !obj.contains_key("id") {
            obj.insert("id".to_string(), json!(Uuid::new_v4()));
        }
        if !obj.contains_key("enabled") {
            obj.insert("enabled".to_string(), json!(true));
        }
    }

    let entity: AiVirtualKey = match serde_json::from_value(body) {
        Ok(entity) => entity,
        Err(error) => return schema_violation("@entity", error.to_string()),
    };
    let expires_at = match entity.expires_at {
        Some(timestamp) => match DateTime::from_timestamp(timestamp, 0) {
            Some(value) => Some(value),
            None => return schema_violation("expires_at", "timestamp is out of range"),
        },
        None => None,
    };
    if let Err(error) = governance
        .create_account(CreateBudgetAccount {
            virtual_key_id: entity.id,
            name: entity.name,
            key_hash: entity.key_hash,
            key_prefix: entity.key_prefix,
            consumer_id: entity.consumer_id,
            allowed_models: entity.allowed_models,
            tpm_limit: entity.tpm_limit,
            rpm_limit: entity.rpm_limit,
            budget_limit_usd: entity.budget_limit,
            enabled: entity.enabled,
            expires_at,
            tags: entity.tags,
            workspace_id: state.default_workspace_id,
        })
        .await
    {
        return budget_store_error(error);
    }

    let created = match select_virtual_key(&state, &entity.id.to_string()).await {
        Ok(created) => created,
        Err(error) => return error,
    };
    let mut response = serde_json::to_value(created).unwrap_or(Value::Null);
    if let Some(object) = response.as_object_mut() {
        object.remove("key_hash");
        object.insert("key".to_string(), json!(raw_key));
    }
    state.virtual_key_auth.invalidate_all();
    let response = project_response(&state, StatusCode::CREATED, response);
    (StatusCode::CREATED, Json(response))
}

/// PATCH /ai-virtual-keys/:id_or_name — 更新 AI Virtual Key
pub async fn update(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let body = match normalize_mutation_input(body, MutationKind::Patch) {
        Ok(body) => body,
        Err(error) => return error,
    };
    let Some(governance) = state.ai_budget_governance.as_ref().map(Arc::clone) else {
        return budget_admin_unavailable();
    };
    let existing = match select_virtual_key(&state, &id_or_name).await {
        Ok(existing) => existing,
        Err(error) => return error,
    };
    let object = body.as_object().expect("normalization requires an object");
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let enabled = match object.get("enabled") {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return schema_violation("enabled", "expected a boolean"),
    };
    let command = UpdateBudgetAccount {
        virtual_key_id: existing.id,
        scope: admin_scope(&state),
        name,
        consumer_id: match parse_optional_mutation(object, "consumer_id") {
            Ok(value) => value,
            Err(error) => return error,
        },
        allowed_models: match parse_optional_mutation(object, "allowed_models") {
            Ok(value) => value,
            Err(error) => return error,
        },
        tpm_limit: match parse_optional_mutation(object, "tpm_limit") {
            Ok(value) => value,
            Err(error) => return error,
        },
        rpm_limit: match parse_optional_mutation(object, "rpm_limit") {
            Ok(value) => value,
            Err(error) => return error,
        },
        budget_limit: match parse_budget_mutation(object) {
            Ok(value) => value,
            Err(error) => return error,
        },
        enabled,
        expires_at: match parse_expires_at_mutation(object) {
            Ok(value) => value,
            Err(error) => return error,
        },
        tags: match parse_optional_mutation(object, "tags") {
            Ok(value) => value,
            Err(error) => return error,
        },
    };
    if let Err(error) = governance.update_account(command).await {
        return budget_store_error(error);
    }
    let updated = match select_virtual_key(&state, &existing.id.to_string()).await {
        Ok(updated) => updated,
        Err(error) => return error,
    };
    state.virtual_key_auth.invalidate_all();
    let response = project_response(
        &state,
        StatusCode::OK,
        serde_json::to_value(updated).unwrap(),
    );
    (StatusCode::OK, Json(response))
}

/// DELETE /ai-virtual-keys/:id_or_name — 删除 AI Virtual Key
pub async fn delete_one(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let Some(governance) = state.ai_budget_governance.as_ref().map(Arc::clone) else {
        return budget_admin_unavailable().into_response();
    };
    let existing = match select_virtual_key(&state, &id_or_name).await {
        Ok(existing) => existing,
        Err(error) => return error.into_response(),
    };
    match governance
        .delete_account(DeleteBudgetAccount {
            virtual_key_id: existing.id,
            scope: admin_scope(&state),
        })
        .await
    {
        Ok(_) => {
            state.virtual_key_auth.invalidate_all();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => budget_store_error(error).into_response(),
    }
}

/// GET /ai-virtual-keys/:id/budget-ledger — 查询预算账本
pub async fn budget_ledger(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(params): Query<BudgetLedgerParams>,
) -> impl IntoResponse {
    let Some(store) = state.ai_budget_admin.as_ref().map(Arc::clone) else {
        return budget_admin_unavailable();
    };
    let virtual_key_id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return schema_violation("id", "expected a UUID"),
    };
    let query = BudgetLedgerQuery {
        virtual_key_id,
        scope: admin_scope(&state),
        statuses: match parse_ledger_statuses(params.status.as_deref()) {
            Ok(value) => value,
            Err(error) => return error,
        },
        created_at_from: match parse_query_time("from", params.from.as_deref()) {
            Ok(value) => value,
            Err(error) => return error,
        },
        created_at_to: match parse_query_time("to", params.to.as_deref()) {
            Ok(value) => value,
            Err(error) => return error,
        },
        after: match parse_ledger_cursor(params.cursor.as_deref()) {
            Ok(value) => value,
            Err(error) => return error,
        },
        page_size: params.size.unwrap_or(50),
    };
    match store.list_ledger(query).await {
        Ok(page) => (
            StatusCode::OK,
            Json(json!({
                "data": page.entries.iter().map(budget_ledger_entry_json).collect::<Vec<_>>(),
                "account": page.account.as_ref().map(budget_account_json),
                "next_cursor": page.next_cursor.as_ref().map(encode_ledger_cursor),
            })),
        ),
        Err(error) => budget_store_error(error),
    }
}

/// POST /ai-virtual-keys/:id/budget-reconciliations — 人工结算或豁免未决 intent
pub async fn reconcile_budget(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(body): Json<BudgetReconciliationBody>,
) -> impl IntoResponse {
    let Some(store) = state.ai_budget_admin.as_ref().map(Arc::clone) else {
        return budget_admin_unavailable();
    };
    let virtual_key_id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return schema_violation("id", "expected a UUID"),
    };
    let reason = body.reason.trim();
    if reason.is_empty() {
        return schema_violation("reason", "required field missing");
    }
    let resolution = match parse_reconciliation_resolution(&body) {
        Ok(resolution) => resolution,
        Err(error) => return error,
    };
    match store
        .reconcile(ReconcileBudgetIntent {
            virtual_key_id,
            intent_id: body.intent_id,
            operation_id: body.operation_id,
            scope: admin_scope(&state),
            // 当前 Admin 尚未接入管理员主体认证；使用跨节点稳定 actor 保证同一
            // operation ID 在负载均衡重试时仍可幂等 replay。
            actor: Arc::from("admin-api"),
            reason: reason.to_string(),
            resolution,
        })
        .await
    {
        Ok(result) => (
            StatusCode::OK,
            Json(json!({
                "disposition": result.disposition.as_str(),
                "account": budget_account_json(&result.account),
                "intent": budget_ledger_entry_json(&result.intent),
                "audit_entry": budget_ledger_entry_json(&result.audit_entry),
            })),
        ),
        Err(error) => budget_store_error(error),
    }
}

/// POST /ai-virtual-keys/:id/budget-ledger/rebuild — 校验或重建预算聚合
pub async fn rebuild_budget_ledger(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(body): Json<BudgetRebuildBody>,
) -> impl IntoResponse {
    let Some(store) = state.ai_budget_admin.as_ref().map(Arc::clone) else {
        return budget_admin_unavailable();
    };
    let virtual_key_id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return schema_violation("id", "expected a UUID"),
    };
    let reason = body.reason.trim();
    if reason.is_empty() {
        return schema_violation("reason", "required field missing");
    }
    match store
        .verify_or_rebuild(RebuildBudgetAccount {
            virtual_key_id,
            operation_id: body.operation_id,
            scope: admin_scope(&state),
            actor: Arc::from("admin-api"),
            reason: reason.to_string(),
            dry_run: body.dry_run,
            max_attempts: 3,
        })
        .await
    {
        Ok(result) => (
            StatusCode::OK,
            Json(json!({
                "disposition": result.disposition.as_str(),
                "account": budget_account_json(&result.account),
                "comparison": {
                    "snapshot_revision": result.comparison.snapshot_revision,
                    "checkpoint_revision": result.comparison.checkpoint_revision,
                    "stored_used_usd_decimal": decimal_12(result.comparison.stored_used_usd),
                    "recomputed_used_usd_decimal": decimal_12(result.comparison.recomputed_used_usd),
                    "difference_usd_decimal": decimal_12(result.comparison.difference_usd),
                    "stored_pending_count": result.comparison.stored_pending_count,
                    "recomputed_pending_count": result.comparison.recomputed_pending_count,
                    "stored_unresolved_count": result.comparison.stored_unresolved_count,
                    "recomputed_unresolved_count": result.comparison.recomputed_unresolved_count,
                    "unresolved_request_count": result.comparison.unresolved_request_count,
                    "open_account_issue_count": result.comparison.open_account_issue_count,
                    "is_current": result.comparison.is_current,
                },
                "audit_entry": result.audit_entry.as_ref().map(budget_ledger_entry_json),
            })),
        ),
        Err(error) => budget_store_error(error),
    }
}

/// POST /ai-virtual-keys/:id/rotate — 轮换密钥（生成新密钥，更新哈希）
pub async fn rotate(State(state): State<AdminState>, Path(id): Path<String>) -> impl IntoResponse {
    // 先获取现有 key — fetch existing key first
    let pk = PrimaryKey::from_str_or_uuid(&id);
    let existing = match state.ai_virtual_keys.select(&pk).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("{} not found", AiVirtualKey::table_name()),
                    "name": "not found",
                    "code": 3,
                })),
            )
                .into_response();
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return (status, Json(json!({"message": e.to_string()}))).into_response();
        }
    };

    // 生成新密钥 — generate new key
    let (raw_key, key_hash, key_prefix) = generate_key();

    // 更新 key_hash 和 key_prefix — update key_hash and key_prefix
    let update_body = json!({
        "key_hash": key_hash,
        "key_prefix": key_prefix,
    });

    let (status, Json(mut resp)) = do_update::<AiVirtualKey>(
        &state.ai_virtual_keys,
        &existing.id.to_string(),
        &update_body,
    )
    .await;

    if status.is_success() {
        // 一次性返回新密钥 — return new key once
        if let Some(obj) = resp.as_object_mut() {
            obj.remove("key_hash");
            obj.insert("key".to_string(), json!(raw_key));
        }
        // 轮换后旧密钥必须立即失效 — the rotated-out key must stop working immediately
        state.virtual_key_auth.invalidate_all();
    }

    let resp = project_response(&state, status, resp);
    (status, Json(resp)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reconciliation_body(
        action: Option<&str>,
        cost_usd_decimal: Option<&str>,
        waive: Option<bool>,
    ) -> BudgetReconciliationBody {
        BudgetReconciliationBody {
            intent_id: Uuid::nil(),
            operation_id: Uuid::nil(),
            action: action.map(str::to_string),
            cost_usd_decimal: cost_usd_decimal.map(str::to_string),
            waive,
            reason: "invoice reviewed".to_string(),
        }
    }

    fn assert_resolution_error(
        body: &BudgetReconciliationBody,
        expected_field: &str,
        expected_message: &str,
    ) {
        let (status, Json(value)) = parse_reconciliation_resolution(body).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            value["fields"][expected_field],
            Value::String(expected_message.to_string())
        );
    }

    #[test]
    fn parses_canonical_settlement_and_waiver() {
        let settlement =
            parse_reconciliation_resolution(&reconciliation_body(None, Some("1.25"), Some(false)))
                .unwrap();
        assert_eq!(
            settlement,
            BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::new(125, 2),
            }
        );

        let settlement_without_false =
            parse_reconciliation_resolution(&reconciliation_body(None, Some("0"), None)).unwrap();
        assert_eq!(
            settlement_without_false,
            BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::ZERO,
            }
        );

        let waiver =
            parse_reconciliation_resolution(&reconciliation_body(None, None, Some(true))).unwrap();
        assert_eq!(waiver, BudgetReconciliationResolution::Waive);
    }

    #[test]
    fn accepts_legacy_action_payloads() {
        let settlement = parse_reconciliation_resolution(&reconciliation_body(
            Some("settle"),
            Some("2.5"),
            None,
        ))
        .unwrap();
        assert_eq!(
            settlement,
            BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::new(25, 1),
            }
        );

        let waiver =
            parse_reconciliation_resolution(&reconciliation_body(Some("waive"), None, None))
                .unwrap();
        assert_eq!(waiver, BudgetReconciliationResolution::Waive);
    }

    #[test]
    fn rejects_conflicting_canonical_resolution_fields() {
        assert_resolution_error(
            &reconciliation_body(None, Some("1"), Some(true)),
            "waive",
            "cost_usd_decimal and waive=true are mutually exclusive",
        );
        assert_resolution_error(
            &reconciliation_body(Some("settle"), Some("1"), Some(true)),
            "waive",
            "must not be true when action is settle",
        );
        assert_resolution_error(
            &reconciliation_body(Some("waive"), Some("1"), Some(true)),
            "cost_usd_decimal",
            "must be omitted when action is waive",
        );
        assert_resolution_error(
            &reconciliation_body(Some("waive"), None, Some(false)),
            "waive",
            "must be true or omitted when action is waive",
        );
    }

    #[test]
    fn rejects_missing_or_invalid_resolution_fields() {
        assert_resolution_error(
            &reconciliation_body(None, None, None),
            "cost_usd_decimal",
            "provide cost_usd_decimal or waive=true",
        );
        assert_resolution_error(
            &reconciliation_body(None, None, Some(false)),
            "cost_usd_decimal",
            "provide cost_usd_decimal or waive=true",
        );
        assert_resolution_error(
            &reconciliation_body(Some("settle"), None, None),
            "cost_usd_decimal",
            "required when action is settle",
        );
        assert_resolution_error(
            &reconciliation_body(Some("invalid"), None, None),
            "action",
            "expected settle or waive",
        );
    }

    #[test]
    fn rejects_invalid_settlement_amount() {
        let error = parse_budget_amount("-1").unwrap_err();
        assert_resolution_error(
            &reconciliation_body(None, Some("-1"), None),
            "cost_usd_decimal",
            &error,
        );
    }
}
