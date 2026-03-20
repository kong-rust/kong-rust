use async_trait::async_trait;
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use kong_core::error::{KongError, Result};
use kong_core::traits::{Dao, Entity, Page, PageParams, PrimaryKey};

use crate::database::Database;

/// Generic PostgreSQL DAO implementation — 通用 PostgreSQL DAO 实现
///
/// Dynamically generates SQL queries based on Entity trait and EntitySchema definitions. — 基于实体的 Entity trait 和 EntitySchema 描述，动态生成 SQL 查询。
/// Uses serde_json as an intermediate format for entity-row conversion. — 使用 serde_json 作为中间格式进行实体与数据库行之间的转换。
pub struct PgDao<T: Entity> {
    db: Database,
    /// Entity column definitions (column names, type mappings) — 实体的列描述（列名、类型映射）
    schema: EntitySchema,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Entity> PgDao<T> {
    pub fn new(db: Database, schema: EntitySchema) -> Self {
        Self {
            db,
            schema,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Column type descriptor — 列类型描述
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnType {
    /// UUID type — UUID 类型
    Uuid,
    /// Text type — 文本类型
    Text,
    /// Integer type — 整数类型
    Integer,
    /// Float type — 浮点类型
    Float,
    /// Boolean type — 布尔类型
    Boolean,
    /// Timestamp (stored as TIMESTAMP WITH TIME ZONE, API returns epoch seconds) — 时间戳（存储为 TIMESTAMP WITH TIME ZONE，API 返回 epoch 秒）
    Timestamp,
    /// Millisecond-precision timestamp (e.g. Target's created_at) — 时间戳（毫秒精度，如 Target 的 created_at）
    TimestampMs,
    /// JSONB type (e.g. config, headers) — JSONB 类型（如 config、headers）
    Jsonb,
    /// TEXT[] array type (e.g. tags, methods) — TEXT[] 数组类型（如 tags、methods）
    TextArray,
    /// JSONB[] array type (e.g. routes.sources, routes.destinations) — JSONB[] 数组类型（如 routes.sources、routes.destinations）
    JsonbArray,
    /// UUID[] array type (e.g. services.ca_certificates) — UUID[] 数组类型（如 services.ca_certificates）
    UuidArray,
    /// UUID foreign key (JSON: { "id": "uuid" }, DB: <field>_id UUID column) — UUID 外键（JSON 中为 { "id": "uuid" }，DB 中为 <field>_id UUID 列）
    ForeignKey,
}

/// Column definition — 列描述
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// JSON field name (e.g. "service") — JSON 字段名（如 "service"）
    pub json_name: String,
    /// Database column name (e.g. "service_id") — 数据库列名（如 "service_id"）
    pub db_column: String,
    /// Column type — 列类型
    pub col_type: ColumnType,
    /// Whether the column is nullable — 是否可空
    pub nullable: bool,
}

/// Entity schema descriptor — 实体 Schema 描述
#[derive(Debug, Clone)]
pub struct EntitySchema {
    /// Database table name — 数据库表名
    pub table_name: String,
    /// List of column definitions — 列定义列表
    pub columns: Vec<ColumnDef>,
}

impl EntitySchema {
    pub fn new(table_name: &str) -> Self {
        Self {
            table_name: table_name.to_string(),
            columns: Vec::new(),
        }
    }

    /// Add a column definition — 添加列定义
    pub fn column(
        mut self,
        json_name: &str,
        db_column: &str,
        col_type: ColumnType,
        nullable: bool,
    ) -> Self {
        self.columns.push(ColumnDef {
            json_name: json_name.to_string(),
            db_column: db_column.to_string(),
            col_type,
            nullable,
        });
        self
    }

    /// Shortcut: add UUID primary key column — 快捷方法: 添加 UUID 主键列
    pub fn pk(self) -> Self {
        self.column("id", "id", ColumnType::Uuid, false)
    }

    /// Shortcut: add timestamp column pair — 快捷方法: 添加时间戳列对
    pub fn timestamps(self) -> Self {
        self.column("created_at", "created_at", ColumnType::Timestamp, false)
            .column("updated_at", "updated_at", ColumnType::Timestamp, false)
    }

    /// Shortcut: add tags column — 快捷方法: 添加 tags 列
    pub fn tags(self) -> Self {
        self.column("tags", "tags", ColumnType::TextArray, true)
    }

    /// Shortcut: add required text column — 快捷方法: 添加必填文本列
    pub fn text(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Text, false)
    }

    /// Shortcut: add optional text column — 快捷方法: 添加可空文本列
    pub fn text_opt(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Text, true)
    }

    /// Shortcut: add required integer column — 快捷方法: 添加必填整数列
    pub fn integer(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Integer, false)
    }

    /// Shortcut: add optional integer column — 快捷方法: 添加可空整数列
    pub fn integer_opt(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Integer, true)
    }

    /// Shortcut: add boolean column — 快捷方法: 添加布尔列
    pub fn boolean(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Boolean, false)
    }

    /// Shortcut: add optional boolean column — 快捷方法: 添加可空布尔列
    pub fn boolean_opt(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Boolean, true)
    }

    /// Shortcut: add JSONB column — 快捷方法: 添加 JSONB 列
    pub fn jsonb(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Jsonb, true)
    }

    /// Shortcut: add foreign key column — 快捷方法: 添加外键列
    pub fn foreign_key(self, name: &str) -> Self {
        self.column(name, &format!("{}_id", name), ColumnType::ForeignKey, true)
    }

    /// Shortcut: add required foreign key column — 快捷方法: 添加必填外键列
    pub fn foreign_key_required(self, name: &str) -> Self {
        self.column(name, &format!("{}_id", name), ColumnType::ForeignKey, false)
    }

    /// Shortcut: add text array column — 快捷方法: 添加文本数组列
    pub fn text_array(self, name: &str) -> Self {
        self.column(name, name, ColumnType::TextArray, true)
    }

    /// Shortcut: add JSONB[] array column — 快捷方法: 添加 JSONB[] 数组列
    pub fn jsonb_array(self, name: &str) -> Self {
        self.column(name, name, ColumnType::JsonbArray, true)
    }

    /// Shortcut: add UUID[] array column — 快捷方法: 添加 UUID[] 数组列
    pub fn uuid_array(self, name: &str) -> Self {
        self.column(name, name, ColumnType::UuidArray, true)
    }

    /// Shortcut: add float column — 快捷方法: 添加浮点列
    pub fn float(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Float, false)
    }

    /// Shortcut: add optional float column — 快捷方法: 添加可空浮点列
    pub fn float_opt(self, name: &str) -> Self {
        self.column(name, name, ColumnType::Float, true)
    }

    /// Find column definition by JSON name — 查找列定义
    fn find_column(&self, json_name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.json_name == json_name)
    }

    /// Find column definition by DB column name — 查找列定义（按 DB 列名）
    #[allow(dead_code)]
    fn find_column_by_db(&self, db_column: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.db_column == db_column)
    }
}

// ============ Helper functions — 辅助函数 ============

/// Build a JSON object from PgRow and deserialize into an entity — 从 PgRow 构建 JSON 对象，然后反序列化为实体
fn row_to_entity<T: Entity>(row: &PgRow, schema: &EntitySchema) -> Result<T> {
    let mut json_obj = serde_json::Map::new();

    for col in &schema.columns {
        let value = extract_column_value(row, col)?;
        json_obj.insert(col.json_name.clone(), value);
    }

    serde_json::from_value(Value::Object(json_obj))
        .map_err(|e| KongError::SerializationError(format!("行转实体失败: {}", e)))
}

/// Extract a single column value from PgRow as JSON — 从 PgRow 提取单列值为 JSON
fn extract_column_value(row: &PgRow, col: &ColumnDef) -> Result<Value> {
    let db_col = col.db_column.as_str();

    match col.col_type {
        ColumnType::Uuid => {
            let val: Option<Uuid> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .map(|v| Value::String(v.to_string()))
                .unwrap_or(Value::Null))
        }
        ColumnType::Text => {
            let val: Option<String> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val.map(Value::String).unwrap_or(Value::Null))
        }
        ColumnType::Integer => {
            // PostgreSQL integer may be i32 or i64 — PostgreSQL integer 可能是 i32 或 i64
            let val: Option<i64> = row
                .try_get::<Option<i64>, _>(db_col)
                .or_else(|_| {
                    row.try_get::<Option<i32>, _>(db_col)
                        .map(|v| v.map(|n| n as i64))
                })
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .map(|v| Value::Number(serde_json::Number::from(v)))
                .unwrap_or(Value::Null))
        }
        ColumnType::Float => {
            let val: Option<f64> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(match val {
                Some(v) if v.fract() == 0.0 && v >= i64::MIN as f64 && v <= i64::MAX as f64 => {
                    // Output as integer when no fractional part, compatible with i32/i64 model fields — 无小数部分时输出为整数，兼容 Rust 模型中的 i32/i64 字段
                    Value::Number(serde_json::Number::from(v as i64))
                }
                Some(v) => serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                None => Value::Null,
            })
        }
        ColumnType::Boolean => {
            let val: Option<bool> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val.map(Value::Bool).unwrap_or(Value::Null))
        }
        ColumnType::Timestamp => {
            // SELECT expression already converted to f64 via EXTRACT(EPOCH FROM ...) — SELECT 表达式已用 EXTRACT(EPOCH FROM ...) 转为 f64
            // Here db_col actually reads the epoch_<original_column> alias — 此处 db_col 实际读到的是 epoch_<原列名> 别名
            let alias = format!("epoch_{}", db_col);
            let val: Option<f64> = row
                .try_get(alias.as_str())
                .or_else(|_| row.try_get::<Option<f64>, _>(db_col))
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .map(|v| {
                    let secs = v.floor() as i64;
                    Value::Number(serde_json::Number::from(secs))
                })
                .unwrap_or(Value::Null))
        }
        ColumnType::TimestampMs => {
            // Millisecond-precision timestamp, also converted to f64 via EXTRACT — 毫秒精度时间戳，同样通过 EXTRACT 转为 f64
            let alias = format!("epoch_{}", db_col);
            let val: Option<f64> = row
                .try_get(alias.as_str())
                .or_else(|_| row.try_get::<Option<f64>, _>(db_col))
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null))
        }
        ColumnType::Jsonb => {
            let val: Option<Value> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val.unwrap_or(Value::Null))
        }
        ColumnType::TextArray => {
            let val: Option<Vec<String>> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .map(|v| Value::Array(v.into_iter().map(Value::String).collect()))
                .unwrap_or(Value::Null))
        }
        ColumnType::JsonbArray => {
            let val: Option<Vec<Value>> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val.map(|v| Value::Array(v)).unwrap_or(Value::Null))
        }
        ColumnType::UuidArray => {
            let val: Option<Vec<Uuid>> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .map(|v| {
                    Value::Array(
                        v.into_iter()
                            .map(|u| Value::String(u.to_string()))
                            .collect(),
                    )
                })
                .unwrap_or(Value::Null))
        }
        ColumnType::ForeignKey => {
            // Stored as UUID column in DB, needs wrapping as { "id": "uuid" } in JSON — DB 中存储为 UUID 列，JSON 中需要包装为 { "id": "uuid" }
            let val: Option<Uuid> = row
                .try_get(db_col)
                .map_err(|e| KongError::DatabaseError(format!("列 {} 读取失败: {}", db_col, e)))?;
            Ok(val
                .map(|v| {
                    let mut obj = serde_json::Map::new();
                    obj.insert("id".to_string(), Value::String(v.to_string()));
                    Value::Object(obj)
                })
                .unwrap_or(Value::Null))
        }
    }
}

/// Extract column values from entity JSON for SQL binding — 从实体 JSON 提取列值用于 SQL 绑定
fn entity_to_params(entity_json: &Value, schema: &EntitySchema) -> Result<Vec<(String, SqlParam)>> {
    let obj = entity_json
        .as_object()
        .ok_or_else(|| KongError::SerializationError("实体不是 JSON 对象".to_string()))?;

    let mut params = Vec::new();

    for col in &schema.columns {
        let json_val = obj.get(&col.json_name).unwrap_or(&Value::Null);
        let param = json_to_sql_param(json_val, &col.col_type)?;
        params.push((col.db_column.clone(), param));
    }

    Ok(params)
}

/// JSON value to SQL parameter — JSON 值到 SQL 参数
#[derive(Debug, Clone)]
pub enum SqlParam {
    Uuid(Option<Uuid>),
    Text(Option<String>),
    Integer(Option<i64>),
    Float(Option<f64>),
    Boolean(Option<bool>),
    Jsonb(Option<Value>),
    TextArray(Option<Vec<String>>),
    JsonbArray(Option<Vec<Value>>),
    UuidArray(Option<Vec<Uuid>>),
    /// Epoch seconds timestamp — epoch 秒时间戳
    TimestampEpoch(Option<i64>),
    /// Epoch milliseconds timestamp (f64) — epoch 毫秒时间戳（f64）
    TimestampEpochMs(Option<f64>),
}

fn json_to_sql_param(value: &Value, col_type: &ColumnType) -> Result<SqlParam> {
    if value.is_null() {
        return Ok(match col_type {
            ColumnType::Uuid | ColumnType::ForeignKey => SqlParam::Uuid(None),
            ColumnType::Text => SqlParam::Text(None),
            ColumnType::Integer => SqlParam::Integer(None),
            ColumnType::Float => SqlParam::Float(None),
            ColumnType::Boolean => SqlParam::Boolean(None),
            ColumnType::Timestamp => SqlParam::TimestampEpoch(None),
            ColumnType::TimestampMs => SqlParam::TimestampEpochMs(None),
            ColumnType::Jsonb => SqlParam::Jsonb(None),
            ColumnType::TextArray => SqlParam::TextArray(None),
            ColumnType::JsonbArray => SqlParam::JsonbArray(None),
            ColumnType::UuidArray => SqlParam::UuidArray(None),
        });
    }

    match col_type {
        ColumnType::Uuid => {
            let s = value
                .as_str()
                .ok_or_else(|| KongError::ValidationError("UUID 字段必须是字符串".to_string()))?;
            let uuid = Uuid::parse_str(s)
                .map_err(|e| KongError::ValidationError(format!("无效的 UUID: {}", e)))?;
            Ok(SqlParam::Uuid(Some(uuid)))
        }
        ColumnType::ForeignKey => {
            // Extract UUID from { "id": "uuid-string" } — 从 { "id": "uuid-string" } 提取 UUID
            if let Some(obj) = value.as_object() {
                if let Some(id_val) = obj.get("id") {
                    let s = id_val.as_str().ok_or_else(|| {
                        KongError::ValidationError("外键 id 必须是字符串".to_string())
                    })?;
                    let uuid = Uuid::parse_str(s).map_err(|e| {
                        KongError::ValidationError(format!("无效的外键 UUID: {}", e))
                    })?;
                    return Ok(SqlParam::Uuid(Some(uuid)));
                }
            }
            // Also supports passing UUID string directly — 也支持直接传 UUID 字符串
            if let Some(s) = value.as_str() {
                let uuid = Uuid::parse_str(s)
                    .map_err(|e| KongError::ValidationError(format!("无效的外键 UUID: {}", e)))?;
                return Ok(SqlParam::Uuid(Some(uuid)));
            }
            Err(KongError::ValidationError("外键字段格式无效".to_string()))
        }
        ColumnType::Text => {
            let s = value
                .as_str()
                .ok_or_else(|| KongError::ValidationError("文本字段必须是字符串".to_string()))?;
            Ok(SqlParam::Text(Some(s.to_string())))
        }
        ColumnType::Integer => {
            let n = value
                .as_i64()
                .ok_or_else(|| KongError::ValidationError("整数字段必须是数字".to_string()))?;
            Ok(SqlParam::Integer(Some(n)))
        }
        ColumnType::Float => {
            let n = value
                .as_f64()
                .ok_or_else(|| KongError::ValidationError("浮点字段必须是数字".to_string()))?;
            Ok(SqlParam::Float(Some(n)))
        }
        ColumnType::Boolean => {
            let b = value
                .as_bool()
                .ok_or_else(|| KongError::ValidationError("布尔字段必须是 bool".to_string()))?;
            Ok(SqlParam::Boolean(Some(b)))
        }
        ColumnType::Timestamp => {
            let n = value
                .as_i64()
                .ok_or_else(|| KongError::ValidationError("时间戳字段必须是数字".to_string()))?;
            Ok(SqlParam::TimestampEpoch(Some(n)))
        }
        ColumnType::TimestampMs => {
            let n = value.as_f64().ok_or_else(|| {
                KongError::ValidationError("时间戳(ms)字段必须是数字".to_string())
            })?;
            Ok(SqlParam::TimestampEpochMs(Some(n)))
        }
        ColumnType::Jsonb => Ok(SqlParam::Jsonb(Some(value.clone()))),
        ColumnType::JsonbArray => {
            let arr = value.as_array().ok_or_else(|| {
                KongError::ValidationError("JSONB[] 字段必须是 JSON 数组".to_string())
            })?;
            Ok(SqlParam::JsonbArray(Some(arr.clone())))
        }
        ColumnType::UuidArray => {
            let arr = value.as_array().ok_or_else(|| {
                KongError::ValidationError("UUID[] 字段必须是 JSON 数组".to_string())
            })?;
            let uuids: std::result::Result<Vec<Uuid>, _> = arr
                .iter()
                .map(|v| {
                    let s = v.as_str().ok_or_else(|| {
                        KongError::ValidationError("UUID 数组元素必须是字符串".to_string())
                    })?;
                    Uuid::parse_str(s)
                        .map_err(|e| KongError::ValidationError(format!("无效的 UUID: {}", e)))
                })
                .collect();
            Ok(SqlParam::UuidArray(Some(uuids?)))
        }
        ColumnType::TextArray => {
            let arr = value.as_array().ok_or_else(|| {
                KongError::ValidationError("数组字段必须是 JSON 数组".to_string())
            })?;
            let strings: std::result::Result<Vec<String>, _> = arr
                .iter()
                .map(|v| {
                    v.as_str().map(|s| s.to_string()).ok_or_else(|| {
                        KongError::ValidationError("数组元素必须是字符串".to_string())
                    })
                })
                .collect();
            Ok(SqlParam::TextArray(Some(strings?)))
        }
    }
}

/// Encode pagination offset token (Kong-compatible base64-encoded JSON) — 编码分页偏移量令牌（与 Kong 兼容的 base64 编码 JSON）
fn encode_offset(id: &Uuid) -> String {
    use base64::Engine;
    let json = serde_json::to_string(&[id.to_string()]).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// Decode pagination offset token — 解码分页偏移量令牌
fn decode_offset(token: &str) -> Result<Uuid> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.as_bytes())
        .map_err(|e| KongError::ValidationError(format!("无效的 offset 令牌: {}", e)))?;

    let json_str = String::from_utf8(bytes)
        .map_err(|e| KongError::ValidationError(format!("无效的 offset 编码: {}", e)))?;

    let arr: Vec<String> = serde_json::from_str(&json_str)
        .map_err(|e| KongError::ValidationError(format!("无效的 offset JSON: {}", e)))?;

    let uuid_str = arr
        .first()
        .ok_or_else(|| KongError::ValidationError("offset 数组为空".to_string()))?;

    Uuid::parse_str(uuid_str)
        .map_err(|e| KongError::ValidationError(format!("无效的 offset UUID: {}", e)))
}

// ============ Dao trait implementation — Dao trait 实现 ============

#[async_trait]
impl<T: Entity> Dao<T> for PgDao<T> {
    async fn insert(&self, entity: &T) -> Result<T> {
        let entity_json = serde_json::to_value(entity)
            .map_err(|e| KongError::SerializationError(format!("序列化失败: {}", e)))?;

        let params = entity_to_params(&entity_json, &self.schema)?;
        let table = &self.schema.table_name;

        // Build INSERT SQL — 构建 INSERT SQL
        let columns: Vec<&str> = params.iter().map(|(col, _)| col.as_str()).collect();

        // Use TO_TIMESTAMP conversion for timestamp columns — 对时间戳列使用 TO_TIMESTAMP 转换
        let placeholders_with_cast: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, (_, param))| match param {
                SqlParam::TimestampEpoch(_) | SqlParam::TimestampEpochMs(_) => {
                    format!("TO_TIMESTAMP(${}) AT TIME ZONE 'UTC'", i + 1)
                }
                _ => format!("${}", i + 1),
            })
            .collect();

        // SELECT expressions (timestamps need to be converted back to epoch) — SELECT 表达式（时间戳需要转回 epoch）
        let select_exprs = build_select_exprs(&self.schema);

        let sql = format!(
            "INSERT INTO \"{}\" ({}) VALUES ({}) RETURNING {}",
            table,
            columns
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", "),
            placeholders_with_cast.join(", "),
            select_exprs,
        );

        let mut query = sqlx::query(&sql);

        // Bind parameters — 绑定参数
        for (_, param) in &params {
            query = bind_param(query, param);
        }

        let row = query
            .fetch_one(self.db.pool())
            .await
            .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        row_to_entity(&row, &self.schema)
    }

    async fn select(&self, pk: &PrimaryKey) -> Result<Option<T>> {
        let table = &self.schema.table_name;
        let select_exprs = build_select_exprs(&self.schema);

        let (where_clause, pk_value) = match pk {
            PrimaryKey::Id(id) => ("\"id\" = $1".to_string(), PkValue::Uuid(*id)),
            PrimaryKey::EndpointKey(key) => {
                if let Some(ek) = T::endpoint_key() {
                    // Try parsing as UUID first — 先尝试作为 UUID 解析
                    if let Ok(uuid) = Uuid::parse_str(key) {
                        ("\"id\" = $1".to_string(), PkValue::Uuid(uuid))
                    } else {
                        (format!("\"{}\" = $1", ek), PkValue::Text(key.clone()))
                    }
                } else {
                    return Err(KongError::ValidationError(
                        "该实体不支持端点键查询".to_string(),
                    ));
                }
            }
        };

        let sql = format!(
            "SELECT {} FROM \"{}\" WHERE {} LIMIT 1",
            select_exprs, table, where_clause
        );

        let query = match &pk_value {
            PkValue::Uuid(id) => sqlx::query(&sql).bind(id),
            PkValue::Text(key) => sqlx::query(&sql).bind(key),
        };

        let row = query
            .fetch_optional(self.db.read_pool())
            .await
            .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        match row {
            Some(row) => Ok(Some(row_to_entity(&row, &self.schema)?)),
            None => Ok(None),
        }
    }

    async fn page(&self, params: &PageParams) -> Result<Page<T>> {
        let table = &self.schema.table_name;
        let select_exprs = build_select_exprs(&self.schema);
        let limit = params.size + 1; // Fetch one extra to determine if there's a next page — 多取一条用于判断是否有下一页

        let (sql, offset_uuid) = if let Some(ref offset_token) = params.offset {
            let offset_id = decode_offset(offset_token)?;
            let mut where_parts = vec!["\"id\" > $1".to_string()];

            // Tag filtering — 标签过滤
            if let Some(ref tags) = params.tags {
                if !tags.is_empty() {
                    where_parts.push(format!("\"tags\" @> $2::text[]"));
                }
            }

            let sql = format!(
                "SELECT {} FROM \"{}\" WHERE {} ORDER BY \"id\" ASC LIMIT {}",
                select_exprs,
                table,
                where_parts.join(" AND "),
                limit
            );
            (sql, Some(offset_id))
        } else {
            let mut where_parts = Vec::new();

            if let Some(ref tags) = params.tags {
                if !tags.is_empty() {
                    where_parts.push("\"tags\" @> $1::text[]".to_string());
                }
            }

            let where_clause = if where_parts.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", where_parts.join(" AND "))
            };

            let sql = format!(
                "SELECT {} FROM \"{}\"{} ORDER BY \"id\" ASC LIMIT {}",
                select_exprs, table, where_clause, limit
            );
            (sql, None)
        };

        let mut query = sqlx::query(&sql);

        if let Some(offset_id) = offset_uuid {
            query = query.bind(offset_id);
        }

        if let Some(ref tags) = params.tags {
            if !tags.is_empty() {
                query = query.bind(tags);
            }
        }

        let rows = query
            .fetch_all(self.db.read_pool())
            .await
            .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        let has_next = rows.len() > params.size;
        let rows = if has_next {
            &rows[..params.size]
        } else {
            &rows[..]
        };

        let mut data: Vec<T> = Vec::with_capacity(rows.len());
        for row in rows {
            data.push(row_to_entity(row, &self.schema)?);
        }

        let offset = if has_next {
            data.last().map(|entity| encode_offset(&entity.id()))
        } else {
            None
        };

        Ok(Page {
            data,
            offset: offset.clone(),
            next: offset.map(|o| format!("/{entity}?offset={o}", entity = T::table_name())),
        })
    }

    async fn update(&self, pk: &PrimaryKey, partial: &Value) -> Result<T> {
        let obj = partial
            .as_object()
            .ok_or_else(|| KongError::ValidationError("更新数据必须是 JSON 对象".to_string()))?;

        if obj.is_empty() {
            return Err(KongError::ValidationError("更新数据不能为空".to_string()));
        }

        let table = &self.schema.table_name;
        let select_exprs = build_select_exprs(&self.schema);

        // Build SET clause (only includes provided fields) — 构建 SET 子句（只包含提供的字段）
        let mut set_parts = Vec::new();
        let mut bind_params: Vec<SqlParam> = Vec::new();
        let mut param_idx = 1;

        for (key, value) in obj {
            if key == "id" || key == "created_at" {
                continue; // Do not allow updating primary key and created_at — 不允许更新主键和创建时间
            }

            if let Some(col) = self.schema.find_column(key) {
                let param = json_to_sql_param(value, &col.col_type)?;
                let placeholder = match &param {
                    SqlParam::TimestampEpoch(_) | SqlParam::TimestampEpochMs(_) => {
                        format!("TO_TIMESTAMP(${}) AT TIME ZONE 'UTC'", param_idx)
                    }
                    _ => format!("${}", param_idx),
                };
                set_parts.push(format!("\"{}\" = {}", col.db_column, placeholder));
                bind_params.push(param);
                param_idx += 1;
            }
        }

        // Auto-update updated_at — 自动更新 updated_at
        if self.schema.find_column("updated_at").is_some() && !obj.contains_key("updated_at") {
            set_parts.push(format!(
                "\"updated_at\" = CURRENT_TIMESTAMP AT TIME ZONE 'UTC'"
            ));
        }

        if set_parts.is_empty() {
            return Err(KongError::ValidationError("没有可更新的字段".to_string()));
        }

        // WHERE clause — WHERE 子句
        let where_clause = match pk {
            PrimaryKey::Id(id) => {
                bind_params.push(SqlParam::Uuid(Some(*id)));
                format!("\"id\" = ${}", param_idx)
            }
            PrimaryKey::EndpointKey(key) => {
                if let Ok(uuid) = Uuid::parse_str(key) {
                    bind_params.push(SqlParam::Uuid(Some(uuid)));
                    format!("\"id\" = ${}", param_idx)
                } else if let Some(ek) = T::endpoint_key() {
                    bind_params.push(SqlParam::Text(Some(key.clone())));
                    format!("\"{}\" = ${}", ek, param_idx)
                } else {
                    return Err(KongError::ValidationError(
                        "该实体不支持端点键查询".to_string(),
                    ));
                }
            }
        };

        let sql = format!(
            "UPDATE \"{}\" SET {} WHERE {} RETURNING {}",
            table,
            set_parts.join(", "),
            where_clause,
            select_exprs
        );

        let mut query = sqlx::query(&sql);
        for param in &bind_params {
            query = bind_param(query, param);
        }

        let row = query
            .fetch_optional(self.db.pool())
            .await
            .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        match row {
            Some(row) => row_to_entity(&row, &self.schema),
            None => Err(KongError::NotFound {
                entity_type: T::table_name().to_string(),
                id: format!("{:?}", pk),
            }),
        }
    }

    async fn upsert(&self, pk: &PrimaryKey, entity: &T) -> Result<T> {
        let entity_json = serde_json::to_value(entity)
            .map_err(|e| KongError::SerializationError(format!("序列化失败: {}", e)))?;

        let params = entity_to_params(&entity_json, &self.schema)?;
        let table = &self.schema.table_name;
        let select_exprs = build_select_exprs(&self.schema);

        let columns: Vec<&str> = params.iter().map(|(col, _)| col.as_str()).collect();

        let placeholders_with_cast: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, (_, param))| match param {
                SqlParam::TimestampEpoch(_) | SqlParam::TimestampEpochMs(_) => {
                    format!("TO_TIMESTAMP(${}) AT TIME ZONE 'UTC'", i + 1)
                }
                _ => format!("${}", i + 1),
            })
            .collect();

        // ON CONFLICT update all non-primary-key columns — ON CONFLICT 更新所有非主键列
        let update_sets: Vec<String> = params
            .iter()
            .filter(|(col, _)| col != "id")
            .map(|(col, _)| format!("\"{}\" = EXCLUDED.\"{}\"", col, col))
            .collect();

        // Determine conflict constraint — 确定冲突约束
        let conflict_column = match pk {
            PrimaryKey::Id(_) => "id".to_string(),
            PrimaryKey::EndpointKey(_) => T::endpoint_key().unwrap_or("id").to_string(),
        };

        let sql = format!(
            "INSERT INTO \"{}\" ({}) VALUES ({}) ON CONFLICT (\"{}\") DO UPDATE SET {} RETURNING {}",
            table,
            columns
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", "),
            placeholders_with_cast.join(", "),
            conflict_column,
            update_sets.join(", "),
            select_exprs,
        );

        let mut query = sqlx::query(&sql);
        for (_, param) in &params {
            query = bind_param(query, param);
        }

        let row = query
            .fetch_one(self.db.pool())
            .await
            .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        row_to_entity(&row, &self.schema)
    }

    async fn delete(&self, pk: &PrimaryKey) -> Result<()> {
        let table = &self.schema.table_name;

        let (where_clause, pk_value) = match pk {
            PrimaryKey::Id(id) => ("\"id\" = $1".to_string(), PkValue::Uuid(*id)),
            PrimaryKey::EndpointKey(key) => {
                if let Ok(uuid) = Uuid::parse_str(key) {
                    ("\"id\" = $1".to_string(), PkValue::Uuid(uuid))
                } else if let Some(ek) = T::endpoint_key() {
                    (format!("\"{}\" = $1", ek), PkValue::Text(key.clone()))
                } else {
                    return Err(KongError::ValidationError(
                        "该实体不支持端点键查询".to_string(),
                    ));
                }
            }
        };

        let sql = format!("DELETE FROM \"{}\" WHERE {}", table, where_clause);

        let result = match &pk_value {
            PkValue::Uuid(id) => sqlx::query(&sql).bind(id),
            PkValue::Text(key) => sqlx::query(&sql).bind(key),
        }
        .execute(self.db.pool())
        .await
        .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        if result.rows_affected() == 0 {
            return Err(KongError::NotFound {
                entity_type: T::table_name().to_string(),
                id: format!("{:?}", pk),
            });
        }

        Ok(())
    }

    async fn select_by_foreign_key(
        &self,
        foreign_key_field: &str,
        foreign_key_value: &Uuid,
        params: &PageParams,
    ) -> Result<Page<T>> {
        let table = &self.schema.table_name;
        let select_exprs = build_select_exprs(&self.schema);
        let limit = params.size + 1;

        // Find the DB column name for the foreign key (e.g. service -> service_id) — 查找外键列的 DB 名（如 service -> service_id）
        let fk_col = self
            .schema
            .find_column(foreign_key_field)
            .map(|c| c.db_column.clone())
            .unwrap_or_else(|| format!("{}_id", foreign_key_field));

        let (sql, offset_uuid) = if let Some(ref offset_token) = params.offset {
            let offset_id = decode_offset(offset_token)?;
            let sql = format!(
                "SELECT {} FROM \"{}\" WHERE \"{}\" = $1 AND \"id\" > $2 ORDER BY \"id\" ASC LIMIT {}",
                select_exprs, table, fk_col, limit
            );
            (sql, Some(offset_id))
        } else {
            let sql = format!(
                "SELECT {} FROM \"{}\" WHERE \"{}\" = $1 ORDER BY \"id\" ASC LIMIT {}",
                select_exprs, table, fk_col, limit
            );
            (sql, None)
        };

        let mut query = sqlx::query(&sql).bind(foreign_key_value);
        if let Some(offset_id) = offset_uuid {
            query = query.bind(offset_id);
        }

        let rows = query
            .fetch_all(self.db.read_pool())
            .await
            .map_err(|e| map_sqlx_error(e, T::table_name()))?;

        let has_next = rows.len() > params.size;
        let rows = if has_next {
            &rows[..params.size]
        } else {
            &rows[..]
        };

        let mut data: Vec<T> = Vec::with_capacity(rows.len());
        for row in rows {
            data.push(row_to_entity(row, &self.schema)?);
        }

        let offset = if has_next {
            data.last().map(|entity| encode_offset(&entity.id()))
        } else {
            None
        };

        Ok(Page {
            data,
            offset: offset.clone(),
            next: offset.map(|o| format!("/{entity}?offset={o}", entity = T::table_name())),
        })
    }
}

// ============ Helper types and functions — 辅助类型和函数 ============

enum PkValue {
    Uuid(Uuid),
    Text(String),
}

/// Build SELECT expressions (timestamp columns converted to numeric via EXTRACT(EPOCH FROM ...)) — 构建 SELECT 表达式（时间戳列用 EXTRACT(EPOCH FROM ...) 转为数值）
fn build_select_exprs(schema: &EntitySchema) -> String {
    schema
        .columns
        .iter()
        .map(|col| match col.col_type {
            ColumnType::Timestamp | ColumnType::TimestampMs => {
                // EXTRACT returns numeric type, explicit cast to float8 for sqlx to decode as f64 — EXTRACT 返回 numeric 类型，需显式转为 float8 以便 sqlx 解码为 f64
                format!(
                    "EXTRACT(EPOCH FROM \"{}\" AT TIME ZONE 'UTC')::float8 AS \"epoch_{}\"",
                    col.db_column, col.db_column
                )
            }
            _ => format!("\"{}\"", col.db_column),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Bind SQL parameter to query — 绑定 SQL 参数到 query
fn bind_param<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    param: &'q SqlParam,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match param {
        SqlParam::Uuid(v) => query.bind(v),
        SqlParam::Text(v) => query.bind(v),
        SqlParam::Integer(v) => query.bind(v),
        SqlParam::Float(v) => query.bind(v),
        SqlParam::Boolean(v) => query.bind(v),
        SqlParam::Jsonb(v) => query.bind(v),
        SqlParam::TextArray(v) => query.bind(v),
        SqlParam::JsonbArray(v) => query.bind(v),
        SqlParam::UuidArray(v) => query.bind(v),
        SqlParam::TimestampEpoch(v) => {
            // Bind as f64 for TO_TIMESTAMP() — 绑定 f64 用于 TO_TIMESTAMP()
            query.bind(v.map(|n| n as f64))
        }
        SqlParam::TimestampEpochMs(v) => query.bind(v),
    }
}

/// Map sqlx error to KongError — 将 sqlx 错误映射为 KongError
fn map_sqlx_error(err: sqlx::Error, entity_type: &str) -> KongError {
    match &err {
        sqlx::Error::Database(db_err) => {
            let code = db_err.code().unwrap_or_default();
            let message = db_err.message();

            match code.as_ref() {
                // 23505: unique_violation
                "23505" => KongError::UniqueViolation(format!("{}: {}", entity_type, message)),
                // 23503: foreign_key_violation
                "23503" => KongError::ForeignKeyViolation(format!("{}: {}", entity_type, message)),
                // 23502: not_null_violation
                "23502" => KongError::ValidationError(format!(
                    "{}: 必填字段为空 - {}",
                    entity_type, message
                )),
                _ => KongError::DatabaseError(format!("{}: {}", entity_type, message)),
            }
        }
        sqlx::Error::RowNotFound => KongError::NotFound {
            entity_type: entity_type.to_string(),
            id: "unknown".to_string(),
        },
        _ => KongError::DatabaseError(format!("{}: {}", entity_type, err)),
    }
}

// ============ Entity schema definitions — 各实体的 Schema 定义 ============

/// Create Schema for Service entity — 创建 Service 实体的 Schema
pub fn service_schema() -> EntitySchema {
    EntitySchema::new("services")
        .pk()
        .timestamps()
        .text_opt("name")
        .integer("retries")
        .text("protocol")
        .text("host")
        .integer("port")
        .text_opt("path")
        .integer("connect_timeout")
        .integer("write_timeout")
        .integer("read_timeout")
        .tags()
        .foreign_key("client_certificate")
        .boolean_opt("tls_verify")
        .integer_opt("tls_verify_depth")
        .uuid_array("ca_certificates")
        .boolean("enabled")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Route entity — 创建 Route 实体的 Schema
pub fn route_schema() -> EntitySchema {
    EntitySchema::new("routes")
        .pk()
        .timestamps()
        .text_opt("name")
        .text_array("protocols")
        .text_array("methods")
        .text_array("hosts")
        .text_array("paths")
        .jsonb("headers")
        .integer("https_redirect_status_code")
        .integer("regex_priority")
        .boolean("strip_path")
        .text("path_handling")
        .boolean("preserve_host")
        .boolean("request_buffering")
        .boolean("response_buffering")
        .tags()
        .foreign_key("service")
        .text_array("snis")
        .jsonb_array("sources")
        .jsonb_array("destinations")
        .text_opt("expression")
        .integer_opt("priority")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Consumer entity — 创建 Consumer 实体的 Schema
pub fn consumer_schema() -> EntitySchema {
    EntitySchema::new("consumers")
        .pk()
        .timestamps()
        .text_opt("username")
        .text_opt("custom_id")
        .tags()
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Upstream entity — 创建 Upstream 实体的 Schema
pub fn upstream_schema() -> EntitySchema {
    EntitySchema::new("upstreams")
        .pk()
        .timestamps()
        .text("name")
        .text("algorithm")
        .text_opt("hash_on_cookie_path")
        .text("hash_on")
        .text("hash_fallback")
        .text_opt("hash_on_header")
        .text_opt("hash_fallback_header")
        .text_opt("hash_on_cookie")
        .text_opt("hash_on_query_arg")
        .text_opt("hash_fallback_query_arg")
        .text_opt("hash_on_uri_capture")
        .text_opt("hash_fallback_uri_capture")
        .integer("slots")
        .jsonb("healthchecks")
        .tags()
        .text_opt("host_header")
        .foreign_key("client_certificate")
        .boolean("use_srv_name")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Target entity — 创建 Target 实体的 Schema
pub fn target_schema() -> EntitySchema {
    EntitySchema::new("targets")
        .pk()
        .column("created_at", "created_at", ColumnType::TimestampMs, false)
        .column("updated_at", "updated_at", ColumnType::TimestampMs, false)
        .text("target")
        .integer("weight")
        .text_opt("cache_key")
        .tags()
        .foreign_key_required("upstream")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Plugin entity — 创建 Plugin 实体的 Schema
pub fn plugin_schema() -> EntitySchema {
    EntitySchema::new("plugins")
        .pk()
        .timestamps()
        .text("name")
        .jsonb("config")
        .boolean("enabled")
        .text_opt("instance_name")
        .text_array("protocols")
        .text_opt("cache_key")
        .tags()
        .foreign_key("route")
        .foreign_key("service")
        .foreign_key("consumer")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Certificate entity — 创建 Certificate 实体的 Schema
pub fn certificate_schema() -> EntitySchema {
    EntitySchema::new("certificates")
        .pk()
        .timestamps()
        .text("cert")
        .text("key")
        .text_opt("cert_alt")
        .text_opt("key_alt")
        .tags()
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for SNI entity — 创建 Sni 实体的 Schema
pub fn sni_schema() -> EntitySchema {
    EntitySchema::new("snis")
        .pk()
        .timestamps()
        .text("name")
        .tags()
        .foreign_key_required("certificate")
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for CaCertificate entity — 创建 CaCertificate 实体的 Schema
pub fn ca_certificate_schema() -> EntitySchema {
    EntitySchema::new("ca_certificates")
        .pk()
        .timestamps()
        .text("cert")
        .text_opt("cert_digest")
        .tags()
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}

/// Create Schema for Vault entity — 创建 Vault 实体的 Schema
pub fn vault_schema() -> EntitySchema {
    EntitySchema::new("sm_vaults")
        .pk()
        .timestamps()
        .text("prefix")
        .text("name")
        .text_opt("description")
        .jsonb("config")
        .tags()
        .column("ws_id", "ws_id", ColumnType::Uuid, true)
}
