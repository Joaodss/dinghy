use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

/// Compose file names, in the order `docker compose` itself prefers them.
pub const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yml",
    "docker-compose.yaml",
];

/// A stack is a directory under the stacks dir that holds a Compose file.
#[derive(Debug, Clone)]
pub struct StackDir {
    pub name: String,
    pub dir: PathBuf,
    pub compose_file: String,
    pub has_env: bool,
}

/// Stack and root names: `^[a-z0-9][a-z0-9_-]*$`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 128
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn inspect_dir(name: &str, dir: PathBuf) -> Option<StackDir> {
    let compose_file = COMPOSE_FILES.iter().find(|f| dir.join(f).is_file())?;
    Some(StackDir {
        name: name.to_string(),
        has_env: dir.join(".env").is_file(),
        compose_file: compose_file.to_string(),
        dir,
    })
}

/// Lists every valid stack directory, sorted by name.
pub async fn discover(stacks_dir: &Path) -> AppResult<Vec<StackDir>> {
    let stacks_dir = stacks_dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> AppResult<Vec<StackDir>> {
        let mut stacks = Vec::new();
        for entry in std::fs::read_dir(&stacks_dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !valid_name(&name) || !entry.path().is_dir() {
                continue;
            }
            if let Some(stack) = inspect_dir(&name, entry.path()) {
                stacks.push(stack);
            }
        }
        stacks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(stacks)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

/// Looks up one stack, validating its name before touching the filesystem.
pub async fn find(stacks_dir: &Path, name: &str) -> AppResult<StackDir> {
    if !valid_name(name) {
        return Err(AppError::bad_request("invalid stack name"));
    }
    let dir = stacks_dir.join(name);
    let owned = name.to_string();
    tokio::task::spawn_blocking(move || dir.is_dir().then(|| inspect_dir(&owned, dir)).flatten())
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found(format!("stack {name} not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        for ok in [
            "a",
            "actual-budget",
            "caddy",
            "planeador_ferias",
            "0x",
            "a-b_c9",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "_a",
            "A",
            "Caddy",
            "a b",
            "a/b",
            "..",
            "a..b",
            "a;rm -rf /",
            "a$b",
            "é",
            "a\nb",
            ".env",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn discovers_stacks() {
        let root = tempfile::tempdir().unwrap();
        let mk = |name: &str, file: Option<&str>| {
            let dir = root.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            if let Some(f) = file {
                std::fs::write(dir.join(f), "services: {}\n").unwrap();
            }
        };
        mk("caddy", Some("compose.yaml"));
        mk("actual", Some("docker-compose.yml"));
        mk("empty", None);
        mk("Bad", Some("compose.yaml"));
        std::fs::write(root.path().join("actual/.env"), "A=1\n").unwrap();

        let stacks = discover(root.path()).await.unwrap();
        let names: Vec<_> = stacks.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["actual", "caddy"]);
        assert_eq!(stacks[0].compose_file, "docker-compose.yml");
        assert!(stacks[0].has_env);
        assert!(!stacks[1].has_env);

        assert!(find(root.path(), "caddy").await.is_ok());
        assert_eq!(find(root.path(), "empty").await.unwrap_err().status, 404);
        assert_eq!(find(root.path(), "../etc").await.unwrap_err().status, 400);
    }
}
