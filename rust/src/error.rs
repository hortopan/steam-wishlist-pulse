use axum::http::StatusCode;
use axum::response::IntoResponse;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Database(#[from] rusqlite::Error),

    #[error("{0}")]
    Http(#[from] reqwest::Error),

    #[error("{0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            AppError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Http(_) => StatusCode::BAD_GATEWAY,
            AppError::Json(_) => StatusCode::BAD_REQUEST,
            AppError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            axum::Json(serde_json::json!({"error": self.to_string()})),
        )
            .into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
