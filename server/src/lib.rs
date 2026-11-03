pub mod auth;
pub mod config;
pub mod containers;
pub mod error;
pub mod files;
pub mod ops;
pub mod stacks;
pub mod web;

use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use bollard::Docker;
use tokio::sync::{Mutex, Semaphore};

use crate::auth::{AccessVerifier, Sessions};
use crate::config::Config;
use crate::ops::Ops;

pub struct AppState {
    pub config: Config,
    pub docker: Docker,
    pub access: Option<AccessVerifier>,
    pub sessions: Sessions,
    pub ops: Arc<Ops>,
    pub write_lock: Mutex<()>,
    pub login_gate: Semaphore,
}

impl AppState {
    pub fn new(config: Config, docker: Docker, access: Option<AccessVerifier>) -> Self {
        Self {
            sessions: Sessions::new(config.session_secret.clone(), config.session_ttl),
            config,
            docker,
            access,
            ops: Arc::new(Ops::default()),
            write_lock: Mutex::new(()),
            login_gate: Semaphore::new(1),
        }
    }
}

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

async fn security_headers(req: Request, next: Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    if is_api {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

/// Every route sits behind Cloudflare Access except `/healthz`; every API route
/// except sign-in sits behind the session and CSRF checks as well.
pub fn app(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/api/stacks", get(containers::list_stacks))
        .route("/api/stacks/{stack}", get(containers::stack_detail))
        .route("/api/stacks/{stack}/{action}", post(ops::stack_action))
        .route("/api/ops/{id}/stream", get(ops::op_stream))
        .route("/api/containers/{id}/logs", get(containers::container_logs))
        .route("/api/roots", get(files::list_roots))
        .route("/api/files", get(files::list_dir))
        .route(
            "/api/files/content",
            get(files::read_file).put(files::write_file),
        )
        .route("/api/files/create", post(files::create_entry))
        .route("/api/files/rename", post(files::rename_entry))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    Router::new()
        .merge(protected)
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/session", get(auth::session))
        .fallback(web::static_handler)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_access,
        ))
        .route("/healthz", get(containers::healthz))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}
