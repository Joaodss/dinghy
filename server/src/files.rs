//! File browsing and editing, confined to the configured roots.
//!
//! Every path is resolved and canonicalized before it is checked against its
//! root, so `..`, absolute paths and symlinks that point outside are refused.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::header::{ETAG, IF_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::config::FileRoot;
use crate::error::{AppError, AppResult};

pub const MAX_FILE_SIZE: u64 = 1024 * 1024;
const MASK: &str = "••••••••";

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

/// Splits a relative path into plain components, refusing anything that could
/// climb out of the root before the filesystem is even touched.
pub fn clean_relative(rel: &str) -> AppResult<Vec<String>> {
    if rel.contains('\0') || rel.contains('\\') || rel.contains(':') {
        return Err(AppError::bad_request("invalid path"));
    }
    if rel.starts_with('/') {
        return Err(AppError::bad_request("absolute paths are not allowed"));
    }
    let mut parts = Vec::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return Err(AppError::bad_request("path escapes its root")),
        }
    }
    Ok(parts)
}

/// `std::fs::canonicalize`, minus the `\\?\` prefix Windows adds (local development only).
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(plain) = canonical.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
        && !plain.starts_with(r"UNC\")
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(canonical)
}

fn ensure_inside(root: &Path, path: &Path) -> AppResult<()> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(AppError::forbidden("path escapes its root"))
    }
}

/// Resolves an existing path, following symlinks, and checks it stays in the root.
pub fn resolve_existing(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let parts = clean_relative(rel)?;
    let joined = parts
        .iter()
        .fold(root.to_path_buf(), |p, part| p.join(part));
    let canonical = canonicalize(&joined)?;
    ensure_inside(root, &canonical)?;
    Ok(canonical)
}

/// Resolves a directory entry without following the final component, for
/// creating and renaming. The parent must resolve inside the root.
pub fn resolve_entry(root: &Path, rel: &str) -> AppResult<PathBuf> {
    let mut parts = clean_relative(rel)?;
    let name = parts
        .pop()
        .ok_or_else(|| AppError::bad_request("path must name an entry inside the root"))?;
    let parent = resolve_existing(root, &parts.join("/"))?;
    if !parent.is_dir() {
        return Err(AppError::bad_request("parent is not a directory"));
    }
    Ok(parent.join(name))
}

pub fn etag_for(content: &[u8]) -> String {
    format!("\"{}\"", hex::encode(&Sha256::digest(content)[..16]))
}

/// `.env`, `prod.env`, `.env.local`: files whose values are secrets.
pub fn is_env_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name == ".env" || name.ends_with(".env") || name.starts_with(".env.")
}

/// Replaces every value in an env file with a mask, keeping keys and comments.
pub fn mask_env(content: &str) -> String {
    content
        .split_inclusive('\n')
        .map(|line| {
            let body = line.trim_end_matches(['\n', '\r']);
            let ending = &line[body.len()..];
            let trimmed = body.trim_start();
            match body.split_once('=') {
                Some((key, value)) if !trimmed.starts_with('#') => {
                    let masked = if value.trim().is_empty() { "" } else { MASK };
                    format!("{key}={masked}{ending}")
                }
                _ => line.to_string(),
            }
        })
        .collect()
}

fn root<'a>(state: &'a AppState, name: &str) -> AppResult<&'a FileRoot> {
    state
        .config
        .file_root(name)
        .ok_or_else(|| AppError::not_found(format!("unknown root {name:?}")))
}

pub async fn list_roots(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let names: Vec<&str> = state
        .config
        .file_roots
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    Json(json!({ "roots": names }))
}

#[derive(Deserialize)]
pub struct PathQuery {
    root: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    reveal: bool,
}

#[derive(Serialize)]
pub struct Entry {
    name: String,
    /// `dir`, `file` or `symlink` (a symlink that leaves the root or dangles).
    kind: &'static str,
    size: u64,
    modified: Option<u64>,
}

pub async fn list_dir(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> AppResult<Json<Vec<Entry>>> {
    let root = root(&state, &q.root)?.path.clone();
    blocking(move || {
        let dir = resolve_existing(&root, &q.path)?;
        if !dir.is_dir() {
            return Err(AppError::bad_request("not a directory"));
        }
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Classify by what a symlink points to, but only when it stays in the root.
            let target = canonicalize(entry.path())
                .ok()
                .filter(|t| t.starts_with(&root));
            let meta = target.as_ref().and_then(|t| std::fs::metadata(t).ok());
            let kind = match &meta {
                Some(m) if m.is_dir() => "dir",
                Some(_) => "file",
                None => "symlink",
            };
            entries.push(Entry {
                name,
                kind,
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
            });
        }
        entries.sort_by(|a, b| {
            (a.kind != "dir")
                .cmp(&(b.kind != "dir"))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(Json(entries))
    })
    .await
}

fn read_text(path: &Path) -> AppResult<(String, String)> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(AppError::bad_request("not a file"));
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "files over 1 MB are not opened",
        ));
    }
    let bytes = std::fs::read(path)?;
    let etag = etag_for(&bytes);
    if bytes.contains(&0) {
        return Err(AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "binary files are not opened",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "binary files are not opened",
        )
    })?;
    Ok((text, etag))
}

fn with_etag(body: impl IntoResponse, etag: &str) -> Response {
    let mut res = body.into_response();
    if let Ok(v) = HeaderValue::from_str(etag) {
        res.headers_mut().insert(ETAG, v);
    }
    res
}

pub async fn read_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> AppResult<Response> {
    let root = root(&state, &q.root)?.path.clone();
    blocking(move || {
        let path = resolve_existing(&root, &q.path)?;
        let (content, etag) = read_text(&path)?;
        let env = is_env_file(&path);
        let masked = env && !q.reveal;
        if env && q.reveal {
            tracing::info!(root = %q.root, path = %q.path, "revealed env file");
        }
        let content = if masked { mask_env(&content) } else { content };
        Ok(with_etag(
            Json(json!({ "content": content, "etag": etag, "masked": masked, "env": env })),
            &etag,
        ))
    })
    .await
}

/// Writes `content` next to `path` and renames it into place, so readers never
/// see a half-written file.
fn write_atomic(path: &Path, content: &[u8]) -> AppResult<()> {
    let dir = path
        .parent()
        .ok_or_else(|| AppError::bad_request("no parent directory"))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(
        ".{name}.dinghy-{}.tmp",
        hex::encode(rand::random::<[u8; 6]>())
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

pub async fn write_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
    headers: HeaderMap,
    body: String,
) -> AppResult<Response> {
    let root = root(&state, &q.root)?.path.clone();
    let expected = headers
        .get(IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| {
            AppError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "If-Match header is required",
            )
        })?;
    if body.len() as u64 > MAX_FILE_SIZE {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "files over 1 MB are not saved",
        ));
    }
    // Serializes writes so the check-then-rename below cannot interleave.
    let _guard = state.write_lock.lock().await;
    blocking(move || {
        let path = resolve_existing(&root, &q.path)?;
        let (current, current_etag) = read_text(&path)?;
        if current_etag != expected {
            return Err(AppError::new(
                StatusCode::PRECONDITION_FAILED,
                "the file changed since it was opened",
            ));
        }
        if is_env_file(&path) && body.contains(MASK) && !current.contains(MASK) {
            return Err(AppError::bad_request(
                "refusing to save masked values; reveal the file first",
            ));
        }
        write_atomic(&path, body.as_bytes())?;
        tracing::info!(root = %q.root, path = %q.path, bytes = body.len(), "saved file");
        let etag = etag_for(body.as_bytes());
        Ok(with_etag(Json(json!({ "etag": etag })), &etag))
    })
    .await
}

#[derive(Deserialize)]
pub struct CreateRequest {
    root: String,
    path: String,
    #[serde(default)]
    dir: bool,
}

pub async fn create_entry(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRequest>,
) -> AppResult<Response> {
    let root = root(&state, &req.root)?.path.clone();
    let _guard = state.write_lock.lock().await;
    blocking(move || {
        let path = resolve_entry(&root, &req.path)?;
        if req.dir {
            std::fs::create_dir(&path)?;
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
        }
        tracing::info!(root = %req.root, path = %req.path, dir = req.dir, "created entry");
        Ok((StatusCode::CREATED, Json(json!({ "etag": etag_for(b"") }))).into_response())
    })
    .await
}

#[derive(Deserialize)]
pub struct RenameRequest {
    root: String,
    from: String,
    to: String,
}

pub async fn rename_entry(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenameRequest>,
) -> AppResult<Response> {
    let root = root(&state, &req.root)?.path.clone();
    let _guard = state.write_lock.lock().await;
    blocking(move || {
        let from = resolve_entry(&root, &req.from)?;
        let to = resolve_entry(&root, &req.to)?;
        std::fs::symlink_metadata(&from)?;
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(AppError::conflict("destination already exists"));
        }
        std::fs::rename(&from, &to)?;
        tracing::info!(root = %req.root, from = %req.from, to = %req.to, "renamed entry");
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = canonicalize(dir.path()).unwrap().join("root");
        std::fs::create_dir_all(root.join("caddy")).unwrap();
        std::fs::write(root.join("caddy/compose.yaml"), "services: {}\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "outside").unwrap();
        (dir, root)
    }

    #[test]
    fn refuses_traversal() {
        let (_dir, root) = setup();
        assert!(resolve_existing(&root, "caddy/compose.yaml").is_ok());
        assert!(resolve_existing(&root, "./caddy/../caddy/compose.yaml").is_err());
        for bad in [
            "../secret.txt",
            "caddy/../../secret.txt",
            "/etc/passwd",
            "C:/Windows",
            "..\\secret.txt",
            "a\0b",
        ] {
            assert!(resolve_existing(&root, bad).is_err(), "{bad:?}");
            assert!(resolve_entry(&root, bad).is_err(), "{bad:?}");
        }
        // A query string with %2e%2e reaches us already decoded as "..".
        assert!(resolve_existing(&root, "%2e%2e/secret.txt").is_err());
        assert!(resolve_entry(&root, "").is_err());
        assert_eq!(resolve_existing(&root, "").unwrap(), root);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_out_of_root() {
        let (dir, root) = setup();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("updir")).unwrap();
        std::os::unix::fs::symlink(root.join("caddy/compose.yaml"), root.join("inside")).unwrap();
        assert_eq!(
            resolve_existing(&root, "link").unwrap_err().status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            resolve_existing(&root, "updir/secret.txt")
                .unwrap_err()
                .status,
            StatusCode::FORBIDDEN
        );
        assert!(resolve_entry(&root, "updir/new.txt").is_err());
        assert!(resolve_existing(&root, "inside").is_ok());
    }

    #[test]
    fn masks_env_values() {
        let env = "# comment\nexport TOKEN=abc123\nEMPTY=\nURL=http://x?a=b\r\n\nnot a pair\n";
        let masked = mask_env(env);
        assert_eq!(
            masked,
            format!("# comment\nexport TOKEN={MASK}\nEMPTY=\nURL={MASK}\r\n\nnot a pair\n")
        );
        assert!(!masked.contains("abc123"));
        assert!(is_env_file(Path::new("/x/.env")));
        assert!(is_env_file(Path::new("prod.env")));
        assert!(is_env_file(Path::new(".env.local")));
        assert!(!is_env_file(Path::new("compose.yaml")));
        assert!(!is_env_file(Path::new("environment.yaml")));
    }

    #[test]
    fn reads_text_only() {
        let (_dir, root) = setup();
        std::fs::write(root.join("bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(root.join("big"), vec![b'a'; MAX_FILE_SIZE as usize + 1]).unwrap();
        assert_eq!(
            read_text(&root.join("bin")).unwrap_err().status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            read_text(&root.join("big")).unwrap_err().status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let (text, etag) = read_text(&root.join("caddy/compose.yaml")).unwrap();
        assert_eq!(text, "services: {}\n");
        assert_eq!(etag, etag_for(b"services: {}\n"));
    }

    #[test]
    fn atomic_write_replaces_content() {
        let (_dir, root) = setup();
        let path = root.join("caddy/compose.yaml");
        write_atomic(&path, b"services:\n  a: {}\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "services:\n  a: {}\n"
        );
        let leftovers: Vec<_> = std::fs::read_dir(root.join("caddy"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
