//! One error type for everything a client can see.
//!
//! Every failure becomes `{"error":"<code>","message":"<text>"}` with a fitting
//! HTTP status. Messages are written for the client: no paths, no parser
//! internals, no payload content. Internal details go to the log instead.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into() }
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn invalid_parameter(message: impl Into<String>) -> Self {
        Self::bad_request("invalid_parameter", message)
    }

    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", message)
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", message)
    }

    pub fn image_too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "image_too_large", message)
    }

    pub fn busy() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "all conversion slots are in use, retry shortly",
        )
    }

    pub fn timeout() -> Self {
        Self::new(StatusCode::GATEWAY_TIMEOUT, "timeout", "conversion took too long")
    }

    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "internal error")
    }
}

/// Stored in the response extensions (never sent to the client).
#[derive(Clone)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub message: String,
}

// Turning the error into an HTTP response is all axum needs to let handlers
// return `Result<_, ApiError>` and use `?`.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.code, "message": self.message });
        let mut response = (
            self.status,
            [(header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response();
        // Carried along so the logging middleware in api.rs can write ONE line
        // with the request's method and path next to the error.
        response.extensions_mut().insert(ErrorInfo { code: self.code, message: self.message });
        response
    }
}
