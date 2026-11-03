//! HTTP-level tests. They drive the real router in-process (no socket needed)
//! with tower's `oneshot`, so they exercise parameter parsing, limits, error
//! bodies and the conversion core together.
//!
//! SVG text needs fonts. Point PIXELBRIDGE_TEST_FONT_DIR at a directory with
//! DejaVu fonts (the build container installs them to /usr/share/fonts/dejavu);
//! the text test is skipped with a message when none are found.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bytes::Bytes;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use nsh_pixelbridge::api::{self, AppState};
use nsh_pixelbridge::config::Config;
use nsh_pixelbridge::convert;
use tower::ServiceExt;

const SIMPLE_SVG: &[u8] = include_bytes!("../testdata/simple.svg");
const TRANSPARENT_SVG: &[u8] = include_bytes!("../testdata/transparent.svg");
const TEXT_SVG: &[u8] = include_bytes!("../testdata/text.svg");
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

// ------------------------------------------------------------- helpers

fn font_dir() -> String {
    std::env::var("PIXELBRIDGE_TEST_FONT_DIR").unwrap_or_else(|_| "/usr/share/fonts/dejavu".to_string())
}

fn default_config() -> Config {
    Config::from_lookup(|_| None).unwrap()
}

fn state_for(config: Config) -> Arc<AppState> {
    AppState::new(config, convert::load_fonts(Path::new(&font_dir())))
}

fn state() -> Arc<AppState> {
    state_for(default_config())
}

async fn call(state: &Arc<AppState>, request: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let response = api::router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, body)
}

fn post_binary(query: &str, content_type: Option<&str>, body: &[u8]) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(format!("/v1/image/convert?{query}"));
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(Body::from(body.to_vec())).unwrap()
}

/// The options (format, width, ...) go in the query string, the image in the JSON body.
fn post_json(query: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/image/convert-json?{query}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn decode(bytes: &[u8]) -> RgbaImage {
    image::load_from_memory(bytes).expect("response is a decodable image").to_rgba8()
}

fn error_code(body: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(body).expect("error body is JSON");
    value["error"].as_str().expect("error field").to_string()
}

fn png_bytes(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba(color)));
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
    out
}

async fn convert_ok(query: &str, svg: &[u8]) -> (HeaderMap, Bytes) {
    let (status, headers, body) = call(&state(), post_binary(query, Some("image/svg+xml"), svg)).await;
    assert_eq!(status, StatusCode::OK, "body: {}", String::from_utf8_lossy(&body));
    (headers, body)
}

// ------------------------------------------------------ SVG -> PNG basics

#[tokio::test]
async fn svg_to_png_default_size() {
    let (headers, body) = convert_ok("format=png", SIMPLE_SVG).await;
    assert_eq!(&body[..8], &PNG_SIGNATURE);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers["x-image-width"], "100");
    assert_eq!(headers["x-image-height"], "50");
    let img = decode(&body);
    assert_eq!(img.dimensions(), (100, 50));
    assert_eq!(img.get_pixel(2, 2).0, [0x33, 0x66, 0xcc, 255]);
}

#[tokio::test]
async fn sizing_width_height_and_both() {
    let (_, body) = convert_ok("format=png&width=200", SIMPLE_SVG).await;
    assert_eq!(decode(&body).dimensions(), (200, 100));

    let (_, body) = convert_ok("format=png&height=25", SIMPLE_SVG).await;
    assert_eq!(decode(&body).dimensions(), (50, 25));

    // both: fitted inside the box, never stretched
    let (_, body) = convert_ok("format=png&width=200&height=200", SIMPLE_SVG).await;
    assert_eq!(decode(&body).dimensions(), (200, 100));
    let (_, body) = convert_ok("format=png&width=40&height=40", SIMPLE_SVG).await;
    assert_eq!(decode(&body).dimensions(), (40, 20));
}

#[tokio::test]
async fn svg_without_declared_content_type_is_detected() {
    let (status, _, body) = call(&state(), post_binary("format=png", None, SIMPLE_SVG)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..8], &PNG_SIGNATURE);
}

// ------------------------------------------------- transparency handling

#[tokio::test]
async fn transparent_svg_keeps_alpha_in_png() {
    let (_, body) = convert_ok("format=png", TRANSPARENT_SVG).await;
    let img = decode(&body);
    assert_eq!(img.get_pixel(0, 0).0[3], 0, "corner must stay transparent");
    assert_eq!(img.get_pixel(20, 20).0, [255, 0, 0, 255]);
}

#[tokio::test]
async fn transparent_svg_to_jpeg_is_flattened_on_white() {
    let (headers, body) = convert_ok("format=jpeg", TRANSPARENT_SVG).await;
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(&body[..3], &[0xFF, 0xD8, 0xFF]);
    let corner = decode(&body).get_pixel(0, 0).0;
    assert!(corner[0] > 250 && corner[1] > 250 && corner[2] > 250, "corner is {corner:?}, expected white");
}

#[tokio::test]
async fn background_color_is_applied() {
    let (_, body) = convert_ok("format=png&background=%2300ff00", TRANSPARENT_SVG).await;
    assert_eq!(decode(&body).get_pixel(0, 0).0, [0, 255, 0, 255]);

    let (_, body) = convert_ok("format=jpeg&background=%23000000&quality=95", TRANSPARENT_SVG).await;
    let corner = decode(&body).get_pixel(0, 0).0;
    assert!(corner[0] < 5 && corner[1] < 5 && corner[2] < 5, "corner is {corner:?}, expected black");
}

// ------------------------------------------------------------ raster input

#[tokio::test]
async fn raster_conversions() {
    // PNG -> JPEG
    let source = png_bytes(60, 30, [10, 200, 30, 255]);
    let (status, _, body) = call(&state(), post_binary("format=jpeg", Some("image/png"), &source)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..3], &[0xFF, 0xD8, 0xFF]);

    // JPEG -> PNG, resized to half
    let (status, _, png) = call(&state(), post_binary("format=png&width=30", Some("image/jpeg"), &body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&png[..8], &PNG_SIGNATURE);
    assert_eq!(decode(&png).dimensions(), (30, 15));
}

#[tokio::test]
async fn raster_over_pixel_limit_is_refused_before_decoding() {
    let mut config = default_config();
    config.max_pixels = 1000;
    let source = png_bytes(100, 100, [0, 0, 0, 255]);
    let (status, _, body) = call(&state_for(config), post_binary("format=png", None, &source)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&body), "image_too_large");
}

// -------------------------------------------------------------------- text

#[tokio::test]
async fn svg_text_uses_bundled_fonts() {
    if !Path::new(&font_dir()).exists() {
        eprintln!("SKIPPED: no fonts in {} (set PIXELBRIDGE_TEST_FONT_DIR)", font_dir());
        return;
    }
    let (_, body) = convert_ok("format=png", TEXT_SVG).await;
    let img = decode(&body);
    let dark = img.pixels().filter(|p| p.0[0] < 100 && p.0[1] < 100 && p.0[2] < 100).count();
    assert!(dark > 50, "black text pixels expected, found {dark}");
    let red = img.pixels().filter(|p| p.0[0] > 150 && p.0[1] < 80 && p.0[2] < 80).count();
    assert!(red > 50, "red unicode text pixels expected, found {red}");
}

// ----------------------------------------------------------------- errors

#[tokio::test]
async fn bad_requests_are_rejected_with_json_errors() {
    let s = state();

    let (status, _, body) = call(&s, post_binary("format=png", Some("image/svg+xml"), b"")).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::BAD_REQUEST, "empty_body"));

    let (status, _, body) = call(&s, post_binary("format=png", Some("image/svg+xml"), b"<svg><unclosed")).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::BAD_REQUEST, "invalid_svg"));

    let (status, _, body) = call(&s, post_binary("format=png", None, b"just some text")).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"));

    let (status, _, body) = call(&s, post_binary("format=png", Some("text/plain"), SIMPLE_SVG)).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"));

    let (status, _, body) = call(&s, post_binary("format=png", Some("image/png"), SIMPLE_SVG)).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::BAD_REQUEST, "format_mismatch"));

    let (status, _, body) = call(&s, post_binary("format=png", Some("image/png"), b"\x89PNG\r\n\x1a\nbroken")).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::BAD_REQUEST, "invalid_image"));

    for query in [
        "",                          // format missing
        "format=gif",                // unsupported output
        "format=png&width=0",        // zero
        "format=png&width=-5",       // negative
        "format=png&width=abc",      // not a number
        "format=png&widht=10",       // typo, unknown parameter
        "format=png&quality=80",     // quality is jpeg-only
        "format=jpeg&quality=101",   // out of range
        "format=jpeg&background=transparent",
        "format=png&background=red", // only hex colors
    ] {
        let (status, _, body) = call(&s, post_binary(query, Some("image/svg+xml"), SIMPLE_SVG)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "query '{query}'");
        assert_eq!(error_code(&body), "invalid_parameter", "query '{query}'");
    }
}

#[tokio::test]
async fn entity_declarations_are_refused() {
    let bomb = br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>&b;</text></svg>"#;
    let (status, _, body) = call(&state(), post_binary("format=png", None, bomb)).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::BAD_REQUEST, "unsupported_svg"));
}

// ----------------------------------------------------------------- limits

#[tokio::test]
async fn request_size_limit() {
    let mut config = default_config();
    config.max_request_bytes = 1024;
    let big = vec![b' '; 5000];
    let (status, _, body) = call(&state_for(config), post_binary("format=png", None, &big)).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"));
}

#[tokio::test]
async fn output_size_limits() {
    let s = state();
    let (status, _, body) = call(&s, post_binary("format=png&width=5000", None, SIMPLE_SVG)).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "image_too_large"));

    let (status, _, _) = call(&s, post_binary("format=png&width=4294967295", None, SIMPLE_SVG)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let mut config = default_config();
    config.max_pixels = 1000;
    let (status, _, _) = call(&state_for(config), post_binary("format=png", None, SIMPLE_SVG)).await; // 100x50 = 5000
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn saturated_service_answers_429() {
    let mut config = default_config();
    config.max_concurrent = 0; // no slot at all (the real config forbids this; the test bypasses it)
    let (status, _, body) = call(&state_for(config), post_binary("format=png", None, SIMPLE_SVG)).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::TOO_MANY_REQUESTS, "busy"));
}

// ---------------------------------------- external references stay closed

#[tokio::test]
async fn local_files_and_urls_in_svg_are_not_loaded() {
    // A real, valid PNG on disk. If the renderer followed the reference, the
    // output would show red pixels. It must stay transparent.
    let path = std::env::temp_dir().join(format!("pixelbridge-test-{}.png", std::process::id()));
    std::fs::write(&path, png_bytes(10, 10, [255, 0, 0, 255])).unwrap();
    let p = path.display();

    for href in [format!("{p}"), format!("file://{p}"), "http://127.0.0.1:9/x.png".to_string()] {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image href="{href}" xlink:href="{href}" width="10" height="10"/></svg>"#
        );
        let (status, _, body) = call(&state(), post_binary("format=png", None, svg.as_bytes())).await;
        assert_eq!(status, StatusCode::OK, "href {href}");
        assert_eq!(decode(&body).get_pixel(5, 5).0[3], 0, "href {href} must not be loaded");
    }
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn embedded_data_images_work_but_obey_the_pixel_limit() {
    let embedded = |w, h| {
        let data = STANDARD.encode(png_bytes(w, h, [255, 0, 0, 255]));
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="data:image/png;base64,{data}" width="10" height="10"/></svg>"#
        )
    };

    let (status, _, body) = call(&state(), post_binary("format=png", None, embedded(4, 4).as_bytes())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(decode(&body).get_pixel(5, 5).0, [255, 0, 0, 255]);

    let mut config = default_config();
    config.max_pixels = 10_000;
    let (status, _, body) = call(&state_for(config), post_binary("format=png", None, embedded(200, 200).as_bytes())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(decode(&body).get_pixel(5, 5).0[3], 0, "oversized embedded image must be skipped");
}

// ------------------------------------------------------------ JSON endpoint

#[tokio::test]
async fn json_endpoint_svg_text_in_base64_out() {
    let svg = String::from_utf8(SIMPLE_SVG.to_vec()).unwrap();
    let (status, headers, body) = call(&state(), post_json("format=png&width=200", serde_json::json!({ "svg": svg }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");

    let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["format"], "png");
    assert_eq!(result["content_type"], "image/png");
    assert_eq!(result["width"], 200);
    assert_eq!(result["height"], 100);

    let png = STANDARD.decode(result["data_base64"].as_str().unwrap()).unwrap();
    assert_eq!(&png[..8], &PNG_SIGNATURE);
    assert_eq!(result["size_bytes"], png.len());
    assert_eq!(decode(&png).dimensions(), (200, 100));
}

#[tokio::test]
async fn json_endpoint_base64_input_and_data_url() {
    let source = png_bytes(40, 20, [0, 0, 255, 255]);
    let b64 = STANDARD.encode(&source);

    for input in [b64.clone(), format!("data:image/png;base64,{b64}")] {
        let (status, _, body) = call(&state(), post_json("format=jpeg&quality=80", serde_json::json!({ "data_base64": input }))).await;
        assert_eq!(status, StatusCode::OK);
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["content_type"], "image/jpeg");
        assert_eq!((result["width"].as_u64(), result["height"].as_u64()), (Some(40), Some(20)));
    }
}

#[tokio::test]
async fn json_endpoint_rejects_bad_requests() {
    let s = state();
    let svg = String::from_utf8(SIMPLE_SVG.to_vec()).unwrap();

    for (query, request, code) in [
        ("format=png", serde_json::json!({}), "invalid_parameter"), // no input
        ("format=png", serde_json::json!({ "svg": svg, "data_base64": "aGk=" }), "invalid_parameter"), // both inputs
        ("", serde_json::json!({ "svg": svg }), "invalid_parameter"), // no format
        ("format=png&widht=10", serde_json::json!({ "svg": svg }), "invalid_parameter"), // misspelled option
        ("format=png", serde_json::json!({ "svg": svg, "width": 100 }), "invalid_json"), // options belong in the query
        ("format=png", serde_json::json!({ "svg": "not svg" }), "invalid_svg"),
        ("format=png", serde_json::json!({ "data_base64": "***" }), "invalid_base64"),
        ("format=png", serde_json::json!({ "data_base64": "aGk=" }), "unsupported_media_type"),
    ] {
        let (status, _, body) = call(&s, post_json(query, request.clone())).await;
        assert!(status.is_client_error(), "query '{query}' request {request}");
        assert_eq!(error_code(&body), code, "query '{query}' request {request}");
    }

    let not_json = Request::builder().method("POST").uri("/v1/image/convert-json?format=png").body(Body::from("{oops")).unwrap();
    let (status, _, body) = call(&s, not_json).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::BAD_REQUEST, "invalid_json"));
}

// ---------------------------------------------------------- small endpoints

#[tokio::test]
async fn health_openapi_and_unknown_paths() {
    let s = state();

    let (status, _, body) = call(&s, get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["status"], "ok");

    let (status, headers, body) = call(&s, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(doc["info"]["title"], "NashCom PixelBridge API");
    // The binary endpoint must describe binary bodies, not JSON.
    let request = &doc["paths"]["/v1/image/convert"]["post"]["requestBody"]["content"]["image/svg+xml"]["schema"];
    assert_eq!(request["format"], "binary");
    let response = &doc["paths"]["/v1/image/convert"]["post"]["responses"]["200"]["content"]["image/png"]["schema"];
    assert_eq!(response["format"], "binary");

    // Both endpoints list the same five options as separate query parameters
    // (that is what makes Swagger UI show one input field per option).
    for path in ["/v1/image/convert", "/v1/image/convert-json"] {
        let refs: Vec<&str> = doc["paths"][path]["post"]["parameters"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} has no parameters"))
            .iter()
            .map(|p| p["$ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs.len(), 5, "{path}");
    }
    for name in ["format", "width", "height", "background", "quality"] {
        assert_eq!(doc["components"]["parameters"][name]["in"], "query", "{name}");
    }

    let (status, _, body) = call(&s, get("/nope")).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::NOT_FOUND, "not_found"));
}

#[tokio::test]
async fn swagger_ui_page_and_assets() {
    let s = state();

    let (status, headers, body) = call(&s, get("/docs")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    let page = String::from_utf8(body.to_vec()).unwrap();
    assert!(page.contains("docs/init.js"));

    // The init script points the UI at the OpenAPI document (relative URL, so
    // it also works behind a path prefix).
    let (status, _, body) = call(&s, get("/docs/init.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("'openapi.json'"));

    #[cfg(feature = "swagger-embed")]
    {
        assert!(page.contains("docs/swagger-ui-bundle.js"));
        let (status, headers, body) = call(&s, get("/docs/swagger-ui-bundle.js")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/javascript");
        assert!(body.len() > 100_000, "embedded bundle is only {} bytes", body.len());

        let (status, headers, body) = call(&s, get("/docs/swagger-ui.css")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/css");
        assert!(body.len() > 10_000);
    }

    #[cfg(not(feature = "swagger-embed"))]
    {
        assert!(page.contains("cdn.jsdelivr.net"));
        let (status, _, _) = call(&s, get("/docs/swagger-ui-bundle.js")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "the CDN build must not serve the bundle itself");
    }
}

#[tokio::test]
async fn openapi_can_be_served_from_a_file_and_is_reread_on_every_request() {
    let path = std::env::temp_dir().join(format!("pixelbridge-openapi-{}.json", std::process::id()));
    let mut config = default_config();
    config.openapi_file = Some(path.clone());
    let s = state_for(config);

    let title = |body: &[u8]| serde_json::from_slice::<serde_json::Value>(body).unwrap()["info"]["title"].clone();

    std::fs::write(&path, r#"{"openapi":"3.0.3","info":{"title":"from file one","version":"1"},"paths":{}}"#).unwrap();
    let (status, _, body) = call(&s, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(title(&body), "from file one");

    // An edit is visible on the very next request, without a restart.
    std::fs::write(&path, r#"{"openapi":"3.0.3","info":{"title":"from file two","version":"1"},"paths":{}}"#).unwrap();
    let (_, _, body) = call(&s, get("/openapi.json")).await;
    assert_eq!(title(&body), "from file two");

    // A broken file is an error, not a silent fall-back to the built-in copy.
    std::fs::write(&path, "{ not json").unwrap();
    let (status, _, body) = call(&s, get("/openapi.json")).await;
    assert_eq!((status, error_code(&body).as_str()), (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"));

    std::fs::remove_file(&path).unwrap();
    let (status, _, _) = call(&s, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn public_url_is_advertised_in_openapi() {
    let config = Config::from_lookup(|name| (name == "PIXELBRIDGE_PUBLIC_URL").then(|| "https://img.example.com/".to_string())).unwrap();
    let (_, _, body) = call(&state_for(config), get("/openapi.json")).await;
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(doc["servers"][0]["url"], "https://img.example.com");
}
