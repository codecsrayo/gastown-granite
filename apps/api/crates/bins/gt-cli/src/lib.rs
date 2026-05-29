//! `gt-cli` — Rust port of the Go `gt` command surface (Paso 9.A, hq-hapx).
//!
//! Phase 1 shipped thin wrappers over the surface that already exists in Rust. Phase 2
//! (hq-hapx.1) implements the commands whose backend is now available:
//!
//! - **HTTP** against `gt-web`: `agents`, `beads`, `heartbeat`, `feed`, `doctor` (probes).
//! - **MCP** via the `gt-mcp-cli` subprocess: `enqueue`, `rotate`, `done` (`merge.submit`).
//! - **edge passthrough**: `bd` (issue tracker), `ready`/`blocked` (`bd ready`/`bd blocked`).
//! - **edge local**: `prime` (role context from the environment), `daemon` (process lifecycle
//!   over the `gt` server binary).
//!
//! Still a stub: `sling` — needs RealEffects self-host (hq-8iur.6 → hq-oap5.2). It prints its
//! blocker and exits non-zero so scripts fail loudly instead of silently no-op-ing.
//!
//! The HTTP fetchers are `pub` and return typed rows so the integration test can assert on
//! them directly against a real in-process `gt-web`.

use std::path::PathBuf;
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
    about = "Gas Town CLI (Rust port — Paso 9.A, hq-hapx)."
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
    /// Show ready work — forwards to `bd ready` (blocker-aware claimable issues).
    Ready,
    /// Show blocked issues — forwards to `bd blocked`.
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
    /// Output role context for the current shell (reads `GT_ROLE`/`GT_RIG`/`GT_CREW` etc.).
    Prime,
    /// Submit the current work to the merge queue via MCP `merge.submit` (wraps `gt-mcp-cli`).
    Done {
        /// Bead awaiting merge (required).
        #[arg(long)]
        bead: String,
        /// Branch to merge. Defaults to the current git branch.
        #[arg(long)]
        branch: Option<String>,
        /// Channel-event id for auditing. Defaults to a freshly generated ULID.
        #[arg(long = "channel-msg-id")]
        channel_msg_id: Option<String>,
        /// Validate only — does not register the slot (`merge.submit.validate`).
        #[arg(long)]
        validate: bool,
    },
    /// Workspace health checks: gt-web `/health`+`/readyz` probes plus local binary checks.
    Doctor,
    /// Manage the Gas Town daemon (the long-running `gt` server process).
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Emit a message onto a `gt-channel` (`<root>/<channel>/<ulid>.event`). Payload from
    /// `--payload`, `--payload-file`, or stdin. Prints the message id.
    EmitEvent {
        /// Channel name (subdir of the channel root), e.g. `merge`.
        channel: String,
        /// Inline payload string. Mutually exclusive with `--payload-file`.
        #[arg(long)]
        payload: Option<String>,
        /// Read payload from a file (`-` for stdin).
        #[arg(long = "payload-file")]
        payload_file: Option<String>,
    },
    /// Block until a message arrives on a `gt-channel`. By default the message is left on the
    /// channel (peek); pass `--ack` to consume it.
    AwaitEvent {
        channel: String,
        /// Max seconds to wait. Omitted / 0 = wait until a message arrives or interrupted.
        #[arg(long)]
        timeout: Option<u64>,
        /// Consume the message (delete the backing file) after printing.
        #[arg(long)]
        ack: bool,
    },

    // --- Stub: backend not yet in Rust. ---
    /// [BLOCKED] Dispatch work to an agent. Needs RealEffects self-host (hq-oap5.2 / was hq-8iur.6).
    Sling {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Subcommands of `gt daemon`. The Rust "daemon" is the `gt` server binary (bins/gt) — the
/// composition root that runs every actor. `start`/`stop`/`status` manage it via a pidfile;
/// `run` execs it in the foreground for systemd-style supervision.
#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Run the daemon in the foreground (execs the `gt` server binary; for supervisors).
    Run,
    /// Start the daemon in the background (detached) and record its pid.
    Start,
    /// Stop the running daemon (SIGTERM to the recorded pid).
    Stop,
    /// Show whether the daemon is running.
    Status,
    /// Tail the daemon log file.
    Logs {
        /// Number of trailing lines to show.
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Follow the log in real time.
        #[arg(short = 'f', long)]
        follow: bool,
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
        Command::Ready => {
            let mut a = vec!["ready".to_string()];
            if cli.json {
                a.push("--json".to_string());
            }
            run_passthrough("bd", &a).await
        }
        Command::Blocked => {
            let mut a = vec!["blocked".to_string()];
            if cli.json {
                a.push("--json".to_string());
            }
            run_passthrough("bd", &a).await
        }
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
        Command::Prime => {
            run_prime(cli.json);
            Ok(0)
        }
        Command::Done { bead, branch, channel_msg_id, validate } => {
            run_done(bead, branch, channel_msg_id, validate).await
        }
        Command::Doctor => run_doctor(&client, &cfg, cli.json).await,
        Command::Daemon { action } => run_daemon(action).await,
        Command::EmitEvent { channel, payload, payload_file } => {
            run_emit_event(&channel, payload, payload_file, cli.json).await
        }
        Command::AwaitEvent { channel, timeout, ack } => {
            run_await_event(&channel, timeout, ack, cli.json).await
        }

        Command::Sling { .. } => Ok(blocked(
            "sling",
            "RealEffects self-host (hq-oap5.2 / was hq-8iur.6); use Go `gt sling` until then",
        )),
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

// --- prime -----------------------------------------------------------------

/// Agent roles, mirroring the Go taxonomy (`internal/session/identity.go`). Used by `gt prime`
/// to compare the `GT_ROLE` env against the role implied by the current directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Mayor,
    Deacon,
    Boot,
    Dog,
    Overseer,
    Witness,
    Refinery,
    Polecat,
    Crew,
    Unknown,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Mayor => "mayor",
            Role::Deacon => "deacon",
            Role::Boot => "boot",
            Role::Dog => "dog",
            Role::Overseer => "overseer",
            Role::Witness => "witness",
            Role::Refinery => "refinery",
            Role::Polecat => "polecat",
            Role::Crew => "crew",
            Role::Unknown => "unknown",
        }
    }

    fn from_token(s: &str) -> Role {
        match s {
            "mayor" => Role::Mayor,
            "deacon" => Role::Deacon,
            "boot" => Role::Boot,
            "dog" => Role::Dog,
            "overseer" => Role::Overseer,
            "witness" => Role::Witness,
            "refinery" => Role::Refinery,
            "polecat" => Role::Polecat,
            "crew" => Role::Crew,
            _ => Role::Unknown,
        }
    }
}

/// Walk up from `start` for the town-root marker `mayor/town.json`, returning the *outermost*
/// match (a rig that was once a standalone town still carries the marker, so the real town root
/// is the highest one). Mirrors Go `beads.FindTownRoot`.
fn find_town_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = start;
    let mut candidate: Option<PathBuf> = None;
    loop {
        if dir.join("mayor").join("town.json").is_file() {
            candidate = Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return candidate,
        }
    }
}

/// Infer `(role, rig, name)` from `cwd` relative to `town_root`. Port of Go `detectRole`
/// (`internal/cmd/role.go`): the directory layout is the cwd signal `gt prime` compares against
/// `GT_ROLE`. `name` carries the polecat / crew / dog name. The town root itself is neutral.
fn detect_role_from_cwd(
    cwd: &std::path::Path,
    town_root: &std::path::Path,
) -> (Role, Option<String>, Option<String>) {
    let Ok(rel) = cwd.strip_prefix(town_root) else {
        return (Role::Unknown, None, None);
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return (Role::Unknown, None, None);
    }
    match parts[0].as_str() {
        "mayor" => return (Role::Mayor, None, None),
        "deacon" => {
            if parts.len() >= 3 && parts[1] == "dogs" {
                if parts[2] == "boot" {
                    return (Role::Boot, None, None);
                }
                return (Role::Dog, None, Some(parts[2].clone()));
            }
            return (Role::Deacon, None, None);
        }
        _ => {}
    }
    let rig = parts[0].clone();
    if parts.len() >= 2 {
        match parts[1].as_str() {
            "mayor" => return (Role::Mayor, Some(rig), None),
            "witness" => return (Role::Witness, Some(rig), None),
            "refinery" => return (Role::Refinery, Some(rig), None),
            "polecats" if parts.len() >= 3 => {
                return (Role::Polecat, Some(rig), Some(parts[2].clone()))
            }
            "crew" if parts.len() >= 3 => return (Role::Crew, Some(rig), Some(parts[2].clone())),
            _ => {}
        }
    }
    (Role::Unknown, Some(rig), None)
}

/// Parse `GT_ROLE` into `(base role, rig, name)`. Normally a flat token (`witness`, `polecat`,
/// …); the compound forms `<rig>/<role>`, `<rig>/polecats/<name>`, `<rig>/crew/<name>` are also
/// accepted. A documented subset of Go `parseRoleString` — enough for the mismatch check.
fn parse_env_role(raw: &str) -> (Role, Option<String>, Option<String>) {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        return (Role::Unknown, None, None);
    }
    if !s.contains('/') {
        return (Role::from_token(s), None, None);
    }
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 3 {
        match parts[1] {
            "polecats" | "polecat" => {
                return (Role::Polecat, Some(parts[0].into()), Some(parts[2].into()))
            }
            "crew" => return (Role::Crew, Some(parts[0].into()), Some(parts[2].into())),
            _ => {}
        }
    }
    if parts.len() >= 2 {
        let r = Role::from_token(parts[1]);
        if r != Role::Unknown {
            return (r, Some(parts[0].into()), None);
        }
    }
    for p in &parts {
        let r = Role::from_token(p);
        if r != Role::Unknown {
            return (r, None, None);
        }
    }
    (Role::Unknown, None, None)
}

/// Canonical home directory for a role, mirroring Go `getRoleHome`. `None` when the role needs a
/// rig/name we do not have.
fn expected_home(
    role: Role,
    rig: Option<&str>,
    name: Option<&str>,
    town_root: &std::path::Path,
) -> Option<PathBuf> {
    let j = |parts: &[&str]| {
        let mut p = town_root.to_path_buf();
        for s in parts {
            p.push(s);
        }
        p
    };
    match role {
        Role::Mayor => Some(j(&["mayor"])),
        Role::Deacon => Some(j(&["deacon"])),
        Role::Boot => Some(j(&["deacon", "dogs", "boot"])),
        Role::Dog => name.map(|n| j(&["deacon", "dogs", n])),
        Role::Witness => rig.map(|r| j(&[r, "witness"])),
        Role::Refinery => rig.map(|r| j(&[r, "refinery", "rig"])),
        Role::Polecat => match (rig, name) {
            (Some(r), Some(n)) => Some(j(&[r, "polecats", n])),
            _ => None,
        },
        Role::Crew => match (rig, name) {
            (Some(r), Some(n)) => Some(j(&[r, "crew", n])),
            _ => None,
        },
        Role::Overseer | Role::Unknown => None,
    }
}

/// Emit role context for the current shell. The Go `gt prime` injects the full role operating
/// manual; this Rust port surfaces the identity the rest of the system keys off — the `GT_ROLE`
/// env (plus rig/crew/hook bead) and the cwd — and adds the two safety behaviors `gt prime`
/// owns: a prominent **role/location mismatch** warning (`warnRoleMismatch`) and a **seed hint**
/// when `GT_ROLE` is unset but the cwd implies a role. A CLI cannot mutate its parent shell, so
/// the "seed" is an `export` hint rather than a tmux `set-environment` (that is B2 territory).
/// Directive-file expansion is deferred.
fn run_prime(json: bool) {
    let role = env_or("GT_ROLE", "unknown");
    let rig = std::env::var("GT_RIG").ok().filter(|s| !s.is_empty());
    let crew = std::env::var("GT_CREW").ok().filter(|s| !s.is_empty());
    let hook_bead = std::env::var("GT_HOOK_BEAD").ok().filter(|s| !s.is_empty());
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_str = cwd.display().to_string();

    let town_root = find_town_root(&cwd);
    let (cwd_role, cwd_rig, cwd_name) = match &town_root {
        Some(tr) => detect_role_from_cwd(&cwd, tr),
        None => (Role::Unknown, None, None),
    };

    let env_set = role != "unknown";
    let (env_role, env_rig, env_name) = if env_set {
        let (r, cr, cn) = parse_env_role(&role);
        let rig2 = cr.or_else(|| rig.clone());
        let name2 = cn
            .or_else(|| crew.clone())
            .or_else(|| std::env::var("GT_POLECAT").ok().filter(|s| !s.is_empty()));
        (r, rig2, name2)
    } else {
        (Role::Unknown, None, None)
    };

    // Mismatch: the env claims one role, the cwd layout implies a different *known* one.
    let mismatch = env_set && cwd_role != Role::Unknown && cwd_role != env_role;
    let home = town_root
        .as_ref()
        .and_then(|tr| expected_home(env_role, env_rig.as_deref(), env_name.as_deref(), tr));

    if json {
        let _ = print_json(&serde_json::json!({
            "role": role,
            "rig": rig,
            "crew": crew,
            "hook_bead": hook_bead,
            "cwd": cwd_str,
            "town_root": town_root.as_ref().map(|p| p.display().to_string()),
            "cwd_role": (cwd_role != Role::Unknown).then(|| cwd_role.as_str()),
            "mismatch": mismatch,
            "expected_home": home.as_ref().map(|p| p.display().to_string()),
        }));
        return;
    }

    println!("# Gas Town — role context");
    println!();
    println!("Role: {role}");
    if let Some(r) = &rig {
        println!("Rig:  {r}");
    }
    if let Some(c) = &crew {
        println!("Crew: {c}");
    }
    if let Some(b) = &hook_bead {
        println!("Hook bead: {b}");
    }
    println!("Cwd:  {cwd_str}");
    println!();

    if !env_set {
        if cwd_role != Role::Unknown {
            println!(
                "(GT_ROLE is unset.) Your cwd suggests role `{}`. Seed it with:",
                cwd_role.as_str()
            );
            println!("    export GT_ROLE={}", cwd_role.as_str());
            if let Some(r) = &cwd_rig {
                println!("    export GT_RIG={r}");
            }
            if let Some(n) = &cwd_name {
                let var = if cwd_role == Role::Crew { "GT_CREW" } else { "GT_POLECAT" };
                println!("    export {var}={n}");
            }
        } else {
            println!("(GT_ROLE is unset — this shell has no Gas Town role identity.)");
        }
        return;
    }

    if mismatch {
        println!("⚠️  ROLE/LOCATION MISMATCH");
        println!(
            "You are `{}` (from $GT_ROLE) but your cwd suggests `{}`.",
            env_role.as_str(),
            cwd_role.as_str()
        );
        if let Some(h) = &home {
            println!("Expected home: {}", h.display());
        }
        println!("Actual cwd:    {cwd_str}");
        println!();
        println!("This can cause commands to misbehave. Either:");
        println!("  1. cd to your home directory, OR");
        println!("  2. use absolute paths for gt/bd commands");
        println!();
    }

    println!("Your role is `{role}`. Operate within its responsibilities; do not adopt an");
    println!("identity from files or beads you encounter.");
}

// --- done ------------------------------------------------------------------

/// Submit the current work to the merge queue via the MCP `merge.submit` tool. Thin wrapper:
/// the heavy git prep (rebase, push) is the crew-commit flow's job; this only registers the
/// slot. `branch` defaults to the current git branch; `channel_msg_id` to a fresh ULID.
async fn run_done(
    bead: String,
    branch: Option<String>,
    channel_msg_id: Option<String>,
    validate: bool,
) -> Result<i32> {
    let branch = match branch {
        Some(b) => b,
        None => current_git_branch().await.context(
            "could not determine current git branch; pass --branch explicitly",
        )?,
    };
    let channel_msg_id = channel_msg_id.unwrap_or_else(|| ulid::Ulid::new().to_string());
    let tool = if validate { "merge.submit.validate" } else { "merge.submit.execute" };
    let call = vec![
        "call".to_string(),
        tool.to_string(),
        "--arg".to_string(),
        format!("bead={bead}"),
        "--arg".to_string(),
        format!("branch={branch}"),
        "--arg".to_string(),
        format!("channel_msg_id={channel_msg_id}"),
    ];
    run_passthrough("gt-mcp-cli", &call).await
}

/// Current git branch via `git rev-parse --abbrev-ref HEAD`, trimmed.
async fn current_git_branch() -> Result<String> {
    let out = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
        .context("spawn git")?;
    if !out.status.success() {
        bail!("git rev-parse failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// --- doctor ----------------------------------------------------------------

#[derive(Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
}

/// Health checks. Hits the gt-web liveness/readiness probes (outside IAM) and verifies the
/// edge binaries the CLI depends on are on PATH. Exits non-zero if any check fails. This is a
/// pragmatic subset of the Go `gt doctor` (~80 checks over town/rig/dolt state).
async fn run_doctor(client: &reqwest::Client, cfg: &Config, json: bool) -> Result<i32> {
    let mut checks = Vec::new();

    // gt-web liveness.
    checks.push(http_probe(client, cfg, "/health", "gt-web-liveness", &[200]).await);
    // gt-web readiness (200 ready, 503 not-ready — both mean the server answered).
    checks.push(http_probe(client, cfg, "/readyz", "gt-web-readiness", &[200]).await);
    // Edge binaries the CLI shells out to.
    checks.push(binary_present("bd", "beads-binary").await);
    checks.push(binary_present("gt-mcp-cli", "mcp-cli-binary").await);

    let all_ok = checks.iter().all(|c| c.ok);

    if json {
        print_json(&checks)?;
    } else {
        for c in &checks {
            let mark = if c.ok { "✓" } else { "✗" };
            println!("{mark} {:<20} {}", c.name, c.detail);
        }
        println!();
        if all_ok {
            println!("doctor: all {} checks passed", checks.len());
        } else {
            let failed = checks.iter().filter(|c| !c.ok).count();
            println!("doctor: {failed}/{} check(s) failed", checks.len());
        }
    }

    Ok(if all_ok { 0 } else { 1 })
}

async fn http_probe(
    client: &reqwest::Client,
    cfg: &Config,
    path: &str,
    name: &str,
    ok_codes: &[u16],
) -> DoctorCheck {
    let url = format!("{}{path}", cfg.base_url);
    match client.get(&url).send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let ok = ok_codes.contains(&code);
            DoctorCheck {
                name: name.to_string(),
                ok,
                detail: format!("GET {path} -> HTTP {code}"),
            }
        }
        Err(e) => DoctorCheck {
            name: name.to_string(),
            ok: false,
            detail: format!("GET {path} unreachable: {e}"),
        },
    }
}

async fn binary_present(bin: &str, name: &str) -> DoctorCheck {
    let found = tokio::process::Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    DoctorCheck {
        name: name.to_string(),
        ok: found,
        detail: if found {
            format!("`{bin}` on PATH")
        } else {
            format!("`{bin}` not found on PATH")
        },
    }
}

// --- daemon ----------------------------------------------------------------

/// Directory holding the daemon pidfile + log. `GT_DAEMON_DIR` wins; else `$GT_WORKSPACE/daemon`;
/// else `./daemon` relative to cwd.
fn daemon_dir() -> PathBuf {
    if let Ok(d) = std::env::var("GT_DAEMON_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let base = std::env::var("GT_WORKSPACE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| ".".to_string());
    PathBuf::from(base).join("daemon")
}

fn pidfile() -> PathBuf {
    daemon_dir().join("daemon.pid")
}

fn logfile() -> PathBuf {
    daemon_dir().join("daemon.log")
}

/// The server binary the daemon runs. `GT_DAEMON_BIN` overrides; default `gt` (bins/gt).
fn daemon_bin() -> String {
    std::env::var("GT_DAEMON_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "gt".to_string())
}

/// Read the recorded pid, if the pidfile exists and parses.
fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pidfile()).ok()?.trim().parse().ok()
}

/// Is `pid` a live process? Uses `kill -0` (no signal sent), avoiding a libc/nix dependency.
async fn pid_alive(pid: u32) -> bool {
    tokio::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn run_daemon(action: DaemonAction) -> Result<i32> {
    match action {
        DaemonAction::Run => {
            // Foreground: exec the server binary, inheriting stdio. Returns its exit code.
            run_passthrough(&daemon_bin(), &[]).await
        }
        DaemonAction::Start => daemon_start().await,
        DaemonAction::Stop => daemon_stop().await,
        DaemonAction::Status => daemon_status().await,
        DaemonAction::Logs { lines, follow } => daemon_logs(lines, follow).await,
    }
}

async fn daemon_start() -> Result<i32> {
    if let Some(pid) = read_pid() {
        if pid_alive(pid).await {
            eprintln!("daemon already running (PID {pid})");
            return Ok(1);
        }
    }
    let dir = daemon_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create daemon dir {}", dir.display()))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logfile())
        .with_context(|| format!("open log {}", logfile().display()))?;
    let log_err = log.try_clone().context("clone log handle")?;

    // Detached background process: new process group so it survives the shell, stdio to the log.
    let bin = daemon_bin();
    let child = {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .process_group(0)
            .spawn()
            .with_context(|| format!("spawn daemon `{bin}` (is it on PATH?)"))?
    };
    let pid = child.id();
    std::fs::write(pidfile(), pid.to_string()).context("write pidfile")?;
    println!("daemon started (PID {pid})");
    Ok(0)
}

async fn daemon_stop() -> Result<i32> {
    let Some(pid) = read_pid() else {
        eprintln!("daemon is not running (no pidfile)");
        return Ok(1);
    };
    if !pid_alive(pid).await {
        let _ = std::fs::remove_file(pidfile());
        eprintln!("daemon is not running (stale pidfile cleared)");
        return Ok(1);
    }
    let status = tokio::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await
        .context("spawn kill")?;
    if !status.success() {
        bail!("kill -TERM {pid} failed");
    }
    let _ = std::fs::remove_file(pidfile());
    println!("daemon stopped (was PID {pid})");
    Ok(0)
}

async fn daemon_status() -> Result<i32> {
    match read_pid() {
        Some(pid) if pid_alive(pid).await => {
            println!("daemon is running (PID {pid})");
            Ok(0)
        }
        Some(pid) => {
            println!("daemon is not running (stale pidfile, PID {pid})");
            Ok(1)
        }
        None => {
            println!("daemon is not running");
            Ok(1)
        }
    }
}

async fn daemon_logs(lines: usize, follow: bool) -> Result<i32> {
    let log = logfile();
    if !log.exists() {
        bail!("no log file at {}", log.display());
    }
    let mut args = vec!["-n".to_string(), lines.to_string()];
    if follow {
        args.push("-f".to_string());
    }
    args.push(log.display().to_string());
    run_passthrough("tail", &args).await
}

// --- channel events --------------------------------------------------------

/// Resolve the channel root directory consumers (refinery etc.) and the CLI agree on.
///
/// Precedence:
///   1. `GT_CHANNEL_ROOT` env (explicit override).
///   2. `<town_root>/.channels` (town root = walking up for `mayor/town.json`, shared with `gt prime`).
///   3. `$GT_WORKSPACE/.channels`.
///   4. `./.channels` relative to cwd.
fn channel_root() -> PathBuf {
    if let Ok(r) = std::env::var("GT_CHANNEL_ROOT") {
        if !r.is_empty() {
            return PathBuf::from(r);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(tr) = find_town_root(&cwd) {
            return tr.join(".channels");
        }
    }
    if let Ok(ws) = std::env::var("GT_WORKSPACE") {
        if !ws.is_empty() {
            return PathBuf::from(ws).join(".channels");
        }
    }
    PathBuf::from(".channels")
}

/// Testable core: emit `payload` on `<root>/<channel>`, returning the assigned message id.
fn emit_message(
    root: &std::path::Path,
    channel: &str,
    payload: &[u8],
) -> Result<String> {
    let ch = gt_channel::Channel::open(root, channel)
        .map_err(|e| anyhow::anyhow!("open channel `{channel}` under {}: {e}", root.display()))?;
    ch.emit(payload).map_err(|e| anyhow::anyhow!("emit on `{channel}`: {e}"))
}

/// Testable core: wait for one message on `<root>/<channel>`. `timeout` of `None` or `Some(0)`
/// means wait forever (only the caller's `ctrl-c` cancels). `ack` deletes the backing file
/// after read. Returns `None` on timeout, `Some((id, payload))` otherwise. The receiver is held
/// open for the duration so `gt-channel`'s notify watcher stays alive.
async fn await_message(
    root: &std::path::Path,
    channel: &str,
    timeout: Option<u64>,
    ack: bool,
) -> Result<Option<(String, Vec<u8>)>> {
    let ch = gt_channel::Channel::open(root, channel)
        .map_err(|e| anyhow::anyhow!("open channel `{channel}` under {}: {e}", root.display()))?;
    let mut rx = ch
        .subscribe(16)
        .map_err(|e| anyhow::anyhow!("subscribe `{channel}`: {e}"))?;
    let msg_opt = match timeout {
        Some(secs) if secs > 0 => {
            match tokio::time::timeout(std::time::Duration::from_secs(secs), rx.recv()).await {
                Ok(m) => m,
                Err(_) => return Ok(None),
            }
        }
        _ => rx.recv().await,
    };
    let Some(msg) = msg_opt else {
        bail!("channel `{channel}` closed before a message arrived");
    };
    if ack {
        ch.ack(&msg).map_err(|e| anyhow::anyhow!("ack: {e}"))?;
    }
    Ok(Some((msg.id, msg.payload)))
}

/// `gt emit-event <channel>` — write a payload to the channel. Payload source priority:
/// `--payload` (inline) > `--payload-file <path>` (or `-` for stdin) > stdin (default).
async fn run_emit_event(
    channel: &str,
    payload: Option<String>,
    payload_file: Option<String>,
    json: bool,
) -> Result<i32> {
    let bytes: Vec<u8> = match (payload, payload_file) {
        (Some(_), Some(_)) => bail!("pass only one of --payload / --payload-file"),
        (Some(p), None) => p.into_bytes(),
        (None, Some(f)) if f == "-" => read_stdin_bytes()?,
        (None, Some(f)) => std::fs::read(&f).with_context(|| format!("read payload file `{f}`"))?,
        (None, None) => read_stdin_bytes()?,
    };
    let root = channel_root();
    let id = emit_message(&root, channel, &bytes)?;
    if json {
        print_json(&serde_json::json!({
            "channel": channel,
            "id": id,
            "root": root.display().to_string(),
            "bytes": bytes.len(),
        }))?;
    } else {
        println!("{id}");
    }
    Ok(0)
}

/// `gt await-event <channel>` — block for one message; exit 1 on timeout, 130 on `ctrl-c`.
async fn run_await_event(
    channel: &str,
    timeout: Option<u64>,
    ack: bool,
    json: bool,
) -> Result<i32> {
    let root = channel_root();
    let wait = await_message(&root, channel, timeout, ack);
    let result = tokio::select! {
        r = wait => r,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("gt await-event: interrupted");
            return Ok(130);
        }
    };
    match result? {
        None => {
            if json {
                print_json(&serde_json::json!({ "channel": channel, "timed_out": true }))?;
            } else {
                eprintln!(
                    "gt await-event: timed out after {}s",
                    timeout.unwrap_or(0)
                );
            }
            Ok(1)
        }
        Some((id, payload)) => {
            let payload_str = String::from_utf8_lossy(&payload).into_owned();
            if json {
                print_json(&serde_json::json!({
                    "channel": channel,
                    "id": id,
                    "payload": payload_str,
                    "acked": ack,
                }))?;
            } else {
                println!("{id}\t{payload_str}");
            }
            Ok(0)
        }
    }
}

fn read_stdin_bytes() -> Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .context("read payload from stdin")?;
    Ok(buf)
}

// --- shared helpers --------------------------------------------------------

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
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

#[cfg(test)]
mod prime_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_env_role_flat_and_compound() {
        assert_eq!(parse_env_role("witness").0, Role::Witness);
        assert_eq!(parse_env_role("mayor").0, Role::Mayor);
        assert_eq!(parse_env_role("boot").0, Role::Boot);
        assert_eq!(parse_env_role("").0, Role::Unknown);
        assert_eq!(parse_env_role("bogus").0, Role::Unknown);

        let (r, rig, name) = parse_env_role("gastown/witness");
        assert_eq!((r, rig.as_deref(), name), (Role::Witness, Some("gastown"), None));

        let (r, rig, name) = parse_env_role("gastown/polecats/alpha");
        assert_eq!(
            (r, rig.as_deref(), name.as_deref()),
            (Role::Polecat, Some("gastown"), Some("alpha"))
        );

        let (r, rig, name) = parse_env_role("gastown/crew/bob");
        assert_eq!(
            (r, rig.as_deref(), name.as_deref()),
            (Role::Crew, Some("gastown"), Some("bob"))
        );
    }

    #[test]
    fn detect_role_from_cwd_maps_layout() {
        let town = Path::new("/town");
        let check = |sub: &str| detect_role_from_cwd(&town.join(sub), town);

        assert_eq!(check("mayor").0, Role::Mayor);
        assert_eq!(check("mayor/rig").0, Role::Mayor);
        assert_eq!(check("deacon").0, Role::Deacon);
        assert_eq!(check("deacon/dogs/boot").0, Role::Boot);

        let (r, _, name) = check("deacon/dogs/rex");
        assert_eq!((r, name.as_deref()), (Role::Dog, Some("rex")));

        let (r, rig, _) = check("gastown/witness/rig");
        assert_eq!((r, rig.as_deref()), (Role::Witness, Some("gastown")));
        assert_eq!(check("gastown/refinery/rig").0, Role::Refinery);

        let (r, rig, name) = check("gastown/polecats/alpha");
        assert_eq!(
            (r, rig.as_deref(), name.as_deref()),
            (Role::Polecat, Some("gastown"), Some("alpha"))
        );
        let (r, rig, name) = check("gastown/crew/bob");
        assert_eq!(
            (r, rig.as_deref(), name.as_deref()),
            (Role::Crew, Some("gastown"), Some("bob"))
        );

        // Town root + bare rig root are neutral (no role inferred).
        assert_eq!(detect_role_from_cwd(town, town).0, Role::Unknown);
        assert_eq!(check("gastown").0, Role::Unknown);
        // Outside the town root → unknown.
        assert_eq!(detect_role_from_cwd(Path::new("/elsewhere"), town).0, Role::Unknown);
    }

    #[test]
    fn expected_home_matches_go_layout() {
        let town = Path::new("/town");
        assert_eq!(expected_home(Role::Mayor, None, None, town), Some(town.join("mayor")));
        assert_eq!(
            expected_home(Role::Witness, Some("gastown"), None, town),
            Some(town.join("gastown").join("witness"))
        );
        assert_eq!(
            expected_home(Role::Refinery, Some("gastown"), None, town),
            Some(town.join("gastown").join("refinery").join("rig"))
        );
        assert_eq!(
            expected_home(Role::Polecat, Some("gastown"), Some("alpha"), town),
            Some(town.join("gastown").join("polecats").join("alpha"))
        );
        // Missing rig → no home.
        assert_eq!(expected_home(Role::Witness, None, None, town), None);
    }

    #[test]
    fn mismatch_decision() {
        // env witness, cwd refinery → mismatch.
        let env = parse_env_role("witness").0;
        let (cwd, _, _) = detect_role_from_cwd(
            &Path::new("/town").join("gastown/refinery/rig"),
            Path::new("/town"),
        );
        assert_ne!(cwd, Role::Unknown);
        assert!(cwd != env);

        // env witness, cwd witness → no mismatch.
        let (cwd2, _, _) = detect_role_from_cwd(
            &Path::new("/town").join("gastown/witness/rig"),
            Path::new("/town"),
        );
        assert_eq!(cwd2, env);
    }

    #[test]
    fn find_town_root_walks_up_to_outermost_marker() {
        let base = std::env::temp_dir().join(format!(
            "gt-cli-prime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let outer = base.join("town");
        let inner = outer.join("gastown"); // a rig that was once its own town
        let deep = inner.join("witness").join("rig");
        std::fs::create_dir_all(outer.join("mayor")).unwrap();
        std::fs::create_dir_all(inner.join("mayor")).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(outer.join("mayor").join("town.json"), "{}").unwrap();
        std::fs::write(inner.join("mayor").join("town.json"), "{}").unwrap();

        // From deep inside, the outermost marker (outer) wins, not the nested rig.
        assert_eq!(find_town_root(&deep), Some(outer.clone()));
        // No marker above → None.
        assert_eq!(find_town_root(&base), None);

        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;

    fn fresh_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "gt-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emit_then_await_round_trip() {
        let root = fresh_root("ch-rt");
        let id = emit_message(&root, "merge", b"hello").expect("emit");
        let got = await_message(&root, "merge", Some(2), true)
            .await
            .expect("await")
            .expect("not timed out");
        assert_eq!(got.0, id);
        assert_eq!(got.1, b"hello");
        // ack removed the backing file.
        let listing: Vec<_> = std::fs::read_dir(root.join("merge")).unwrap().collect();
        assert!(listing.is_empty(), "channel dir should be empty after ack");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn await_times_out_on_empty_channel() {
        let root = fresh_root("ch-to");
        let got = await_message(&root, "merge", Some(1), true).await.expect("await");
        assert!(got.is_none(), "no message arrived within 1s; expected timeout");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_ack_leaves_message_on_channel() {
        let root = fresh_root("ch-peek");
        let _id = emit_message(&root, "x", b"keep me").expect("emit");
        let got = await_message(&root, "x", Some(2), false)
            .await
            .expect("await")
            .expect("not timed out");
        assert_eq!(got.1, b"keep me");
        // No ack → the .event file is still on disk for the next subscriber.
        let listing: Vec<_> = std::fs::read_dir(root.join("x"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("event"))
            .collect();
        assert_eq!(listing.len(), 1, "peek (no --ack) must leave the file in place");
        let _ = std::fs::remove_dir_all(&root);
    }
}
