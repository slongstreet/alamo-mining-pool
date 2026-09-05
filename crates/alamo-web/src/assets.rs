//! Embedded dashboard assets built from `web/dist`.

use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
struct Dist;

const NOT_BUILT: &str = "<!doctype html><title>Alamo</title>\
<body style=\"font-family:system-ui;margin:3rem;color:#333\">\
<h1>Alamo Mining Pool</h1>\
<p>The dashboard has not been built into this binary.</p>\
<p>Run <code>npm install &amp;&amp; npm run build</code> in <code>web/</code>, \
then rebuild the daemon. The API is available under <code>/api/</code>.</p></body>";

/// Serve an embedded asset, falling back to `index.html` for SPA routes.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Dist::get(path) {
        return file_response(path, file.data.into_owned());
    }
    // Client-side routes: anything without a file extension gets the app shell.
    if !path.contains('.') {
        if let Some(index) = Dist::get("index.html") {
            return file_response("index.html", index.data.into_owned());
        }
        return Html(NOT_BUILT).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

fn file_response(path: &str, data: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        data,
    )
        .into_response()
}
