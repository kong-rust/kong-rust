use thiserror::Error;

/// Kong-Rust 统一错误类型
#[derive(Error, Debug)]
pub enum KongError {
    /// 数据库操作错误
    #[error("数据库错误: {0}")]
    DatabaseError(String),

    /// 实体未找到
    #[error("{entity_type} 未找到: {id}")]
    NotFound {
        entity_type: String,
        id: String,
    },

    /// Schema 验证错误
    #[error("schema 验证失败: {0}")]
    ValidationError(String),

    /// 唯一约束冲突
    #[error("唯一约束冲突: {0}")]
    UniqueViolation(String),

    /// 外键约束错误
    #[error("外键约束错误: {0}")]
    ForeignKeyViolation(String),

    /// 插件执行错误
    #[error("插件错误 [{plugin_name}]: {message}")]
    PluginError {
        plugin_name: String,
        message: String,
    },

    /// Lua 运行时错误
    #[error("Lua 运行时错误: {0}")]
    LuaError(String),

    /// 配置错误
    #[error("配置错误: {0}")]
    ConfigError(String),

    /// 路由匹配错误
    #[error("路由错误: {0}")]
    RouterError(String),

    /// 上游连接错误
    #[error("上游连接错误: {0}")]
    UpstreamError(String),

    /// TLS 错误
    #[error("TLS 错误: {0}")]
    TlsError(String),

    /// 缓存错误
    #[error("缓存错误: {0}")]
    CacheError(String),

    /// 序列化/反序列化错误
    #[error("序列化错误: {0}")]
    SerializationError(String),

    /// IO 错误
    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),

    /// 内部错误
    #[error("内部错误: {0}")]
    InternalError(String),
}

/// 统一 Result 类型
pub type Result<T> = std::result::Result<T, KongError>;

impl KongError {
    /// 获取与 Kong 兼容的错误名称
    pub fn error_name(&self) -> &str {
        match self {
            KongError::NotFound { .. } => "not found",
            KongError::ValidationError(_) => "schema violation",
            KongError::UniqueViolation(_) => "unique constraint violation",
            KongError::ForeignKeyViolation(_) => "foreign key violation",
            _ => "internal error",
        }
    }

    /// 获取与 Kong 兼容的 HTTP 状态码
    pub fn status_code(&self) -> u16 {
        match self {
            KongError::NotFound { .. } => 404,
            KongError::ValidationError(_) => 400,
            KongError::UniqueViolation(_) => 409,
            KongError::ForeignKeyViolation(_) => 400,
            KongError::PluginError { .. } => 500,
            KongError::LuaError(_) => 500,
            KongError::ConfigError(_) => 400,
            _ => 500,
        }
    }

    /// 获取与 Kong 兼容的错误代码
    pub fn error_code(&self) -> u32 {
        match self {
            KongError::ValidationError(_) => 2,
            KongError::UniqueViolation(_) => 5,
            KongError::ForeignKeyViolation(_) => 4,
            KongError::NotFound { .. } => 3,
            _ => 1,
        }
    }
}

impl From<serde_json::Error> for KongError {
    fn from(e: serde_json::Error) -> Self {
        KongError::SerializationError(e.to_string())
    }
}
