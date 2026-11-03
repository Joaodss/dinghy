//! End-to-end checks of the HTTP layer: the three auth gates, file editing
//! rules and input validation. None of these touch Docker.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::PasswordHasher;
use argon2::{Algorithm as ArgonAlgorithm, Argon2, Params, Version};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bollard::Docker;
use dinghy::auth::AccessVerifier;
use dinghy::config::Config;
use dinghy::jwt::JwkSet;
use dinghy::{AppState, app};
use http_body_util::BodyExt;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};
use serde_json::{Value, json};
use tower::ServiceExt;

const KEY_PEM: &str = include_str!("fixtures/access_test_key.pem");
const KID: &str = "test-kid";
const AUD: &str = "test-aud";
const TEAM: &str = "noodleserver";
const EMAIL: &str = "admin@example.com";
const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    _dir: tempfile::TempDir,
    stacks: PathBuf,
    app: Router,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn key_pair() -> RsaKeyPair {
    let der: String = KEY_PEM
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    RsaKeyPair::from_pkcs8(&STANDARD.decode(der).unwrap()).unwrap()
}

/// The test key as Cloudflare would publish it.
fn jwks() -> JwkSet {
    let public = RsaPublicKeyComponents::<Vec<u8>>::from(key_pair().public());
    serde_json::from_value(json!({ "keys": [{
        "kty": "RSA", "kid": KID,
        "n": URL_SAFE_NO_PAD.encode(&public.n), "e": URL_SAFE_NO_PAD.encode(&public.e),
    }]}))
    .unwrap()
}

/// A Cloudflare Access style RS256 token signed with the test key.
fn token(aud: &str, email: &str, exp: u64) -> String {
    let header = json!({ "alg": "RS256", "kid": KID, "typ": "JWT" });
    let claims = json!({
        "aud": [aud],
        "iss": format!("https://{TEAM}.cloudflareaccess.com"),
        "email": email,
        "exp": exp,
        "iat": now(),
    });
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let pair = key_pair();
    let mut sig = vec![0; pair.public().modulus_len()];
    pair.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut sig,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
}

fn valid_token() -> String {
    token(AUD, EMAIL, now() + 600)
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = dinghy::files::canonicalize(dir.path()).unwrap();
    let stacks = base.join("stacks");
    std::fs::create_dir_all(stacks.join("caddy")).unwrap();
    std::fs::write(
        stacks.join("caddy/compose.yaml"),
        "services:\n  caddy:\n    image: caddy\n",
    )
    .unwrap();
    std::fs::write(stacks.join("caddy/.env"), "API_TOKEN=supersecret\n# note\n").unwrap();
    std::fs::write(base.join("outside.txt"), "nope").unwrap();

    // Cheap argon2 parameters keep the debug-build tests fast.
    let params = Params::new(1024, 1, 1, None).unwrap();
    let hash = Argon2::new(ArgonAlgorithm::Argon2id, Version::V0x13, params)
        .hash_password(PASSWORD.as_bytes())
        .unwrap()
        .to_string();

    let stacks_str = stacks.display().to_string();
    let config = Config::from_lookup(|key| match key {
        "DINGHY_STACKS_DIR" => Some(stacks_str.clone()),
        "DINGHY_PASSWORD_HASH" => Some(hash.clone()),
        "DINGHY_CF_TEAM" => Some(TEAM.into()),
        "DINGHY_CF_AUD" => Some(AUD.into()),
        "DINGHY_ALLOWED_EMAILS" => Some(EMAIL.into()),
        _ => None,
    })
    .unwrap();

    let verifier = AccessVerifier::with_keys(config.access.clone().unwrap(), jwks());

    // None of these tests reach Docker; an HTTP client connects lazily, so the
    // tests run without a Docker socket.
    let docker =
        Docker::connect_with_http("http://127.0.0.1:9", 1, bollard::API_DEFAULT_VERSION).unwrap();
    let state = Arc::new(AppState::new(config, docker, Some(verifier)));
    Harness {
        _dir: dir,
        stacks,
        app: app(state),
    }
}

struct Session {
    cookie: String,
    csrf: String,
}

impl Harness {
    async fn send(&self, req: Request<Body>) -> Response {
        self.app.clone().oneshot(req).await.unwrap()
    }

    fn request(
        &self,
        method: Method,
        uri: &str,
        session: Option<&Session>,
    ) -> axum::http::request::Builder {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("cf-access-jwt-assertion", valid_token());
        if let Some(s) = session {
            builder = builder.header(header::COOKIE, &s.cookie);
        }
        builder
    }

    async fn login(&self) -> Session {
        let res = self
            .send(
                self.request(Method::POST, "/api/login", None)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let set_cookie = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(set_cookie.contains("Secure"));
        assert!(set_cookie.contains("Max-Age=43200"));
        let cookie = set_cookie.split(';').next().unwrap().to_string();
        let body = json_body(res).await;
        Session {
            cookie,
            csrf: body["csrf"].as_str().unwrap().to_string(),
        }
    }
}

async fn json_body(res: Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn empty() -> Body {
    Body::empty()
}

#[tokio::test]
async fn access_token_is_required_everywhere() {
    let h = harness();
    for uri in [
        "/",
        "/index.html",
        "/api/session",
        "/api/stacks",
        "/api/files?root=stacks",
    ] {
        let res = h.send(Request::get(uri).body(empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
    }
    let bad_tokens = [
        token("other-aud", EMAIL, now() + 600),
        token(AUD, "intruder@example.com", now() + 600),
        token(AUD, EMAIL, now() - 600),
        "not.a.jwt".to_string(),
    ];
    for t in bad_tokens {
        let res = h
            .send(
                Request::get("/api/session")
                    .header("cf-access-jwt-assertion", t)
                    .body(empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }
    // The CF_Authorization cookie works as well as the header.
    let res = h
        .send(
            Request::get("/api/session")
                .header(
                    header::COOKIE,
                    format!("CF_Authorization={}", valid_token()),
                )
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn session_and_csrf_are_required() {
    let h = harness();
    let res = h
        .send(
            h.request(Method::GET, "/api/session", None)
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["authenticated"], false);
    assert_eq!(body["email"], EMAIL);

    for uri in [
        "/api/stacks",
        "/api/roots",
        "/api/files?root=stacks",
        "/api/containers/abc/logs",
        "/api/ops/x/stream",
    ] {
        let res = h
            .send(h.request(Method::GET, uri, None).body(empty()).unwrap())
            .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }

    let res = h
        .send(
            h.request(Method::POST, "/api/login", None)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "password": "wrong" }).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().get(header::SET_COOKIE).is_none());

    let session = h.login().await;
    let res = h
        .send(
            h.request(Method::GET, "/api/roots", Some(&session))
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json_body(res).await["roots"], json!(["stacks"]));

    let res = h
        .send(
            h.request(Method::GET, "/api/session", Some(&session))
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(json_body(res).await["csrf"], session.csrf.as_str());

    // POST without, and with a wrong, CSRF token.
    for csrf in [None, Some("deadbeef")] {
        let mut req = h.request(Method::POST, "/api/stacks/caddy/up", Some(&session));
        if let Some(c) = csrf {
            req = req.header("x-dinghy-csrf", c);
        }
        let res = h.send(req.body(empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // A session cookie is not enough without the Access token.
    let res = h
        .send(
            Request::get("/api/roots")
                .header(header::COOKIE, &session.cookie)
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn validates_stack_names_and_actions() {
    let h = harness();
    let s = h.login().await;
    let cases = [
        ("/api/stacks/Caddy/up", StatusCode::BAD_REQUEST),
        ("/api/stacks/caddy;id/up", StatusCode::BAD_REQUEST),
        ("/api/stacks/..%2Fetc/up", StatusCode::BAD_REQUEST),
        ("/api/stacks/missing/up", StatusCode::NOT_FOUND),
        ("/api/stacks/caddy/rm", StatusCode::BAD_REQUEST),
    ];
    for (uri, status) in cases {
        let res = h
            .send(
                h.request(Method::POST, uri, Some(&s))
                    .header("x-dinghy-csrf", &s.csrf)
                    .body(empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(res.status(), status, "{uri}");
    }
    let res = h
        .send(
            h.request(Method::GET, "/api/containers/..%2F..%2Fetc/logs", Some(&s))
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = h
        .send(
            h.request(Method::GET, "/api/nope", Some(&s))
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn file_reads_mask_env_and_refuse_traversal() {
    let h = harness();
    let s = h.login().await;

    let res = h
        .send(
            h.request(Method::GET, "/api/files?root=stacks&path=caddy", Some(&s))
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let names: Vec<String> = json_body(res)
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, [".env", "compose.yaml"]);

    let res = h
        .send(
            h.request(
                Method::GET,
                "/api/files/content?root=stacks&path=caddy/.env",
                Some(&s),
            )
            .body(empty())
            .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    let body = json_body(res).await;
    assert_eq!(body["masked"], true);
    assert!(!body["content"].as_str().unwrap().contains("supersecret"));

    let res = h
        .send(
            h.request(
                Method::GET,
                "/api/files/content?root=stacks&path=caddy/.env&reveal=true",
                Some(&s),
            )
            .body(empty())
            .unwrap(),
        )
        .await;
    assert_eq!(res.headers()[header::ETAG].to_str().unwrap(), etag);
    assert!(
        json_body(res).await["content"]
            .as_str()
            .unwrap()
            .contains("supersecret")
    );

    for path in [
        "../outside.txt",
        "..%2Foutside.txt",
        "%2e%2e%2Foutside.txt",
        "caddy/../../outside.txt",
        "%2Fetc%2Fpasswd",
    ] {
        let uri = format!("/api/files/content?root=stacks&path={path}");
        let res = h
            .send(
                h.request(Method::GET, &uri, Some(&s))
                    .body(empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{path}");
    }
    let res = h
        .send(
            h.request(Method::GET, "/api/files?root=..&path=", Some(&s))
                .body(empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn file_writes_need_matching_etag() {
    let h = harness();
    let s = h.login().await;
    let uri = "/api/files/content?root=stacks&path=caddy/compose.yaml";

    let res = h
        .send(h.request(Method::GET, uri, Some(&s)).body(empty()).unwrap())
        .await;
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();

    let put = |if_match: Option<&str>, csrf: Option<&str>, body: &str| {
        let mut req = h.request(Method::PUT, uri, Some(&s));
        if let Some(m) = if_match {
            req = req.header(header::IF_MATCH, m);
        }
        if let Some(c) = csrf {
            req = req.header("x-dinghy-csrf", c);
        }
        req.body(Body::from(body.to_string())).unwrap()
    };

    assert_eq!(
        h.send(put(Some(&etag), None, "x")).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.send(put(None, Some(&s.csrf), "x")).await.status(),
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        h.send(put(Some("\"stale\""), Some(&s.csrf), "x"))
            .await
            .status(),
        StatusCode::PRECONDITION_FAILED
    );

    let res = h
        .send(put(Some(&etag), Some(&s.csrf), "services: {}\n"))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let new_etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_ne!(new_etag, etag);
    assert_eq!(
        std::fs::read_to_string(h.stacks.join("caddy/compose.yaml")).unwrap(),
        "services: {}\n"
    );

    // The old ETag is now stale.
    assert_eq!(
        h.send(put(Some(&etag), Some(&s.csrf), "y")).await.status(),
        StatusCode::PRECONDITION_FAILED
    );

    // Saving masked .env values is refused.
    let env_uri = "/api/files/content?root=stacks&path=caddy/.env";
    let res = h
        .send(
            h.request(Method::GET, env_uri, Some(&s))
                .body(empty())
                .unwrap(),
        )
        .await;
    let env_etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    let masked = json_body(res).await["content"]
        .as_str()
        .unwrap()
        .to_string();
    let res = h
        .send(
            h.request(Method::PUT, env_uri, Some(&s))
                .header(header::IF_MATCH, &env_etag)
                .header("x-dinghy-csrf", &s.csrf)
                .body(Body::from(masked))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        std::fs::read_to_string(h.stacks.join("caddy/.env"))
            .unwrap()
            .contains("supersecret")
    );
}

#[tokio::test]
async fn creates_and_renames_entries() {
    let h = harness();
    let s = h.login().await;
    let post = |uri: &str, body: Value| {
        h.request(Method::POST, uri, Some(&s))
            .header("x-dinghy-csrf", &s.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let res = h
        .send(post(
            "/api/files/create",
            json!({"root": "stacks", "path": "newstack", "dir": true}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = h
        .send(post(
            "/api/files/create",
            json!({"root": "stacks", "path": "newstack/compose.yaml"}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = h
        .send(post(
            "/api/files/create",
            json!({"root": "stacks", "path": "newstack/compose.yaml"}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let res = h
        .send(post(
            "/api/files/create",
            json!({"root": "stacks", "path": "../evil.txt"}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = h
        .send(post(
            "/api/files/rename",
            json!({"root": "stacks", "from": "newstack/compose.yaml", "to": "newstack/docker-compose.yml"}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert!(h.stacks.join("newstack/docker-compose.yml").is_file());

    let res = h
        .send(post(
            "/api/files/rename",
            json!({"root": "stacks", "from": "newstack/docker-compose.yml", "to": "../stolen.yml"}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = h
        .send(post(
            "/api/files/rename",
            json!({"root": "stacks", "from": "caddy/.env", "to": "caddy/compose.yaml"}),
        ))
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn sets_security_headers() {
    let h = harness();
    let res = h
        .send(
            h.request(Method::GET, "/api/session", None)
                .body(empty())
                .unwrap(),
        )
        .await;
    let headers = res.headers();
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
}
