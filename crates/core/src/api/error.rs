//! 统一错误。序列化为与 ApiResponse 一致的结构,并映射到 HTTP 状态码。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

// 部分变体(如 NotFound)为后续业务接口预留
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    code: i32,
    message: String,
    data: Option<()>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, 400),
            AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, 401),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, 404),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, 500),
        };
        let body = ErrorBody {
            code,
            message: self.to_string(),
            data: None,
        };
        (status, Json(body)).into_response()
    }
}

// 便于用 ? 直接传播 SeaORM 错误
impl From<sea_orm::DbErr> for AppError {
    fn from(e: sea_orm::DbErr) -> Self {
        AppError::Internal(format!("数据库错误: {e}"))
    }
}
