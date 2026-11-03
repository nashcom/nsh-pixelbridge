//! Swagger UI at GET /docs, showing /openapi.json with a "Try it out" button.
//!
//! Two build-time modes (cargo feature "swagger-embed", on by default):
//!   embed : the UI files are compiled into the binary and served from
//!           /docs/... - works offline, the browser contacts nobody else.
//!   cdn   : /docs loads the same Swagger UI version from jsdelivr in the
//!           viewer's browser; nothing is added to the binary.
//! The small page and init script are ours in both modes.

use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;

/// Must match VERSION in docker/fetch_swagger_ui.sh (a unit test checks).
pub const SWAGGER_UI_VERSION: &str = "5.34.1";

/// Page URLs are relative on purpose ("docs/...", "openapi.json"), so the page
/// also works when NGINX publishes the service under a path prefix.
const INIT_JS: &str = "\
window.onload = function () {
  window.ui = SwaggerUIBundle({
    url: 'openapi.json',
    dom_id: '#swagger-ui',
    deepLinking: true,
    presets: [SwaggerUIBundle.presets.apis]
  });
};
";

#[cfg(feature = "swagger-embed")]
mod embedded {
    // Fetched and hash-verified by docker/fetch_swagger_ui.sh.
    pub const CSS: &[u8] = include_bytes!("../swagger-ui/swagger-ui.css");
    pub const JS: &[u8] = include_bytes!("../swagger-ui/swagger-ui-bundle.js");
}

pub fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let router = Router::new()
        .route("/docs", get(page))
        .route("/docs/init.js", get(init_js));

    #[cfg(feature = "swagger-embed")]
    let router = router
        .route("/docs/swagger-ui.css", get(css))
        .route("/docs/swagger-ui-bundle.js", get(bundle));

    router
}

fn page_html() -> String {
    let (css, js) = if cfg!(feature = "swagger-embed") {
        ("docs/swagger-ui.css".to_string(), "docs/swagger-ui-bundle.js".to_string())
    } else {
        (
            format!("https://cdn.jsdelivr.net/npm/swagger-ui-dist@{SWAGGER_UI_VERSION}/swagger-ui.css"),
            format!("https://cdn.jsdelivr.net/npm/swagger-ui-dist@{SWAGGER_UI_VERSION}/swagger-ui-bundle.js"),
        )
    };
    format!(
        "<!doctype html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<title>NashCom PixelBridge API</title>\n\
<link rel=\"stylesheet\" href=\"{css}\">\n\
</head>\n\
<body>\n\
<div id=\"swagger-ui\"></div>\n\
<script src=\"{js}\"></script>\n\
<script src=\"docs/init.js\"></script>\n\
</body>\n\
</html>\n"
    )
}

async fn page() -> Html<String> {
    Html(page_html())
}

async fn init_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "no-cache")], INIT_JS)
}

#[cfg(feature = "swagger-embed")]
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, "public, max-age=3600")],
        embedded::CSS,
    )
}

#[cfg(feature = "swagger-embed")]
async fn bundle() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "public, max-age=3600")],
        embedded::JS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_the_fetch_script() {
        let script = include_str!("../docker/fetch_swagger_ui.sh");
        assert!(
            script.lines().any(|l| l == format!("VERSION={SWAGGER_UI_VERSION}")),
            "SWAGGER_UI_VERSION and docker/fetch_swagger_ui.sh VERSION differ"
        );
    }

    #[test]
    fn page_points_at_the_right_sources() {
        let html = page_html();
        assert!(html.contains("docs/init.js"));
        if cfg!(feature = "swagger-embed") {
            assert!(html.contains("docs/swagger-ui-bundle.js"));
            assert!(!html.contains("cdn.jsdelivr.net"));
        } else {
            assert!(html.contains("cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.1/swagger-ui-bundle.js"));
        }
    }
}
