use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}
impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_ERROR",
            message,
        )
    }
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "当前账号没有执行此操作的权限",
        )
    }
    pub fn missing() -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", "资源不存在")
    }
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error":{"code":self.code,"message":self.message,"request_id":format!("req_{}",Uuid::new_v4()),"field_errors":[]}}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .is_some_and(|database| database.is_unique_violation())
        {
            return Self::conflict("CONFLICT", "记录已存在或版本发生冲突");
        }
        tracing::error!(error = %error, "database operation failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "服务器数据操作失败",
        )
    }
}
impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        tracing::error!(error = %error, "storage operation failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "STORAGE_ERROR",
            "存储暂时不可用",
        )
    }
}
pub type ApiResult<T> = Result<T, ApiError>;
pub fn text<'a>(value: &'a Value, key: &str) -> ApiResult<&'a str> {
    value[key]
        .as_str()
        .filter(|text| !text.trim().is_empty() && text.len() <= 4096)
        .ok_or_else(|| ApiError::validation(format!("字段 {key} 缺失或不合法")))
}
pub fn revision(value: &Value) -> ApiResult<i64> {
    value["revision"]
        .as_i64()
        .filter(|revision| *revision > 0)
        .ok_or_else(|| ApiError::validation("缺少有效 revision"))
}
pub fn reject_unknown(value: &Value, allowed: &[&str]) -> ApiResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| ApiError::validation("需要 JSON 对象"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ApiError::validation("包含不允许修改的字段"));
    }
    Ok(())
}
