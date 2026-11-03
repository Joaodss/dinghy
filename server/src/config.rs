use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use argon2::password_hash::phc::PasswordHash;

use crate::stacks::valid_name;

/// Cloudflare Access settings used to validate the `Cf-Access-Jwt-Assertion` header.
#[derive(Debug, Clone)]
pub struct AccessConfig {
    /// Team domain, e.g. `noodleserver.cloudflareaccess.com`.
    pub team_domain: String,
    pub aud: String,
    /// Lowercased email addresses that may sign in.
    pub allowed_emails: Vec<String>,
}

/// A named directory that the file editor may open.
#[derive(Debug, Clone)]
pub struct FileRoot {
    pub name: String,
    /// Canonical path.
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    /// Canonical path of the stacks directory as seen by this process.
    pub stacks_dir: PathBuf,
    /// Path of the stacks directory on the Docker host, when it differs from `stacks_dir`.
    pub stacks_host_dir: Option<PathBuf>,
    pub file_roots: Vec<FileRoot>,
    pub password_hash: String,
    /// `None` only when explicitly disabled with `DINGHY_INSECURE_NO_ACCESS=1`.
    pub access: Option<AccessConfig>,
    pub cookie_secure: bool,
    /// Program and leading arguments used to run Compose, e.g. `["docker", "compose"]`.
    pub compose_cmd: Vec<String>,
    pub session_secret: Vec<u8>,
    pub session_ttl: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let listen = get("DINGHY_LISTEN")
            .unwrap_or_else(|| "127.0.0.1:5002".into())
            .parse::<SocketAddr>()
            .map_err(|e| format!("DINGHY_LISTEN: {e}"))?;

        let stacks_raw = get("DINGHY_STACKS_DIR").unwrap_or_else(|| "/opt/stacks".into());
        let stacks_dir = crate::files::canonicalize(&stacks_raw)
            .map_err(|e| format!("DINGHY_STACKS_DIR ({stacks_raw}): {e}"))?;
        let stacks_host_dir = get("DINGHY_STACKS_HOST_DIR").map(PathBuf::from);

        let file_roots = match get("DINGHY_FILE_ROOTS") {
            Some(spec) => parse_file_roots(&spec)?,
            None => vec![FileRoot {
                name: "stacks".into(),
                path: stacks_dir.clone(),
            }],
        };

        let password_hash = get("DINGHY_PASSWORD_HASH")
            .ok_or("DINGHY_PASSWORD_HASH is required (see `dinghy hash-password`)")?;
        PasswordHash::new(&password_hash).map_err(|e| format!("DINGHY_PASSWORD_HASH: {e}"))?;

        let access = if get("DINGHY_INSECURE_NO_ACCESS").is_some_and(|v| truthy(&v)) {
            None
        } else {
            let team = get("DINGHY_CF_TEAM")
                .ok_or("DINGHY_CF_TEAM is required (or set DINGHY_INSECURE_NO_ACCESS=1 for local development)")?;
            let aud = get("DINGHY_CF_AUD").ok_or("DINGHY_CF_AUD is required")?;
            let allowed_emails: Vec<String> = get("DINGHY_ALLOWED_EMAILS")
                .unwrap_or_default()
                .split(',')
                .map(|e| e.trim().to_lowercase())
                .filter(|e| !e.is_empty())
                .collect();
            if allowed_emails.is_empty() {
                return Err("DINGHY_ALLOWED_EMAILS must list at least one email".into());
            }
            let team_domain = if team.contains('.') {
                team
            } else {
                format!("{team}.cloudflareaccess.com")
            };
            Some(AccessConfig {
                team_domain,
                aud,
                allowed_emails,
            })
        };

        let cookie_secure = get("DINGHY_COOKIE_SECURE").is_none_or(|v| truthy(&v));

        let compose_cmd: Vec<String> = get("DINGHY_COMPOSE_CMD")
            .unwrap_or_else(|| "docker compose".into())
            .split_whitespace()
            .map(String::from)
            .collect();
        if compose_cmd.is_empty() {
            return Err("DINGHY_COMPOSE_CMD is empty".into());
        }

        let session_secret = match get("DINGHY_SESSION_SECRET") {
            Some(hex_secret) => {
                let bytes = hex::decode(hex_secret.trim())
                    .map_err(|e| format!("DINGHY_SESSION_SECRET: {e}"))?;
                if bytes.len() < 32 {
                    return Err(
                        "DINGHY_SESSION_SECRET must be at least 32 bytes (64 hex characters)"
                            .into(),
                    );
                }
                bytes
            }
            None => rand::random::<[u8; 32]>().to_vec(),
        };

        Ok(Self {
            listen,
            stacks_dir,
            stacks_host_dir,
            file_roots,
            password_hash,
            access,
            cookie_secure,
            compose_cmd,
            session_secret,
            session_ttl: Duration::from_secs(12 * 60 * 60),
        })
    }

    pub fn file_root(&self, name: &str) -> Option<&FileRoot> {
        self.file_roots.iter().find(|r| r.name == name)
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Parses `stacks=/opt/stacks,scripts=/opt/scripts`.
fn parse_file_roots(spec: &str) -> Result<Vec<FileRoot>, String> {
    let mut roots: Vec<FileRoot> = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (name, path) = entry
            .split_once('=')
            .ok_or_else(|| format!("DINGHY_FILE_ROOTS: expected name=path, got {entry:?}"))?;
        let name = name.trim();
        if !valid_name(name) {
            return Err(format!("DINGHY_FILE_ROOTS: invalid root name {name:?}"));
        }
        if roots.iter().any(|r| r.name == name) {
            return Err(format!("DINGHY_FILE_ROOTS: duplicate root name {name:?}"));
        }
        let path = crate::files::canonicalize(path.trim())
            .map_err(|e| format!("DINGHY_FILE_ROOTS: {name}={}: {e}", path.trim()))?;
        roots.push(FileRoot {
            name: name.to_string(),
            path,
        });
    }
    if roots.is_empty() {
        return Err("DINGHY_FILE_ROOTS is empty".into());
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$3QZ6bJ7Ui0Xx5k2hOAnmTnS3Fq5Gy8l5mN0S5qkqQGc";

    fn lookup(map: HashMap<&'static str, String>) -> impl Fn(&str) -> Option<String> {
        move |k| map.get(k).cloned()
    }

    fn base(dir: &std::path::Path) -> HashMap<&'static str, String> {
        HashMap::from([
            ("DINGHY_STACKS_DIR", dir.display().to_string()),
            ("DINGHY_PASSWORD_HASH", HASH.to_string()),
            ("DINGHY_CF_TEAM", "noodleserver".to_string()),
            ("DINGHY_CF_AUD", "aud123".to_string()),
            (
                "DINGHY_ALLOWED_EMAILS",
                " Someone@Example.com ,".to_string(),
            ),
        ])
    }

    #[test]
    fn defaults_and_access() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_lookup(lookup(base(dir.path()))).unwrap();
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:5002");
        assert_eq!(cfg.compose_cmd, vec!["docker", "compose"]);
        assert!(cfg.cookie_secure);
        assert_eq!(cfg.file_roots.len(), 1);
        assert_eq!(cfg.file_roots[0].name, "stacks");
        let access = cfg.access.unwrap();
        assert_eq!(access.team_domain, "noodleserver.cloudflareaccess.com");
        assert_eq!(access.allowed_emails, vec!["someone@example.com"]);
    }

    #[test]
    fn access_is_required_unless_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = base(dir.path());
        env.remove("DINGHY_CF_TEAM");
        assert!(Config::from_lookup(lookup(env.clone())).is_err());
        env.insert("DINGHY_INSECURE_NO_ACCESS", "1".into());
        assert!(Config::from_lookup(lookup(env)).unwrap().access.is_none());
    }

    #[test]
    fn rejects_bad_password_hash_and_roots() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = base(dir.path());
        env.insert("DINGHY_PASSWORD_HASH", "plaintext".into());
        assert!(Config::from_lookup(lookup(env)).is_err());

        let mut env = base(dir.path());
        env.insert("DINGHY_FILE_ROOTS", "../x=/tmp".into());
        assert!(Config::from_lookup(lookup(env)).is_err());
    }

    #[test]
    fn parses_file_roots() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let spec = format!(
            "stacks={},scripts={}",
            a.path().display(),
            b.path().display()
        );
        let roots = parse_file_roots(&spec).unwrap();
        assert_eq!(
            roots.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["stacks", "scripts"]
        );
        assert!(parse_file_roots(&format!("a={0},a={0}", a.path().display())).is_err());
    }
}
