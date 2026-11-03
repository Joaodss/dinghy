use std::io::{BufRead, IsTerminal};
use std::sync::Arc;

use argon2::Argon2;
use argon2::password_hash::PasswordHasher;
use bollard::Docker;
use dinghy::auth::AccessVerifier;
use dinghy::config::Config;
use dinghy::{AppState, app};
use tracing::Level;

const USAGE: &str = "\
dinghy — a lightweight Docker Compose stack manager

Usage:
  dinghy                 Start the server (configured through DINGHY_* variables)
  dinghy hash-password   Read a password from stdin and print its argon2id hash
  dinghy healthcheck     Exit 0 if the local server's /healthz answers 200 (for Docker HEALTHCHECK)
  dinghy --version";

/// A dependency-free HTTP probe, because the distroless image has no curl.
fn healthcheck() {
    use std::io::{Read, Write};
    let listen = std::env::var("DINGHY_LISTEN").unwrap_or_else(|_| "127.0.0.1:5002".into());
    let port = listen.rsplit(':').next().unwrap_or("5002");
    let probe = || -> std::io::Result<bool> {
        let addr = format!("127.0.0.1:{port}");
        let mut stream = std::net::TcpStream::connect_timeout(
            &addr
                .parse()
                .map_err(|e| std::io::Error::other(format!("{e}")))?,
            std::time::Duration::from_secs(3),
        )?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut head = [0u8; 12];
        stream.read_exact(&mut head)?;
        Ok(head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200"))
    };
    match probe() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("healthcheck: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve(),
        Some("hash-password") => hash_password(),
        Some("healthcheck") => healthcheck(),
        Some("--version" | "-V") => println!("dinghy {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h") => println!("{USAGE}"),
        Some(other) => {
            eprintln!("unknown command {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn hash_password() {
    if std::io::stdin().is_terminal() {
        eprintln!("Password (input is visible; pipe it in to avoid that):");
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .expect("read stdin");
    let password = line.trim_end_matches(['\n', '\r']);
    if password.is_empty() {
        eprintln!("empty password");
        std::process::exit(1);
    }
    match Argon2::default().hash_password(password.as_bytes()) {
        Ok(hash) => println!("{hash}"),
        Err(e) => {
            eprintln!("hashing failed: {e}");
            std::process::exit(1);
        }
    }
}

fn serve() {
    let level = match std::env::var("DINGHY_LOG").as_deref() {
        Ok("debug") => Level::DEBUG,
        Ok("warn") => Level::WARN,
        Ok("error") => Level::ERROR,
        _ => Level::INFO,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("configuration error: {e}");
            std::process::exit(2);
        }
    };

    // One thread is plenty for a single user and keeps memory flat.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(run(config));
}

async fn run(config: Config) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let access = match config.access.clone() {
        Some(access) => match AccessVerifier::new(access) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::error!("{e}");
                std::process::exit(2);
            }
        },
        None => {
            tracing::warn!(
                "Cloudflare Access check is DISABLED (DINGHY_INSECURE_NO_ACCESS); never expose this instance"
            );
            None
        }
    };

    let docker = match Docker::connect_with_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            tracing::error!("cannot set up the Docker client: {e}");
            std::process::exit(2);
        }
    };

    let listen = config.listen;
    tracing::info!(
        %listen,
        stacks = %config.stacks_dir.display(),
        roots = ?config.file_roots.iter().map(|r| &r.name).collect::<Vec<_>>(),
        "starting dinghy {}",
        env!("CARGO_PKG_VERSION")
    );
    let state = Arc::new(AppState::new(config, docker, access));

    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("cannot listen on {listen}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown())
        .await
    {
        tracing::error!("server error: {e}");
        std::process::exit(1);
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
