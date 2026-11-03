//! Stack actions run the real `docker compose` CLI as a subprocess. Output is kept
//! only while the operation runs (and briefly afterwards, so a late viewer can
//! catch the end), then dropped.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, stream};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

use crate::AppState;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::stacks::{self, StackDir};

/// How long a finished operation's output stays available.
const KEEP_FINISHED: Duration = Duration::from_secs(120);
const MAX_LINES: usize = 10_000;

/// Environment variables passed through to `docker compose`. Everything else,
/// including Dinghy's own secrets, is withheld.
const ENV_PASSTHROUGH: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "TMPDIR",
    "DOCKER_HOST",
    "DOCKER_CONFIG",
    "DOCKER_CONTEXT",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "DOCKER_API_VERSION",
    // Windows, for local development.
    "SYSTEMROOT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "TEMP",
    "TMP",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Up,
    Down,
    Restart,
    Pull,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "restart" => Some(Self::Restart),
            "pull" => Some(Self::Pull),
            _ => None,
        }
    }

    /// The compose subcommands to run, in order. Pull also brings the stack up.
    pub fn steps(self) -> &'static [&'static [&'static str]] {
        const UP: &[&str] = &["up", "-d", "--remove-orphans"];
        match self {
            Self::Up => &[UP],
            Self::Down => &[&["down"]],
            Self::Restart => &[&["restart"]],
            Self::Pull => &[&["pull"], UP],
        }
    }
}

/// Builds `docker compose [global flags] <subcommand>` as a program and argument
/// list. Nothing goes through a shell.
pub fn compose_command(config: &Config, stack: &StackDir, step: &[&str]) -> (String, Vec<String>) {
    let mut args: Vec<String> = config.compose_cmd[1..].to_vec();
    args.extend(["--ansi", "never", "--progress", "plain", "-p"].map(String::from));
    args.push(stack.name.clone());
    args.push("-f".into());
    args.push(stack.dir.join(&stack.compose_file).display().to_string());
    if let Some(host_dir) = &config.stacks_host_dir {
        // Compose resolves relative bind mounts against the project directory,
        // which must be the path the Docker daemon sees.
        let host_path: PathBuf = host_dir.join(&stack.name);
        args.push("--project-directory".into());
        args.push(host_path.display().to_string());
        if stack.has_env {
            args.push("--env-file".into());
            args.push(stack.dir.join(".env").display().to_string());
        }
    }
    args.extend(step.iter().map(|s| s.to_string()));
    (config.compose_cmd[0].clone(), args)
}

pub struct Op {
    pub id: String,
    pub stack: String,
    pub action: Action,
    lines: Mutex<Vec<String>>,
    exit: Mutex<Option<i32>>,
    version: watch::Sender<u64>,
}

impl Op {
    fn push(&self, line: String) {
        {
            let mut lines = self.lines.lock().unwrap();
            match lines.len() {
                n if n < MAX_LINES => lines.push(line),
                MAX_LINES => lines.push("[output truncated]".into()),
                _ => return,
            }
        }
        self.version.send_modify(|v| *v += 1);
    }

    fn finish(&self, code: i32) {
        *self.exit.lock().unwrap() = Some(code);
        self.version.send_modify(|v| *v += 1);
    }

    fn snapshot(&self, from: usize) -> (Vec<String>, Option<i32>) {
        let lines = self.lines.lock().unwrap();
        let new = lines
            .get(from..)
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        (new, *self.exit.lock().unwrap())
    }
}

#[derive(Default)]
pub struct Ops {
    ops: Mutex<HashMap<String, Arc<Op>>>,
    busy: Mutex<HashSet<String>>,
}

impl Ops {
    pub fn get(&self, id: &str) -> Option<Arc<Op>> {
        self.ops.lock().unwrap().get(id).cloned()
    }

    pub fn start(
        self: &Arc<Self>,
        config: &Config,
        stack: StackDir,
        action: Action,
    ) -> AppResult<Arc<Op>> {
        if !self.busy.lock().unwrap().insert(stack.name.clone()) {
            return Err(AppError::conflict(format!(
                "another action is running on {}",
                stack.name
            )));
        }
        let op = Arc::new(Op {
            id: hex::encode(rand::random::<[u8; 12]>()),
            stack: stack.name.clone(),
            action,
            lines: Mutex::new(Vec::new()),
            exit: Mutex::new(None),
            version: watch::channel(0).0,
        });
        self.ops.lock().unwrap().insert(op.id.clone(), op.clone());

        let commands: Vec<(String, Vec<String>)> = action
            .steps()
            .iter()
            .map(|step| compose_command(config, &stack, step))
            .collect();
        let ops = self.clone();
        let task_op = op.clone();
        tokio::spawn(async move {
            let code = run_steps(&task_op, &stack, commands).await;
            task_op.finish(code);
            tracing::info!(stack = %task_op.stack, action = ?task_op.action, code, "compose action finished");
            ops.busy.lock().unwrap().remove(&task_op.stack);
            tokio::time::sleep(KEEP_FINISHED).await;
            ops.ops.lock().unwrap().remove(&task_op.id);
        });
        Ok(op)
    }
}

async fn pump(reader: impl AsyncRead + Unpin, op: &Op) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                op.push(line.trim_end_matches(['\n', '\r']).to_string());
            }
        }
    }
}

async fn run_steps(op: &Op, stack: &StackDir, commands: Vec<(String, Vec<String>)>) -> i32 {
    for (program, args) in commands {
        op.push(format!("$ {program} {}", args.join(" ")));
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&stack.dir)
            .env_clear()
            .envs(
                ENV_PASSTHROUGH
                    .iter()
                    .filter_map(|k| std::env::var(k).ok().map(|v| (*k, v))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                op.push(format!("failed to start {program}: {e}"));
                return 127;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        tokio::join!(pump(stdout, op), pump(stderr, op));
        let code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(e) => {
                op.push(format!("failed waiting for {program}: {e}"));
                -1
            }
        };
        if code != 0 {
            return code;
        }
    }
    0
}

pub async fn stack_action(
    State(state): State<Arc<AppState>>,
    Path((name, action)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let action = Action::parse(&action).ok_or_else(|| AppError::bad_request("unknown action"))?;
    let stack = stacks::find(&state.config.stacks_dir, &name).await?;
    tracing::info!(stack = %stack.name, ?action, "starting compose action");
    let op = state.ops.start(&state.config, stack, action)?;
    Ok(Json(json!({ "op": op.id })))
}

/// Streams an operation's output from the beginning, then a final `done` event
/// with the exit code.
pub async fn op_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let op = state
        .ops
        .get(&id)
        .ok_or_else(|| AppError::not_found("operation not found"))?;
    let rx = op.version.subscribe();
    let events = stream::unfold(Some((op, rx, 0usize)), |st| async move {
        let (op, mut rx, sent) = st?;
        loop {
            let (lines, exit) = op.snapshot(sent);
            if !lines.is_empty() {
                let n = lines.len();
                let event = Event::default()
                    .event("lines")
                    .json_data(&lines)
                    .unwrap_or_default();
                return Some((Ok(event), Some((op, rx, sent + n))));
            }
            if let Some(code) = exit {
                return Some((
                    Ok(Event::default().event("done").data(code.to_string())),
                    None,
                ));
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    fn config(host_dir: Option<&str>) -> Config {
        Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            stacks_dir: PathBuf::from("/opt/stacks"),
            stacks_host_dir: host_dir.map(PathBuf::from),
            file_roots: vec![],
            password_hash: String::new(),
            access: None,
            cookie_secure: true,
            compose_cmd: vec!["docker".into(), "compose".into()],
            session_secret: vec![0; 32],
            session_ttl: StdDuration::from_secs(1),
        }
    }

    fn stack(has_env: bool) -> StackDir {
        StackDir {
            name: "caddy".into(),
            dir: PathBuf::from("/opt/stacks/caddy"),
            compose_file: "compose.yaml".into(),
            has_env,
        }
    }

    #[test]
    fn builds_compose_arguments() {
        let (program, args) = compose_command(&config(None), &stack(true), Action::Up.steps()[0]);
        assert_eq!(program, "docker");
        let compose_path = PathBuf::from("/opt/stacks/caddy")
            .join("compose.yaml")
            .display()
            .to_string();
        assert_eq!(
            args,
            [
                "compose",
                "--ansi",
                "never",
                "--progress",
                "plain",
                "-p",
                "caddy",
                "-f",
                &compose_path,
                "up",
                "-d",
                "--remove-orphans"
            ]
        );
    }

    #[test]
    fn passes_host_project_directory() {
        let (_, args) = compose_command(
            &config(Some("/mnt/disks/data/stacks")),
            &stack(true),
            &["down"],
        );
        let host = PathBuf::from("/mnt/disks/data/stacks")
            .join("caddy")
            .display()
            .to_string();
        let pos = args
            .iter()
            .position(|a| a == "--project-directory")
            .unwrap();
        assert_eq!(args[pos + 1], host);
        assert!(args.contains(&"--env-file".to_string()));
        let (_, args) = compose_command(
            &config(Some("/mnt/disks/data/stacks")),
            &stack(false),
            &["down"],
        );
        assert!(!args.contains(&"--env-file".to_string()));
    }

    #[test]
    fn actions() {
        assert_eq!(Action::parse("pull"), Some(Action::Pull));
        assert_eq!(Action::parse("rm"), None);
        assert_eq!(Action::Pull.steps().len(), 2);
    }

    #[test]
    fn caps_output() {
        let op = Op {
            id: "x".into(),
            stack: "s".into(),
            action: Action::Up,
            lines: Mutex::new(Vec::new()),
            exit: Mutex::new(None),
            version: watch::channel(0).0,
        };
        for i in 0..MAX_LINES + 5 {
            op.push(i.to_string());
        }
        let (lines, exit) = op.snapshot(0);
        assert_eq!(lines.len(), MAX_LINES + 1);
        assert_eq!(lines.last().unwrap(), "[output truncated]");
        assert_eq!(exit, None);
        op.finish(0);
        assert_eq!(op.snapshot(MAX_LINES + 1), (vec![], Some(0)));
    }
}
