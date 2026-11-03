//! The three gates in front of every request: Cloudflare Access (identity), the app
//! password (session cookie) and a CSRF token for requests that change state.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::PasswordVerifier;
use argon2::password_hash::phc::PasswordHash;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use tokio::sync::{Mutex, RwLock};

use crate::AppState;
use crate::config::AccessConfig;
use crate::error::AppError;
use crate::jwt::{self, JwkSet, RsaKey};

pub const SESSION_COOKIE: &str = "dinghy_session";
pub const CSRF_HEADER: &str = "x-dinghy-csrf";
const ACCESS_HEADER: &str = "cf-access-jwt-assertion";
const ACCESS_COOKIE: &str = "CF_Authorization";
/// Minimum time between two JWKS downloads triggered by an unknown key id.
const JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

type HmacSha256 = Hmac<Sha256>;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns the value of one cookie from the request headers.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// Sessions: stateless, HMAC-signed cookies. Nothing is kept in memory.
// ---------------------------------------------------------------------------

pub struct Sessions {
    secret: Vec<u8>,
    ttl: Duration,
}

impl Sessions {
    pub fn new(secret: Vec<u8>, ttl: Duration) -> Self {
        Self { secret, ttl }
    }

    fn mac(&self, purpose: &str, payload: &str) -> HmacSha256 {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(purpose.as_bytes());
        mac.update(b"|");
        mac.update(payload.as_bytes());
        mac
    }

    fn csrf_for(&self, payload: &str) -> String {
        hex::encode(self.mac("csrf", payload).finalize().into_bytes())
    }

    /// Returns `(cookie value, csrf token)` for a new session.
    pub fn issue(&self, now: u64) -> (String, String) {
        let expires = now + self.ttl.as_secs();
        let nonce = hex::encode(rand::random::<[u8; 16]>());
        let payload = format!("{expires}.{nonce}");
        let sig = hex::encode(self.mac("session", &payload).finalize().into_bytes());
        let csrf = self.csrf_for(&payload);
        (format!("{payload}.{sig}"), csrf)
    }

    /// Checks a cookie value and returns the session's CSRF token if it is valid.
    pub fn verify(&self, value: &str, now: u64) -> Option<String> {
        let (payload, sig) = value.rsplit_once('.')?;
        let (expires, _nonce) = payload.split_once('.')?;
        let sig = hex::decode(sig).ok()?;
        self.mac("session", payload).verify_slice(&sig).ok()?;
        if expires.parse::<u64>().ok()? <= now {
            return None;
        }
        Some(self.csrf_for(payload))
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

// ---------------------------------------------------------------------------
// Cloudflare Access
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AccessClaims {
    email: Option<String>,
}

struct KeyCache {
    keys: JwkSet,
    fetched_at: Instant,
}

/// Validates Cloudflare Access JWTs against the team's public keys, which are
/// fetched on first use and again only when a token names an unknown key.
pub struct AccessVerifier {
    config: AccessConfig,
    client: Option<reqwest::Client>,
    cache: RwLock<Option<KeyCache>>,
    refresh: Mutex<()>,
}

impl AccessVerifier {
    pub fn new(config: AccessConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self {
            config,
            client: Some(client),
            cache: RwLock::new(None),
            refresh: Mutex::new(()),
        })
    }

    /// A verifier with fixed keys that never goes to the network (for tests).
    pub fn with_keys(config: AccessConfig, keys: JwkSet) -> Self {
        Self {
            config,
            client: None,
            cache: RwLock::new(Some(KeyCache {
                keys,
                fetched_at: Instant::now(),
            })),
            refresh: Mutex::new(()),
        }
    }

    fn issuer(&self) -> String {
        format!("https://{}", self.config.team_domain)
    }

    async fn key(&self, kid: &str) -> Result<RsaKey, String> {
        if let Some(key) = self.cached_key(kid).await {
            return Ok(key);
        }
        let _guard = self.refresh.lock().await;
        // Another request may have refreshed the keys while this one waited.
        if let Some(key) = self.cached_key(kid).await {
            return Ok(key);
        }
        let recently = self
            .cache
            .read()
            .await
            .as_ref()
            .is_some_and(|c| c.fetched_at.elapsed() < JWKS_REFRESH_INTERVAL);
        let Some(client) = self.client.as_ref().filter(|_| !recently) else {
            return Err("unknown signing key".into());
        };
        let url = format!("{}/cdn-cgi/access/certs", self.issuer());
        let keys: JwkSet = client
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("fetching {url}: {e}"))?
            .json()
            .await
            .map_err(|e| format!("parsing {url}: {e}"))?;
        tracing::info!(keys = keys.keys.len(), "fetched Cloudflare Access keys");
        *self.cache.write().await = Some(KeyCache {
            keys,
            fetched_at: Instant::now(),
        });
        self.cached_key(kid)
            .await
            .ok_or_else(|| "unknown signing key".into())
    }

    async fn cached_key(&self, kid: &str) -> Option<RsaKey> {
        self.cache.read().await.as_ref()?.keys.rsa_key(kid)
    }

    /// Returns the signed-in email when the token is valid and the email is allowed.
    pub async fn verify(&self, token: &str) -> Result<String, String> {
        let kid = jwt::key_id(token)?;
        let key = self.key(&kid).await?;
        let claims: AccessClaims =
            jwt::verify(token, &key, &self.issuer(), &self.config.aud, now_secs())?;

        let email = claims.email.ok_or("token has no email")?.to_lowercase();
        if self.config.allowed_emails.contains(&email) {
            Ok(email)
        } else {
            Err(format!("{email} is not allowed"))
        }
    }
}

/// The email Cloudflare Access vouched for, when Access is enabled.
#[derive(Clone)]
pub struct AccessIdentity(pub String);

fn deny(status: StatusCode, message: &str) -> Response {
    AppError::new(status, message).into_response()
}

/// Gate 1: every route except `/healthz` needs a valid Cloudflare Access token.
pub async fn require_access(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(verifier) = state.access.as_ref() else {
        return next.run(req).await;
    };
    let token = req
        .headers()
        .get(ACCESS_HEADER)
        .and_then(|v| v.to_str().ok())
        .or_else(|| cookie(req.headers(), ACCESS_COOKIE))
        .map(str::to_owned);
    let Some(token) = token else {
        return deny(StatusCode::FORBIDDEN, "missing Cloudflare Access token");
    };
    match verifier.verify(&token).await {
        Ok(email) => {
            req.extensions_mut().insert(AccessIdentity(email));
            next.run(req).await
        }
        Err(reason) => {
            tracing::warn!(%reason, path = %req.uri().path(), "rejected Cloudflare Access token");
            deny(StatusCode::FORBIDDEN, "invalid Cloudflare Access token")
        }
    }
}

/// Gates 2 and 3: a valid session cookie, plus the CSRF header on unsafe methods.
pub async fn require_session(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(csrf) =
        cookie(req.headers(), SESSION_COOKIE).and_then(|v| state.sessions.verify(v, now_secs()))
    else {
        return deny(StatusCode::UNAUTHORIZED, "not signed in");
    };
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        let sent = req
            .headers()
            .get(CSRF_HEADER)
            .map(|v| v.as_bytes())
            .unwrap_or_default();
        if !constant_time_eq(sent, csrf.as_bytes()) {
            return deny(StatusCode::FORBIDDEN, "missing or invalid CSRF token");
        }
    }
    next.run(req).await
}

fn session_cookie(state: &AppState, value: &str, max_age: u64) -> HeaderValue {
    let secure = if state.config.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ASCII")
}

#[derive(Deserialize)]
pub struct LoginRequest {
    password: String,
}

pub async fn login(State(state): State<Arc<AppState>>, Json(body): Json<LoginRequest>) -> Response {
    // One verification at a time: argon2 needs ~19 MB per attempt, and a failed
    // attempt holds the permit for a second to slow down guessing.
    let Ok(_permit) = state.login_gate.acquire().await else {
        return deny(StatusCode::SERVICE_UNAVAILABLE, "shutting down");
    };
    let hash = state.config.password_hash.clone();
    let ok = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(body.password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);

    if !ok {
        tracing::warn!("failed login attempt");
        tokio::time::sleep(Duration::from_secs(1)).await;
        return deny(StatusCode::UNAUTHORIZED, "wrong password");
    }

    tracing::info!("signed in");
    let (value, csrf) = state.sessions.issue(now_secs());
    let mut res = Json(json!({ "authenticated": true, "csrf": csrf })).into_response();
    res.headers_mut().insert(
        SET_COOKIE,
        session_cookie(&state, &value, state.sessions.ttl().as_secs()),
    );
    res
}

pub async fn logout(State(state): State<Arc<AppState>>) -> Response {
    let mut res = Json(json!({ "authenticated": false })).into_response();
    res.headers_mut()
        .insert(SET_COOKIE, session_cookie(&state, "", 0));
    res
}

/// Tells the SPA whether it is signed in and hands back the CSRF token, which
/// it cannot read from the `HttpOnly` cookie.
pub async fn session(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let email = req
        .extensions()
        .get::<AccessIdentity>()
        .map(|i| i.0.clone());
    let csrf =
        cookie(req.headers(), SESSION_COOKIE).and_then(|v| state.sessions.verify(v, now_secs()));
    Json(json!({
        "authenticated": csrf.is_some(),
        "csrf": csrf,
        "email": email,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trip() {
        let sessions = Sessions::new(vec![7; 32], Duration::from_secs(100));
        let (value, csrf) = sessions.issue(1_000);
        assert_eq!(sessions.verify(&value, 1_050), Some(csrf.clone()));
        // Expired.
        assert_eq!(sessions.verify(&value, 1_100), None);
        // Another secret.
        assert_eq!(
            Sessions::new(vec![8; 32], Duration::from_secs(100)).verify(&value, 1_050),
            None
        );
        // Tampered expiry.
        let tampered = value.replacen("1100", "9999", 1);
        assert_eq!(sessions.verify(&tampered, 1_050), None);
        // Garbage.
        for bad in ["", ".", "a.b.c", "1100.x.zz"] {
            assert_eq!(sessions.verify(bad, 1_050), None);
        }
        // Each session gets its own CSRF token.
        let (_, other_csrf) = sessions.issue(1_000);
        assert_ne!(csrf, other_csrf);
    }

    #[test]
    fn cookie_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("a=1; dinghy_session=abc.def; CF_Authorization=tok"),
        );
        assert_eq!(cookie(&headers, SESSION_COOKIE), Some("abc.def"));
        assert_eq!(cookie(&headers, ACCESS_COOKIE), Some("tok"));
        assert_eq!(cookie(&headers, "missing"), None);
    }
}
