//! HTTP layer: routing, parameter parsing, limits, and the worker hand-off.
//!
//! Two conversion endpoints share one core (convert.rs):
//!   POST /v1/image/convert        raw bytes in, raw image out   (curl, scripts)
//!   POST /v1/image/convert-json   JSON/base64 in and out        (MCP / AI clients)

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::convert::{self, ConvertOptions, Converted, InputKind};
use crate::errors::{ApiError, ErrorInfo};

const OPENAPI_TEMPLATE: &str = include_str!("../openapi.json");

pub struct AppState {
    pub config: Config,
    pub fonts: Arc<resvg::usvg::fontdb::Database>,
    /// One permit per conversion that may run at the same time.
    pub permits: Arc<Semaphore>,
    pub openapi: String,
}

impl AppState {
    pub fn new(config: Config, fonts: resvg::usvg::fontdb::Database) -> Arc<AppState> {
        Arc::new(AppState {
            permits: Arc::new(Semaphore::new(config.max_concurrent)),
            openapi: openapi_document(config.public_url.as_deref()),
            fonts: Arc::new(fonts),
            config,
        })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/v1/image/convert", post(convert_binary))
        .route("/v1/image/convert-json", post(convert_json))
        .merge(crate::docs::routes())
        .fallback(not_found)
        .layer(middleware::from_fn(log_rejections))
        .with_state(state)
}

/// One log line per rejected request: method, path (never the query string,
/// never image data), status, error code and the first part of the message.
async fn log_rejections(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let response = next.run(req).await;

    if let Some(info) = response.extensions().get::<ErrorInfo>() {
        // Messages can carry a fragment of what the client sent: cut them short.
        let shown: String = info.message.chars().take(200).collect();
        let status = response.status();
        if status.is_server_error() && status != StatusCode::GATEWAY_TIMEOUT {
            tracing::error!("{method} {path}: {} {}: {shown}", status.as_u16(), info.code);
        } else {
            tracing::warn!("{method} {path}: {} {}: {shown}", status.as_u16(), info.code);
        }
    }
    response
}

/// The OpenAPI file is embedded in the binary. The only thing patched in is
/// the public server URL, so generated MCP tools know where to call.
pub fn openapi_document(public_url: Option<&str>) -> String {
    parse_openapi(OPENAPI_TEMPLATE, public_url).expect("the embedded openapi.json is valid")
}

/// Checks that the text is a JSON object and patches in the public server URL.
/// Used for the embedded copy and for a file given by PIXELBRIDGE_OPENAPI_FILE.
pub fn parse_openapi(text: &str, public_url: Option<&str>) -> Result<String, String> {
    let mut doc: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    if !doc.is_object() {
        return Err("not a JSON object".to_string());
    }
    if let Some(url) = public_url {
        doc["servers"] = serde_json::json!([{ "url": url }]);
    }
    Ok(doc.to_string())
}

/// Startup check for PIXELBRIDGE_OPENAPI_FILE, so a wrong path or broken JSON
/// stops the service with a clear message instead of failing on the first request.
pub fn check_openapi_file(path: &std::path::Path, public_url: Option<&str>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_openapi(&text, public_url).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

// ------------------------------------------------------------ small endpoints

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn openapi(State(state): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let body = match &state.config.openapi_file {
        None => state.openapi.clone(),
        // Testing aid: read the file on every request, so an edit shows up on
        // the next browser reload. A broken file is an error, not a silent
        // fall-back to the built-in copy, so the mistake is not overlooked.
        Some(path) => {
            let text = tokio::fs::read_to_string(path).await.map_err(|e| {
                tracing::error!("cannot read {}: {e}", path.display());
                ApiError::internal()
            })?;
            parse_openapi(&text, state.config.public_url.as_deref()).map_err(|e| {
                tracing::error!("{}: {e}", path.display());
                ApiError::internal()
            })?
        }
    };
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such endpoint")
}

// -------------------------------------------------------- binary endpoint

/// The conversion options, given as query parameters on BOTH endpoints (so
/// Swagger UI shows them as separate input fields). Unknown parameters are
/// refused so a typo like `widht=100` is an error instead of being ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConvertParams {
    format: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    background: Option<String>,
    quality: Option<u8>,
}

fn options_from_query(query: Option<&str>) -> Result<ConvertOptions, ApiError> {
    let params: ConvertParams = serde_urlencoded::from_str(query.unwrap_or(""))
        .map_err(|e| ApiError::invalid_parameter(format!("query parameters are invalid: {e}")))?;
    ConvertOptions::new(
        params.format.as_deref(),
        params.width,
        params.height,
        params.background.as_deref(),
        params.quality,
    )
}

async fn convert_binary(State(state): State<Arc<AppState>>, req: Request) -> Result<Response, ApiError> {
    let (parts, body) = req.into_parts();
    let opts = options_from_query(parts.uri.query())?;

    let input = read_body(body, state.config.max_request_bytes).await?;
    if input.is_empty() {
        return Err(ApiError::bad_request("empty_body", "request body is empty"));
    }
    check_content_type(&parts.headers, &input)?;

    let out = run_conversion(&state, "convert", input, opts).await?;
    Ok(binary_response(out))
}

/// The declared Content-Type must be plausible AND agree with the bytes.
fn check_content_type(headers: &HeaderMap, input: &[u8]) -> Result<(), ApiError> {
    let kind = convert::sniff(input);
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase());

    let expected = match declared.as_deref() {
        None | Some("") | Some("application/octet-stream") => None,
        Some("image/svg+xml") => Some(InputKind::Svg),
        Some("image/png") => Some(InputKind::Png),
        Some("image/jpeg") | Some("image/jpg") => Some(InputKind::Jpeg),
        Some(other) => {
            return Err(ApiError::unsupported_media_type(format!(
                "Content-Type '{other}' is not supported, use image/svg+xml, image/png or image/jpeg"
            )))
        }
    };

    match (kind, expected) {
        (None, _) => Err(ApiError::unsupported_media_type("input must be SVG, PNG or JPEG")),
        (Some(actual), Some(wanted)) if actual != wanted => Err(ApiError::bad_request(
            "format_mismatch",
            "the Content-Type does not match the data in the request body",
        )),
        _ => Ok(()),
    }
}

fn binary_response(out: Converted) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(out.format.content_type()));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-image-width", HeaderValue::from(out.width));
    headers.insert("x-image-height", HeaderValue::from(out.height));
    (StatusCode::OK, headers, out.data).into_response()
}

// ---------------------------------------------------------- JSON endpoint

/// Body of the JSON endpoint: only the image. The options are query parameters.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    /// SVG source text (plain, not base64).
    svg: Option<String>,
    /// Base64 of an SVG, PNG or JPEG file. A `data:...;base64,` prefix is accepted.
    data_base64: Option<String>,
}

#[derive(Serialize)]
struct JsonResult {
    format: &'static str,
    content_type: &'static str,
    width: u32,
    height: u32,
    size_bytes: usize,
    data_base64: String,
}

async fn convert_json(State(state): State<Arc<AppState>>, req: Request) -> Result<Json<JsonResult>, ApiError> {
    // JSON escaping and base64 make the body larger than the image inside it,
    // so the body limit is twice the image limit; the decoded size is checked below.
    let (parts, body) = req.into_parts();
    let opts = options_from_query(parts.uri.query())?;

    let body_limit = state.config.max_request_bytes.saturating_mul(2).saturating_add(4096);
    let raw = read_body(body, body_limit).await?;

    let request: JsonRequest = serde_json::from_slice(&raw).map_err(|e| {
        ApiError::bad_request(
            "invalid_json",
            format!("body must be JSON with 'svg' or 'data_base64' (options such as format and width are query parameters): {e}"),
        )
    })?;

    let input = match (request.svg, request.data_base64) {
        (Some(svg), None) => {
            let bytes = svg.into_bytes();
            if convert::sniff(&bytes) != Some(InputKind::Svg) {
                return Err(ApiError::bad_request("invalid_svg", "svg must contain SVG source text"));
            }
            bytes
        }
        (None, Some(text)) => decode_base64(&text)?,
        _ => return Err(ApiError::invalid_parameter("provide exactly one of 'svg' or 'data_base64'")),
    };
    if input.is_empty() {
        return Err(ApiError::bad_request("empty_body", "image data is empty"));
    }
    if input.len() > state.config.max_request_bytes {
        return Err(ApiError::payload_too_large("image data exceeds the maximum request size"));
    }

    let out = run_conversion(&state, "convert-json", Bytes::from(input), opts).await?;
    Ok(Json(JsonResult {
        format: out.format.name(),
        content_type: out.format.content_type(),
        width: out.width,
        height: out.height,
        size_bytes: out.data.len(),
        data_base64: STANDARD.encode(&out.data),
    }))
}

fn decode_base64(text: &str) -> Result<Vec<u8>, ApiError> {
    let payload = text
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map(|(_, data)| data)
        .unwrap_or(text);
    // Tolerate line breaks and spaces inside the base64 text.
    let compact: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(compact)
        .map_err(|_| ApiError::bad_request("invalid_base64", "data_base64 is not valid base64"))
}

// ----------------------------------------------------------------- shared

async fn read_body(body: Body, limit: usize) -> Result<Bytes, ApiError> {
    axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| ApiError::payload_too_large("request body exceeds the limit or could not be read"))
}

/// Runs one conversion on a worker thread, bounded by the concurrency limit.
async fn run_conversion(
    state: &Arc<AppState>,
    endpoint: &'static str,
    input: Bytes,
    opts: ConvertOptions,
) -> Result<Converted, ApiError> {
    // No free slot: answer immediately instead of queueing unbounded work.
    let permit = state.permits.clone().try_acquire_owned().map_err(|_| ApiError::busy())?;

    let started = Instant::now();
    let input_len = input.len();
    // What the caller asked for, for the log line (opts moves into the worker).
    let asked = format!(
        "{} width={} height={}{}",
        opts.format.name(),
        opts.width.map_or("-".to_string(), |w| w.to_string()),
        opts.height.map_or("-".to_string(), |h| h.to_string()),
        match opts.background {
            Some(convert::Background::Transparent) => " background=transparent".to_string(),
            Some(convert::Background::Color([r, g, b])) => format!(" background=#{r:02x}{g:02x}{b:02x}"),
            None => String::new(),
        }
    );
    let worker_state = state.clone();

    let task = tokio::task::spawn_blocking(move || {
        // The permit moves into the worker and is released only when the work
        // REALLY ends. If the client already got a timeout, a runaway render
        // keeps its slot, so runaway renders cannot pile up beyond the limit.
        let _permit = permit;
        let limits = worker_state.config.limits();
        convert::convert(&input, &opts, &limits, &worker_state.fonts)
    });

    let waited = tokio::time::timeout(Duration::from_secs(state.config.timeout_seconds), task).await;
    let result = match waited {
        Err(_) => {
            tracing::warn!(
                "{endpoint}: timed out after {}s ({} bytes in, asked: {asked})",
                state.config.timeout_seconds,
                input_len
            );
            return Err(ApiError::timeout());
        }
        Ok(Err(join_error)) => {
            // The worker panicked (a bug); only this request fails.
            tracing::error!("conversion worker failed: {join_error}");
            return Err(ApiError::internal());
        }
        Ok(Ok(result)) => result,
    };

    if let Ok(out) = &result {
        tracing::info!(
            "{endpoint}: asked {asked}: {} bytes in -> {} {}x{} {} bytes out in {} ms",
            input_len,
            out.format.name(),
            out.width,
            out.height,
            out.data.len(),
            started.elapsed().as_millis()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_openapi_is_valid() {
        let doc: serde_json::Value = serde_json::from_str(&openapi_document(Some("https://img.example.com"))).unwrap();
        assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
        assert_eq!(doc["servers"][0]["url"], "https://img.example.com");
        for path in ["/health", "/v1/image/convert", "/v1/image/convert-json"] {
            assert!(doc["paths"].get(path).is_some(), "missing path {path}");
        }
    }

    #[test]
    fn openapi_version_matches_the_crate_version() {
        let doc: serde_json::Value = serde_json::from_str(OPENAPI_TEMPLATE).unwrap();
        assert_eq!(
            doc["info"]["version"],
            env!("CARGO_PKG_VERSION"),
            "openapi.json info.version must be bumped together with Cargo.toml"
        );
        assert_eq!(
            doc["paths"]["/health"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["properties"]["version"]["example"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn base64_variants() {
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("data:image/png;base64,aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("aG\nk=").unwrap(), b"hi");
        assert!(decode_base64("***").is_err());
    }
}
