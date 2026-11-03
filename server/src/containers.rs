//! Reads stack and container state from the Docker API. Nothing is cached: each
//! request asks Docker again, and stats are only fetched for the page being viewed.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::models::{ContainerMemoryStats, ContainerSummary, ContainerSummaryHealthStatusEnum};
use bollard::query_parameters::{
    InspectContainerOptions, ListContainersOptionsBuilder, LogsOptionsBuilder, StatsOptionsBuilder,
};
use futures_util::{Stream, StreamExt, future::join_all, stream};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::stacks::{self, StackDir};

pub const PROJECT_LABEL: &str = "com.docker.compose.project";
pub const SERVICE_LABEL: &str = "com.docker.compose.service";

#[derive(Debug, Clone, Serialize)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub project: Option<String>,
    pub service: Option<String>,
    pub image: String,
    /// `running`, `exited`, `restarting`, ...
    pub state: String,
    /// Docker's human summary, e.g. `Up 3 hours (healthy)`.
    pub status: String,
    /// `healthy`, `unhealthy` or `starting`; absent without a healthcheck.
    pub health: Option<String>,
    pub exit_code: Option<i64>,
    pub created: Option<i64>,
    pub started_at: Option<String>,
    pub memory_bytes: Option<u64>,
    pub memory_limit: Option<u64>,
    pub ports: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StackStatus {
    Running,
    Partial,
    Stopped,
    Error,
}

#[derive(Debug, Serialize)]
pub struct StackSummary {
    pub name: String,
    pub status: StackStatus,
    pub running: usize,
    pub total: usize,
    pub memory_bytes: Option<u64>,
    pub compose_file: String,
    pub has_env: bool,
}

#[derive(Debug, Serialize)]
pub struct StackList {
    pub stacks: Vec<StackSummary>,
    pub orphans: Vec<ContainerInfo>,
}

#[derive(Debug, Serialize)]
pub struct StackDetail {
    pub name: String,
    pub status: StackStatus,
    pub compose_file: String,
    pub has_env: bool,
    pub containers: Vec<ContainerInfo>,
}

/// Parses the exit code from a summary status such as `Exited (137) 2 hours ago`.
pub fn parse_exit_code(status: &str) -> Option<i64> {
    let rest = status.strip_prefix("Exited (")?;
    rest.split_once(')')?.0.parse().ok()
}

fn health_from_status(status: &str) -> Option<String> {
    if status.contains("(unhealthy)") {
        Some("unhealthy".into())
    } else if status.contains("(healthy)") {
        Some("healthy".into())
    } else if status.contains("(health: starting)") {
        Some("starting".into())
    } else {
        None
    }
}

fn is_failed(c: &ContainerInfo) -> bool {
    let crashed =
        matches!(c.state.as_str(), "exited" | "dead") && c.exit_code.is_some_and(|code| code != 0);
    crashed || c.state == "restarting" || c.health.as_deref() == Some("unhealthy")
}

/// Rolls container states up into one stack status.
pub fn aggregate(containers: &[ContainerInfo]) -> StackStatus {
    if containers.iter().any(is_failed) {
        return StackStatus::Error;
    }
    let running = containers.iter().filter(|c| c.state == "running").count();
    match running {
        0 => StackStatus::Stopped,
        n if n == containers.len() => StackStatus::Running,
        _ => StackStatus::Partial,
    }
}

fn format_ports(summary: &ContainerSummary) -> Vec<String> {
    let mut ports: Vec<String> = summary
        .ports
        .iter()
        .flatten()
        .map(|p| {
            let proto = p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into());
            match (p.ip.as_deref(), p.public_port) {
                (Some(ip), Some(public)) if !ip.is_empty() => {
                    let ip = if ip.contains(':') {
                        format!("[{ip}]")
                    } else {
                        ip.to_string()
                    };
                    format!("{ip}:{public}→{}/{proto}", p.private_port)
                }
                (_, Some(public)) => format!("{public}→{}/{proto}", p.private_port),
                _ => format!("{}/{proto}", p.private_port),
            }
        })
        .collect();
    ports.sort();
    ports.dedup();
    ports
}

pub fn from_summary(summary: ContainerSummary) -> ContainerInfo {
    let labels = summary.labels.clone().unwrap_or_default();
    let status = summary.status.clone().unwrap_or_default();
    let health = summary
        .health
        .as_ref()
        .and_then(|h| h.status)
        .and_then(|s| match s {
            ContainerSummaryHealthStatusEnum::HEALTHY => Some("healthy".to_string()),
            ContainerSummaryHealthStatusEnum::UNHEALTHY => Some("unhealthy".to_string()),
            ContainerSummaryHealthStatusEnum::STARTING => Some("starting".to_string()),
            _ => None,
        })
        .or_else(|| health_from_status(&status));
    let name = summary
        .names
        .as_ref()
        .and_then(|n| n.first())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_default();
    ContainerInfo {
        id: summary.id.clone().unwrap_or_default(),
        name,
        project: labels.get(PROJECT_LABEL).cloned(),
        service: labels.get(SERVICE_LABEL).cloned(),
        image: summary.image.clone().unwrap_or_default(),
        state: summary.state.map(|s| s.to_string()).unwrap_or_default(),
        exit_code: parse_exit_code(&status),
        health,
        status,
        created: summary.created,
        started_at: None,
        memory_bytes: None,
        memory_limit: None,
        ports: format_ports(&summary),
    }
}

/// Memory in use the way `docker stats` reports it: usage minus page cache.
pub fn memory_in_use(mem: &ContainerMemoryStats) -> Option<u64> {
    let usage = mem.usage?;
    let cache = mem.stats.as_ref().and_then(|s| {
        s.get("inactive_file")
            .or_else(|| s.get("total_inactive_file"))
            .or_else(|| s.get("cache"))
            .copied()
    });
    Some(usage.saturating_sub(cache.unwrap_or(0)))
}

async fn memory_usage(docker: &Docker, id: &str) -> Option<u64> {
    let options = StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();
    let stats = docker.stats(id, Some(options)).next().await?.ok()?;
    memory_in_use(stats.memory_stats.as_ref()?)
}

/// Fills `memory_bytes` for every running container, concurrently.
async fn fill_memory(docker: &Docker, containers: &mut [ContainerInfo]) {
    let usage = join_all(containers.iter().map(|c| async move {
        if c.state == "running" {
            memory_usage(docker, &c.id).await
        } else {
            None
        }
    }))
    .await;
    for (c, mem) in containers.iter_mut().zip(usage) {
        c.memory_bytes = mem;
    }
}

fn sum_memory(containers: &[ContainerInfo]) -> Option<u64> {
    let values: Vec<u64> = containers.iter().filter_map(|c| c.memory_bytes).collect();
    (!values.is_empty()).then(|| values.iter().sum())
}

async fn list_containers(docker: &Docker, project: Option<&str>) -> AppResult<Vec<ContainerInfo>> {
    let mut builder = ListContainersOptionsBuilder::default().all(true);
    let filters: HashMap<String, Vec<String>>;
    if let Some(project) = project {
        filters = HashMap::from([(
            "label".to_string(),
            vec![format!("{PROJECT_LABEL}={project}")],
        )]);
        builder = builder.filters(&filters);
    }
    let summaries = docker.list_containers(Some(builder.build())).await?;
    Ok(summaries.into_iter().map(from_summary).collect())
}

pub async fn list_stacks(State(state): State<Arc<AppState>>) -> AppResult<Json<StackList>> {
    let dirs = stacks::discover(&state.config.stacks_dir).await?;
    let mut containers = list_containers(&state.docker, None).await?;
    fill_memory(&state.docker, &mut containers).await;

    let mut by_project: HashMap<String, Vec<ContainerInfo>> = HashMap::new();
    let mut orphans = Vec::new();
    for c in containers {
        match c
            .project
            .as_deref()
            .filter(|p| dirs.iter().any(|d| d.name == *p))
        {
            Some(p) => by_project.entry(p.to_string()).or_default().push(c),
            None => orphans.push(c),
        }
    }
    orphans.sort_by(|a, b| a.name.cmp(&b.name));

    let stacks = dirs
        .into_iter()
        .map(|dir| {
            let members = by_project.remove(&dir.name).unwrap_or_default();
            StackSummary {
                status: aggregate(&members),
                running: members.iter().filter(|c| c.state == "running").count(),
                total: members.len(),
                memory_bytes: sum_memory(&members),
                name: dir.name,
                compose_file: dir.compose_file,
                has_env: dir.has_env,
            }
        })
        .collect();
    Ok(Json(StackList { stacks, orphans }))
}

async fn add_inspect_details(docker: &Docker, containers: &mut [ContainerInfo]) {
    let details = join_all(
        containers
            .iter()
            .map(|c| docker.inspect_container(&c.id, None::<InspectContainerOptions>)),
    )
    .await;
    for (c, detail) in containers.iter_mut().zip(details) {
        let Ok(detail) = detail else { continue };
        if let Some(st) = detail.state {
            c.started_at = st.started_at.filter(|s| !s.starts_with("0001-"));
            if matches!(c.state.as_str(), "exited" | "dead") {
                c.exit_code = st.exit_code.or(c.exit_code);
            }
            if let Some(h) = st.health.and_then(|h| h.status) {
                let h = h.to_string();
                if matches!(h.as_str(), "healthy" | "unhealthy" | "starting") {
                    c.health = Some(h);
                }
            }
        }
        c.memory_limit = detail
            .host_config
            .and_then(|h| h.memory)
            .filter(|m| *m > 0)
            .map(|m| m as u64);
    }
}

pub async fn stack_detail(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> AppResult<Json<StackDetail>> {
    let StackDir {
        name,
        compose_file,
        has_env,
        ..
    } = stacks::find(&state.config.stacks_dir, &name).await?;
    let mut containers = list_containers(&state.docker, Some(&name)).await?;
    fill_memory(&state.docker, &mut containers).await;
    add_inspect_details(&state.docker, &mut containers).await;
    containers.sort_by(|a, b| a.service.cmp(&b.service).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(StackDetail {
        status: aggregate(&containers),
        name,
        compose_file,
        has_env,
        containers,
    }))
}

/// Container ids and names: hex ids or `[a-zA-Z0-9][a-zA-Z0-9_.-]*`.
pub fn valid_container_ref(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && id.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    tail: Option<u32>,
}

/// Most Docker log frames merged into one SSE event.
const LOG_BATCH: usize = 1024;

#[derive(Debug, PartialEq, Serialize)]
pub struct LogLine {
    /// `o` for stdout, `e` for stderr.
    pub s: &'static str,
    pub t: String,
}

/// Unfinished lines per stream, carried between Docker frames.
#[derive(Default)]
pub struct LineBuffers {
    out: String,
    err: String,
}

impl LineBuffers {
    pub fn push(&mut self, output: LogOutput, lines: &mut Vec<LogLine>) {
        let (kind, pending, message) = match output {
            LogOutput::StdErr { message } => ("e", &mut self.err, message),
            LogOutput::StdOut { message } | LogOutput::Console { message } => {
                ("o", &mut self.out, message)
            }
            LogOutput::StdIn { .. } => return,
        };
        lines.extend(
            split_lines(pending, &String::from_utf8_lossy(&message))
                .into_iter()
                .map(|t| LogLine { s: kind, t }),
        );
    }
}

/// Splits a log chunk into complete lines, keeping any unfinished line in `pending`.
pub fn split_lines(pending: &mut String, chunk: &str) -> Vec<String> {
    pending.push_str(chunk);
    let mut lines = Vec::new();
    while let Some(pos) = pending.find('\n') {
        let line: String = pending.drain(..=pos).collect();
        lines.push(line.trim_end_matches(['\n', '\r']).to_string());
    }
    lines
}

/// Streams the last `tail` lines and then follows. The Docker stream is owned by
/// the response, so it is dropped the moment the browser disconnects.
pub async fn container_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    if !valid_container_ref(&id) {
        return Err(AppError::bad_request("invalid container id"));
    }
    // Fail with a proper status before switching to a stream.
    state
        .docker
        .inspect_container(&id, None::<InspectContainerOptions>)
        .await?;

    let tail = query.tail.unwrap_or(200).min(5000).to_string();
    let options = LogsOptionsBuilder::default()
        .follow(true)
        .stdout(true)
        .stderr(true)
        .timestamps(true)
        .tail(&tail)
        .build();
    let logs = state.docker.logs(&id, Some(options));

    // Docker sends roughly one frame per write. `ready_chunks` groups the frames
    // that are already waiting into one event, so a quiet container still gets
    // each line at once while a busy one sends a few large events instead of
    // thousands of tiny ones.
    let events = logs
        .ready_chunks(LOG_BATCH)
        .scan(LineBuffers::default(), |buffers, batch| {
            let mut events = Vec::with_capacity(2);
            let mut lines = Vec::new();
            let mut error = None;
            for item in batch {
                match item {
                    Ok(output) => buffers.push(output, &mut lines),
                    Err(e) => {
                        error = Some(e.to_string());
                        break;
                    }
                }
            }
            if !lines.is_empty()
                && let Ok(event) = Event::default().event("lines").json_data(&lines)
            {
                events.push(event);
            }
            if let Some(e) = error {
                events.push(Event::default().event("error").data(e));
            }
            std::future::ready(Some(stream::iter(events.into_iter().map(Ok))))
        })
        .flatten()
        .chain(stream::once(std::future::ready(Ok(Event::default()
            .event("end")
            .data("")))));

    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

pub async fn healthz(State(state): State<Arc<AppState>>) -> AppResult<&'static str> {
    state.docker.ping().await.map_err(|e| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            format!("docker: {e}"),
        )
    })?;
    Ok("ok")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(state: &str, exit_code: Option<i64>, health: Option<&str>) -> ContainerInfo {
        ContainerInfo {
            id: "abc".into(),
            name: "c".into(),
            project: Some("p".into()),
            service: Some("s".into()),
            image: "img".into(),
            state: state.into(),
            status: String::new(),
            health: health.map(String::from),
            exit_code,
            created: None,
            started_at: None,
            memory_bytes: None,
            memory_limit: None,
            ports: vec![],
        }
    }

    #[test]
    fn aggregates_status() {
        let up = container("running", None, None);
        let healthy = container("running", None, Some("healthy"));
        let clean_exit = container("exited", Some(0), None);
        let crashed = container("exited", Some(1), None);
        let sick = container("running", None, Some("unhealthy"));
        let looping = container("restarting", None, None);

        assert_eq!(aggregate(&[]), StackStatus::Stopped);
        assert_eq!(
            aggregate(&[up.clone(), healthy.clone()]),
            StackStatus::Running
        );
        assert_eq!(
            aggregate(&[up.clone(), clean_exit.clone()]),
            StackStatus::Partial
        );
        assert_eq!(
            aggregate(std::slice::from_ref(&clean_exit)),
            StackStatus::Stopped
        );
        assert_eq!(aggregate(&[up.clone(), crashed]), StackStatus::Error);
        assert_eq!(aggregate(&[up.clone(), sick]), StackStatus::Error);
        assert_eq!(aggregate(&[up, looping]), StackStatus::Error);
    }

    #[test]
    fn parses_status_text() {
        assert_eq!(parse_exit_code("Exited (137) 2 hours ago"), Some(137));
        assert_eq!(parse_exit_code("Exited (0) 5 seconds ago"), Some(0));
        assert_eq!(parse_exit_code("Up 3 hours"), None);
        assert_eq!(
            health_from_status("Up 3 hours (healthy)").as_deref(),
            Some("healthy")
        );
        assert_eq!(
            health_from_status("Up 1 second (health: starting)").as_deref(),
            Some("starting")
        );
        assert_eq!(
            health_from_status("Up 2 days (unhealthy)").as_deref(),
            Some("unhealthy")
        );
        assert_eq!(health_from_status("Up 2 days"), None);
    }

    #[test]
    fn memory_excludes_cache() {
        let mem = ContainerMemoryStats {
            usage: Some(100),
            stats: Some(HashMap::from([("inactive_file".to_string(), 30)])),
            ..Default::default()
        };
        assert_eq!(memory_in_use(&mem), Some(70));
        let v1 = ContainerMemoryStats {
            usage: Some(100),
            stats: Some(HashMap::from([("total_inactive_file".to_string(), 120)])),
            ..Default::default()
        };
        assert_eq!(memory_in_use(&v1), Some(0));
    }

    #[test]
    fn splits_partial_lines() {
        let mut pending = String::new();
        assert_eq!(split_lines(&mut pending, "a\nb"), vec!["a"]);
        assert_eq!(split_lines(&mut pending, "c\r\nd\n"), vec!["bc", "d"]);
        assert!(pending.is_empty());
    }

    #[test]
    fn merges_frames_per_stream() {
        let frame = |s: &'static str| axum::body::Bytes::from_static(s.as_bytes());
        let mut buffers = LineBuffers::default();
        let mut lines = Vec::new();
        buffers.push(
            LogOutput::StdOut {
                message: frame("a\nb"),
            },
            &mut lines,
        );
        buffers.push(
            LogOutput::StdErr {
                message: frame("oops\n"),
            },
            &mut lines,
        );
        buffers.push(
            LogOutput::StdOut {
                message: frame("c\n"),
            },
            &mut lines,
        );
        buffers.push(
            LogOutput::StdIn {
                message: frame("ignored\n"),
            },
            &mut lines,
        );
        let line = |s, t: &str| LogLine { s, t: t.into() };
        assert_eq!(lines, [line("o", "a"), line("e", "oops"), line("o", "bc")]);
    }

    #[test]
    fn container_ref_validation() {
        assert!(valid_container_ref("3f2a9c1e5b7d"));
        assert!(valid_container_ref("caddy-caddy-1"));
        assert!(!valid_container_ref("../x"));
        assert!(!valid_container_ref("a/b"));
        assert!(!valid_container_ref(""));
        assert!(!valid_container_ref("-x"));
    }
}
