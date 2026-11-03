use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::AppError;

/// The built frontend (`web/dist`), compiled into the binary.
#[derive(RustEmbed)]
#[folder = "../web/dist"]
#[allow_missing = true]
struct Assets;

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite puts content hashes in asset names, so those can be cached forever.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut res = (StatusCode::OK, file.data).into_response();
    let headers = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime.as_ref()) {
        headers.insert(CONTENT_TYPE, v);
    }
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(res)
}

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return AppError::not_found("no such endpoint").into_response();
    }
    if let Some(res) = serve(path) {
        return res;
    }
    // `/` and unknown routes without a file extension belong to the SPA.
    if !path.rsplit('/').next().unwrap_or_default().contains('.') {
        if let Some(res) = serve("index.html") {
            return res;
        }
        return (
            StatusCode::NOT_FOUND,
            "frontend not built: run `npm run build` in web/",
        )
            .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
