use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use systemdeck_core::api::ErrorBody;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorBody { error: self.message })).into_response()
    }
}

/// Unexpected failures are logged and reported as a generic 500.
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(err: E) -> Self {
        let err = err.into();
        tracing::error!("{err:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
    }
}
