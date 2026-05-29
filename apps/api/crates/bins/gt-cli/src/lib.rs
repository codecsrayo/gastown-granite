//! `gt-cli` — Rust port of the Go `gt` command surface (Paso 9.A, hq-hapx).
//!
//! Phase 1 scope: thin wrappers over the surface that already exists in Rust. Every command
//! is one of three shapes (`docs/10-go-rust-parity.md`):
//!
//! - **HTTP** against `gt-web`: `agents`, `beads`, `heartbeat`, `feed`.
//! - **MCP** via the `gt-mcp-cli` subprocess: `enqueue`, `rotate`.
//! - **edge passthrough**: `bd` shells out to the issue-tracker binary.
//!
//! Commands whose backend is not yet in Rust (`sling`, `prime`, `done`, `doctor`, `daemon`)
//! are stubbed: they print which bead unblocks them and exit non-zero, so a script that
//! depends on them fails loudly instead of silently no-op-ing. They are NOT implemented here.
//!
//! The HTTP fetchers are `pub` and return typed rows so the integration test can assert on
//! them directly against a real in-process `gt-web`.

use std::process::Stdio;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

/// One row of `GET /api/sessions`. Mirrors `gt_web::dto::SessionDto` — defined locally so the
/// CLI binary does not link the whole server crate (gt-web is a dev-dependency only).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub rig: String,
    pub state: String,
    pub role: String,
    pub crew: Option<String>,
}

/// One row of `GET /api/beads`. Mirrors `gt_web::dto::BeadDto`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BeadRow {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: u8,
    pub assignee: Option<String>,
}

/// Connection settings, resolved from the environment.
///
/// - `GT_WEB_BASE` (full URL) wins; else `GT_WEB_BIND` (host:port, scheme prepended); else
///   the gt-web default `http://127.0.0.1:8787`.
/// - `GT_WEB_TOKEN` is the bearer secret. Absent/empty → no `Authorization` header (matches
///   gt-web running in `GT_WEB_AUTH=disabled` dev mode).
#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub token: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let base_url = std::env::var("GT_WEB_BASE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("GT_WEB_BIND").ok().filter(|s| !s.is_empty()).map(|b| {
                    if b.starts_with("http://") || b.starts_with("https://") {
                        b
                    } else {
                        format!("http://{b}")
                    }
                })
            })
            .unwrap_or_else(|| "http://127.0.0.1:8787".to_string());
        let token = std::env::var("GT_WEB_TOKEN").ok().filter(|t| !t.is_empty());
        Self { base_url, token }
    }

    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "gt",
    version,
    about = "Gas Town CLI (Rust port — Paso 9.A, hq-hapx). Phase 1: backend-existing commands."
)]
pub struct Cli {
    /// Emit raw JSON from the API instead of a formatted table.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List active agent sessions (GET /api/sessions).
    Agents {
        /// Filter by role (e.g. `polecat`, `mayor`, `witness`).
        #[arg(long)]
        role: Option<String>,
    },
    /// List beads by dispatch status (GET /api/beads). Valid: pending, dispatched, working,
    /// done, failed (the gt-web `BeadStatus` lifecycle). Default: pending.
    Beads {
        #[arg(long, default_value = "pending")]
        status: String,
    },
    /// [BLOCKED] Show ready issues. Go `gt ready` reads `bd` issue status (ready/blocked),
    /// which is the issue-tracker lifecycle — distinct from gt-web's dispatch `BeadStatus`
    /// (pending/dispatched/working/done/failed). No bd-backed status view in Rust yet.
    Ready,
    /// [BLOCKED] Show blocked issues. Same gap as `ready` — bd issue status, not gt-web dispatch.
    Blocked,
    /// Send a heartbeat to a session (POST /api/nudge).
    Heartbeat {
        /// Target session id (tmux session name).
        session: String,
    },
    /// Tail the live event feed (SSE GET /api/stream). Runs until interrupted.
    Feed,
    /// Enqueue work via MCP `scheduling.enqueue` (wraps `gt-mcp-cli`).
    Enqueue {
        /// Tool argument as `key=value`; repeatable. Forwarded verbatim to gt-mcp-cli.
        #[arg(long = "arg", value_name = "K=V")]
        args: Vec<String>,
    },
    /// Rotate the active quota account via MCP `quota.rotate` (wraps `gt-mcp-cli`).
    Rotate {
        /// Account handle to rotate to. Forwarded as `--arg account=<value>`.
        #[arg(long)]
        account: Option<String>,
        /// Extra tool argument as `key=value`; repeatable.
        #[arg(long = "arg", value_name = "K=V")]
        args: Vec<String>,
    },
    /// Passthrough to the `bd` issue-tracker binary (stdio inherited, exit code propagated).
    Bd {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    // --- Stubs: backend not yet in Rust. Each names its blocker bead. ---
    /// [BLOCKED] Dispatch work to an agent. Needs RealEffects self-host (hq-8iur.6).
    Sling {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// [BLOCKED] Output role context for the current cwd. Needs role-taxonomy CLI (hq-92z9 wiring).
    Prime,
    /// [BLOCKED] Signal work ready for the merge queue. Needs gt-channel CLI surface.
    Done {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// [BLOCKED] Workspace health checks. Needs the reconciliation suite ported.
    Doctor,
    /// [BLOCKED] Run the long-running daemon. Needs the supervisor port (Paso 9.E).
    Daemon {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Run one CLI invocation. Returns the process exit code (0 = success, 2 = blocked/usage,
/// other = propagated from a subprocess).
pub async fn run(cli: Cli, cfg: Config) -> Result<i32> {
    let client = reqwest::Client::new();
    match cli.command {
        Command::Agents { role } => {
            let rows = fetch_sessions(&client, &cfg, role.as_deref()).await?;
            if cli.json {
                print_json(&rows)?;
            } else {
                print_sessions(&rows);
            }
            Ok(0)
        }
        Command::Beads { status } => list_beads(&client, &cfg, &status, cli.json).await,
        Command::Ready => Ok(blocked(
            "ready",
            "bd issue-status views (ready/blocked) not ported — gt-web BeadStatus is the dispatch lifecycle only; use `gt bd` or Go `gt ready`",
        )),
        Command::Blocked => Ok(blocked(
            "blocked",
            "bd issue-status views (ready/blocked) not ported — use `gt bd` or Go `gt blocked`",
        )),
        Command::Heartbeat { session } => {
            let accepted = nudge(&client, &cfg, &session).await?;
            if cli.json {
                println!("{}", serde_json::json!({ "accepted": accepted }));
            } else {
                println!("heartbeat {} -> {}", session, if accepted { "accepted" } else { "rejected" });
            }
            Ok(if accepted { 0 } else { 1 })
        }
        Command::Feed => {
            tail_feed(&client, &cfg).await?;
            Ok(0)
        }
        Command::Enqueue { args } => {
            let mut call = vec!["call".to_string(), "scheduling.enqueue".to_string()];
            for a in args {
                call.push("--arg".to_string());
                call.push(a);
            }
            run_passthrough("gt-mcp-cli", &call).await
        }
        Command::Rotate { account, args } => {
            let mut call = vec!["call".to_string(), "quota.rotate".to_string()];
            if let Some(acc) = account {
                call.push("--arg".to_string());
                call.push(format!("account={acc}"));
            }
            for a in args {
                call.push("--arg".to_string());
                call.push(a);
            }
            run_passthrough("gt-mcp-cli", &call).await
        }
        Command::Bd { args } => run_passthrough("bd", &args).await,

        Command::Sling { .. } => Ok(blocked("sling", "hq-8iur.6 (RealEffects self-host); use Go `gt sling` until then")),
        Command::Prime => Ok(blocked("prime", "role-taxonomy CLI wiring on hq-92z9; use Go `gt prime` until then")),
        Command::Done { .. } => Ok(blocked("done", "gt-channel CLI surface; use Go `gt done` until then")),
        Command::Doctor => Ok(blocked("doctor", "reconciliation suite not yet ported; use Go `gt doctor`")),
        Command::Daemon { .. } => Ok(blocked("daemon", "supervisor port (Paso 9.E); use Go `gt daemon`")),
    }
}

/// `GET /api/sessions[?role=]` → typed rows.
pub async fn fetch_sessions(
    client: &reqwest::Client,
    cfg: &Config,
    role: Option<&str>,
) -> Result<Vec<SessionRow>> {
    let mut url = format!("{}/api/sessions", cfg.base_url);
    if let Some(r) = role {
        url.push_str(&format!("?role={r}"));
    }
    fetch_json(authed(client.get(url), cfg)).await
}

/// `GET /api/beads?status=` → typed rows.
pub async fn fetch_beads(
    client: &reqwest::Client,
    cfg: &Config,
    status: &str,
) -> Result<Vec<BeadRow>> {
    let url = format!("{}/api/beads?status={status}", cfg.base_url);
    fetch_json(authed(client.get(url), cfg)).await
}

/// `POST /api/nudge` → accepted flag.
pub async fn nudge(client: &reqwest::Client, cfg: &Config, session: &str) -> Result<bool> {
    let url = format!("{}/api/nudge", cfg.base_url);
    let resp = authed(client.post(url), cfg)
        .json(&serde_json::json!({ "session": session }))
        .send()
        .await
        .context("nudge request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("POST /api/nudge -> HTTP {status}: {body}");
    }
    #[derive(Deserialize)]
    struct NudgeResp {
        accepted: bool,
    }
    let r: NudgeResp = resp.json().await.context("decode nudge response")?;
    Ok(r.accepted)
}

async fn list_beads(client: &reqwest::Client, cfg: &Config, status: &str, json: bool) -> Result<i32> {
    let rows = fetch_beads(client, cfg, status).await?;
    if json {
        print_json(&rows)?;
    } else {
        print_beads(&rows);
    }
    Ok(0)
}

/// Stream the SSE feed, printing each `data:` payload as one line. Runs until the stream ends
/// or the process is interrupted (Ctrl-C).
async fn tail_feed(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    let url = format!("{}/api/stream", cfg.base_url);
    let resp = authed(client.get(url), cfg)
        .header("accept", "text/event-stream")
        .send()
        .await
        .context("open SSE stream")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("GET /api/stream -> HTTP {status}: {body}");
    }
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                        // SSE frames are newline-delimited; emit the data payload of each.
                        while let Some(nl) = buf.find('\n') {
                            let line = buf[..nl].trim_end_matches('\r').to_string();
                            buf.drain(..=nl);
                            if let Some(data) = line.strip_prefix("data:") {
                                println!("{}", data.trim());
                            }
                        }
                    }
                    Some(Err(e)) => return Err(anyhow::Error::new(e).context("SSE stream error")),
                    None => break,
                }
            }
        }
    }
    Ok(())
}

/// Attach the bearer header if a token is configured.
fn authed(req: reqwest::RequestBuilder, cfg: &Config) -> reqwest::RequestBuilder {
    match &cfg.token {
        Some(t) => req.bearer_auth(t),
        None => req,
    }
}

/// Send a GET-style request and decode JSON, turning a non-2xx into an error with the body.
async fn fetch_json<T: for<'de> Deserialize<'de>>(req: reqwest::RequestBuilder) -> Result<T> {
    let resp = req.send().await.context("request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("HTTP {status}: {body}");
    }
    resp.json::<T>().await.context("decode JSON response")
}

/// Forward to an external binary with inherited stdio; return its exit code.
async fn run_passthrough(bin: &str, args: &[String]) -> Result<i32> {
    let status = tokio::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("failed to spawn `{bin}` (is it on PATH?)"))?;
    Ok(status.code().unwrap_or(1))
}

fn blocked(cmd: &str, reason: &str) -> i32 {
    eprintln!("gt {cmd}: not yet ported to Rust — blocked by {reason}.");
    2
}

fn print_json<T: serde::Serialize>(rows: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(rows)?);
    Ok(())
}

fn print_sessions(rows: &[SessionRow]) {
    if rows.is_empty() {
        println!("(no sessions)");
        return;
    }
    println!("{:<28} {:<12} {:<9} {:<10} {}", "ID", "RIG", "STATE", "ROLE", "CREW");
    for r in rows {
        println!(
            "{:<28} {:<12} {:<9} {:<10} {}",
            r.id,
            r.rig,
            r.state,
            r.role,
            r.crew.as_deref().unwrap_or("-")
        );
    }
}

fn print_beads(rows: &[BeadRow]) {
    if rows.is_empty() {
        println!("(no beads)");
        return;
    }
    println!("{:<16} {:<10} {:<4} {:<14} {}", "ID", "STATUS", "PRI", "ASSIGNEE", "TITLE");
    for r in rows {
        println!(
            "{:<16} {:<10} {:<4} {:<14} {}",
            r.id,
            r.status,
            r.priority,
            r.assignee.as_deref().unwrap_or("-"),
            r.title
        );
    }
}
