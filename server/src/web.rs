use axum::http::header::{ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, VARY};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::AppError;

/// The built frontend (`web/dist`), compiled into the binary. The build also
/// writes `.br` and `.gz` copies of text assets, so nothing is compressed at
/// runtime.
#[derive(RustEmbed)]
#[folder = "../web/dist"]
#[allow_missing = true]
struct Assets;

/// Precompressed variants the client accepts, best first.
pub fn accepted_encodings(accept: &str) -> Vec<(&'static str, &'static str)> {
    let offers = |name: &str| {
        accept.split(',').any(|part| {
            let mut fields = part.split(';').map(str::trim);
            let token_matches = fields.next().is_some_and(|t| t.eq_ignore_ascii_case(name));
            let quality = fields
                .find_map(|f| f.strip_prefix("q=")?.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            token_matches && quality > 0.0
        })
    };
    [("br", ".br"), ("gzip", ".gz")]
        .into_iter()
        .filter(|(name, _)| offers(name))
        .collect()
}

fn serve(path: &str, accept: &str) -> Option<Response> {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let (file, encoding) = accepted_encodings(accept)
        .into_iter()
        .find_map(|(name, ext)| Assets::get(&format!("{path}{ext}")).map(|f| (f, Some(name))))
        .or_else(|| Assets::get(path).map(|f| (f, None)))?;
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
    headers.insert(VARY, HeaderValue::from_static("accept-encoding"));
    if let Some(name) = encoding {
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static(name));
    }
    Some(res)
}

pub async fn static_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return AppError::not_found("no such endpoint").into_response();
    }
    let accept = headers
        .get(ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Some(res) = serve(path, accept) {
        return res;
    }
    // `/` and unknown routes without a file extension belong to the SPA.
    if !path.rsplit('/').next().unwrap_or_default().contains('.') {
        if let Some(res) = serve("index.html", accept) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn names(accept: &str) -> Vec<&'static str> {
        accepted_encodings(accept)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    #[test]
    fn negotiates_encoding() {
        assert_eq!(names("gzip, deflate, br, zstd"), ["br", "gzip"]);
        assert_eq!(names("gzip"), ["gzip"]);
        assert_eq!(names("BR;q=0.5"), ["br"]);
        assert_eq!(names("br;q=0, gzip"), ["gzip"]);
        assert_eq!(names("br; q=0.0, gzip;q=0"), Vec::<&str>::new());
        assert_eq!(names("identity"), Vec::<&str>::new());
        assert_eq!(names(""), Vec::<&str>::new());
    }
}
