//! `hq-fe-auth.2` — HTTP wrapping of the [`gt_login`] PTY driver.
//!
//! Three routes hang off `/api/quota/accounts/:id/login`:
//!
//! - `POST .../login` — start a flow. Mints a `flow_id` (ulid), parks the driver on a
//!   blocking task, and returns 202 with `{flow_id, account}`. A second start while a
//!   flow is in-flight for the same account returns 409 — per-account-lock semantics
//!   formalised in `.4`.
//! - `POST .../login/token` — submit the OAuth token the user pasted back from the
//!   browser. Wakes the driver's `token_source` closure.
//! - `POST .../login/cancel` — abort the flow. Dropping the registry slot closes the
//!   token channel; the driver maps that to [`gt_login::LoginFailure::Cancelled`].
//!
//! Events flow through the running root's `events` broadcast as
//! [`EventRecord`]s with `quota.login_*` kinds. `.3` will refine the SSE wire shape on
//! top of these (the kinds already match the `quota.login_*` strings the bead spec lists).
//!
//! ## Concurrency model
//!
//! The driver itself is *synchronous*: the workspace's `gt-login` crate explicitly
//! avoids tokio so the state machine is replayable from `#[test]`. We bridge with
//! `tokio::task::spawn_blocking`. Cancellation crosses the boundary as a dropped
//! `std::sync::mpsc::Sender`: the driver's `token_source` closure calls
//! `Receiver::recv()`, which returns `Err(_)` once we drop the slot, and the driver
//! reads that as `None` (cancel).

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use gt_agent::SessionQueries;
use gt_audit::EventRecord;
use gt_beads::BeadRepository;
use gt_login::{LoginDriver, LoginEvent, LoginFailure, LoginTimeouts};
use gt_merge::MergeRepository;

use crate::routes::AppError;
use crate::state::AppState;

/// Configuration for spawning the login child + watchdog deadlines (hq-fe-auth.4).
///
/// `url_timeout_secs` bounds the wait between `Started` and `UrlReady` — the CLI
/// should print its OAuth URL within a few seconds; minutes means a stuck binary.
/// `token_timeout_secs` bounds the wait between `UrlReady` and the operator's
/// `/login/token` submission — typically a human in front of a browser, so this is
/// the larger of the two.
///
/// Either set to 0 disables that phase's watchdog (useful in tests). Env knobs:
/// `GT_LOGIN_CMD`, `GT_LOGIN_ARGS`, `GT_LOGIN_URL_TIMEOUT_SECS` (default 30),
/// `GT_LOGIN_TOKEN_TIMEOUT_SECS` (default 300).
#[derive(Debug, Clone)]
pub struct LoginConfig {
    pub program: String,
    pub args: Vec<String>,
    pub url_timeout_secs: u64,
    pub token_timeout_secs: u64,
}

impl LoginConfig {
    pub fn from_env() -> Self {
        let program = std::env::var("GT_LOGIN_CMD").unwrap_or_else(|_| "claude".to_string());
        let args = match std::env::var("GT_LOGIN_ARGS") {
            Ok(s) if !s.is_empty() => s.split_whitespace().map(String::from).collect(),
            _ => vec!["/login".to_string()],
        };
        let url_timeout_secs = std::env::var("GT_LOGIN_URL_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let token_timeout_secs = std::env::var("GT_LOGIN_TOKEN_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);
        Self {
            program,
            args,
            url_timeout_secs,
            token_timeout_secs,
        }
    }
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            program: "claude".to_string(),
            args: vec!["/login".to_string()],
            url_timeout_secs: 30,
            token_timeout_secs: 300,
        }
    }
}

/// Per-account flow slot. Dropped by `cancel` to wake the driver via channel hangup.
struct FlowSlot {
    flow_id: String,
    token_tx: std::sync::mpsc::Sender<String>,
}

/// Account → in-flight flow.
#[derive(Default)]
pub struct LoginRegistry {
    inner: Mutex<HashMap<String, FlowSlot>>,
}

impl LoginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a fresh slot. Returns `Err(existing_flow_id)` if a flow is already in
    /// flight for `account` so the handler can reply 409 with the live flow id (lets
    /// the UI re-attach instead of restarting).
    fn insert(
        &self,
        account: &str,
        flow_id: String,
        token_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("login registry lock");
        if let Some(existing) = guard.get(account) {
            return Err(existing.flow_id.clone());
        }
        guard.insert(
            account.to_string(),
            FlowSlot {
                flow_id,
                token_tx,
            },
        );
        Ok(())
    }

    /// Look up the token sender for `(account, flow_id)`. Returns `None` if no flow is
    /// in flight or `flow_id` does not match (defensive — caller submitted a stale id).
    fn token_tx_for(
        &self,
        account: &str,
        flow_id: &str,
    ) -> Option<std::sync::mpsc::Sender<String>> {
        let guard = self.inner.lock().expect("login registry lock");
        guard
            .get(account)
            .filter(|s| s.flow_id == flow_id)
            .map(|s| s.token_tx.clone())
    }

    /// Drop the slot for `account`. Returns the flow id that was removed, or `None` if
    /// no flow was in flight. Dropping the slot drops the `token_tx` clone we held;
    /// once every clone is dropped, the driver's `recv()` returns `Err` and the
    /// flow transitions to `Failed{Cancelled}`.
    fn remove(&self, account: &str) -> Option<String> {
        let mut guard = self.inner.lock().expect("login registry lock");
        guard.remove(account).map(|s| s.flow_id)
    }
}

// ---------------------------------------------------------------------------
// Wire DTOs.

#[derive(Debug, Clone, Serialize)]
pub struct LoginStartResponse {
    pub flow_id: String,
    pub account: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginTokenRequest {
    pub flow_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginAck {
    pub account: String,
    pub flow_id: String,
}

// ---------------------------------------------------------------------------
// Handlers.

/// `POST /api/quota/accounts/:id/login` — start a login flow.
pub async fn login_start<R, SQ, M>(
    State(state): State<AppState<R, SQ, M>>,
    Path(account): Path<String>,
) -> Result<(StatusCode, Json<LoginStartResponse>), AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    if account.is_empty() {
        return Err(AppError::bad_request("account is empty"));
    }
    let pty = state
        .login_pty
        .clone()
        .ok_or_else(|| AppError::service_unavailable("login pty not wired"))?;

    let flow_id = ulid::Ulid::new().to_string();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    state
        .login_registry
        .insert(&account, flow_id.clone(), tx)
        .map_err(|existing| {
            AppError::conflict(format!(
                "login already in flight for account '{account}' (flow_id={existing})"
            ))
        })?;

    let events_tx = state.events.clone();
    let registry = state.login_registry.clone();
    let cfg = state.login_config.clone();
    let account_label = account.clone();
    let flow_label = flow_id.clone();

    // hq-fe-auth.5 — timeouts now live entirely inside the driver via
    // [`LoginDriver::run_with_timeouts`]. The driver's per-phase watchdog clones a
    // [`gt_login::PtyKiller`] and hard-kills the PTY child on deadline; URL-phase
    // EOF after the kill maps to `Failed{Timeout{phase:"url"}}` (via the `fired`
    // flag joined back into the driver thread); the caller's `token_source` honours
    // the same deadline via `recv_timeout` so `Failed{Timeout{phase:"token"}}`
    // follows the same pattern. No HTTP-side relabel wrapper or tokio watchdog
    // needed — the driver emits the typed terminal state directly. Setting either
    // `*_timeout_secs` to 0 keeps that phase unbounded (used by tests).
    let url_phase = if cfg.url_timeout_secs == 0 {
        std::time::Duration::MAX
    } else {
        std::time::Duration::from_secs(cfg.url_timeout_secs)
    };
    let token_phase = if cfg.token_timeout_secs == 0 {
        std::time::Duration::MAX
    } else {
        std::time::Duration::from_secs(cfg.token_timeout_secs)
    };
    let timeouts = LoginTimeouts::new(url_phase, token_phase);

    let registry_for_task = registry.clone();
    let account_for_task = account_label.clone();
    let flow_for_task = flow_label.clone();
    let events_tx_for_task = events_tx.clone();
    let cfg_for_task = cfg.clone();
    let pty_for_task = pty.clone();

    tokio::task::spawn_blocking(move || {
        let driver = LoginDriver::new(pty_for_task);
        let program = cfg_for_task.program.clone();
        let args: Vec<&str> = cfg_for_task.args.iter().map(String::as_str).collect();
        let events_for_sink = events_tx_for_task.clone();
        let account_for_sink = account_for_task.clone();
        let flow_for_sink = flow_for_task.clone();
        let event_sink = move |evt: LoginEvent| {
            emit_event(&events_for_sink, &account_for_sink, &flow_for_sink, evt);
        };
        // Token-source closure: bound the operator wait by `token_phase`. When the
        // deadline elapses, return None and let the driver's own watchdog produce
        // `Failed{Timeout{phase:"token"}}` (clock-based disambiguation from
        // operator-cancel, which drops the tx and short-circuits this loop).
        let token_source = move |_url: &str| -> Option<String> {
            if token_phase == std::time::Duration::MAX {
                rx.recv().ok()
            } else {
                rx.recv_timeout(token_phase).ok()
            }
        };

        // Panic guard (hq-fe-auth.4): if the driver thread panics mid-flight, the
        // registry slot would otherwise leak. Emit one `Failed{Io}` on the SSE
        // stream so the UI can recover.
        let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            driver.run_with_timeouts(
                &program,
                &args,
                &account_for_task,
                timeouts,
                token_source,
                event_sink,
            )
        }));
        if run_result.is_err() {
            emit_event(
                &events_tx_for_task,
                &account_for_task,
                &flow_for_task,
                LoginEvent::Failed {
                    reason: LoginFailure::Io {
                        message: "driver task panicked".to_string(),
                    },
                },
            );
        }
        // Best-effort cleanup; a successful `cancel` already removed it.
        registry_for_task.remove(&account_for_task);
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(LoginStartResponse { flow_id, account }),
    ))
}

/// `POST /api/quota/accounts/:id/login/token` — forward the operator-supplied token to
/// the in-flight driver.
pub async fn login_token<R, SQ, M>(
    State(state): State<AppState<R, SQ, M>>,
    Path(account): Path<String>,
    Json(req): Json<LoginTokenRequest>,
) -> Result<Json<LoginAck>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    if req.flow_id.is_empty() {
        return Err(AppError::bad_request("flow_id is empty"));
    }
    if req.token.is_empty() {
        return Err(AppError::bad_request("token is empty"));
    }
    let tx = state
        .login_registry
        .token_tx_for(&account, &req.flow_id)
        .ok_or_else(|| AppError::not_found("no in-flight login flow matches account+flow_id"))?;
    // Channel may be closed if the driver already exited (e.g. URL phase failed). Map
    // that to 410 Gone so the UI can re-start cleanly.
    tx.send(req.token)
        .map_err(|_| AppError::gone("login flow already terminated; restart"))?;
    Ok(Json(LoginAck {
        account,
        flow_id: req.flow_id,
    }))
}

/// `POST /api/quota/accounts/:id/login/cancel` — abort the flow.
pub async fn login_cancel<R, SQ, M>(
    State(state): State<AppState<R, SQ, M>>,
    Path(account): Path<String>,
) -> Result<Json<LoginAck>, AppError>
where
    R: BeadRepository + Send + Sync + 'static,
    SQ: SessionQueries + Send + Sync + 'static,
    M: MergeRepository + Send + Sync + 'static,
{
    match state.login_registry.remove(&account) {
        Some(flow_id) => Ok(Json(LoginAck { account, flow_id })),
        None => Err(AppError::not_found(
            "no in-flight login flow for this account",
        )),
    }
}

// ---------------------------------------------------------------------------
// Event plumbing.

fn emit_event(
    events: &broadcast::Sender<EventRecord>,
    account: &str,
    flow_id: &str,
    evt: LoginEvent,
) {
    let dto = match evt {
        LoginEvent::Started => crate::dto::QuotaLoginEvent::Started(crate::dto::QuotaLoginStarted {
            account: account.to_string(),
            flow_id: flow_id.to_string(),
        }),
        LoginEvent::UrlReady { url } => crate::dto::QuotaLoginEvent::UrlReady(
            crate::dto::QuotaLoginUrlReady {
                account: account.to_string(),
                flow_id: flow_id.to_string(),
                url,
            },
        ),
        LoginEvent::Complete { account: a } => crate::dto::QuotaLoginEvent::Complete(
            crate::dto::QuotaLoginComplete {
                account: a,
                flow_id: flow_id.to_string(),
            },
        ),
        LoginEvent::Failed { reason } => crate::dto::QuotaLoginEvent::Failed(
            crate::dto::QuotaLoginFailed {
                account: account.to_string(),
                flow_id: flow_id.to_string(),
                // `LoginFailure` implements `Display` via `thiserror`; the flat
                // `message` lets the UI render without typing the union.
                message: reason.to_string(),
                reason,
            },
        ),
    };
    let rec = EventRecord {
        event_id: ulid::Ulid::new().to_string(),
        correlation_id: flow_id.to_string(),
        causation_id: None,
        ts: rfc3339_now(),
        kind: dto.kind_str().to_string(),
        payload: dto.payload_json(),
    };
    // `send` errors when there are zero receivers; that's expected before any SSE
    // client connects and harmless — drop the frame.
    let _ = events.send(rec);
}

fn rfc3339_now() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_blocks_second_start_returns_live_flow_id() {
        let r = LoginRegistry::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        r.insert("alpha", "FLOW-1".into(), tx).unwrap();
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let collision = r.insert("alpha", "FLOW-2".into(), tx2).unwrap_err();
        assert_eq!(collision, "FLOW-1");
    }

    #[test]
    fn registry_token_tx_matches_only_correct_flow_id() {
        let r = LoginRegistry::new();
        let (tx, rx) = std::sync::mpsc::channel();
        r.insert("alpha", "FLOW-1".into(), tx).unwrap();
        assert!(r.token_tx_for("alpha", "WRONG").is_none());
        let live = r.token_tx_for("alpha", "FLOW-1").unwrap();
        live.send("TOK".into()).unwrap();
        assert_eq!(rx.recv().unwrap(), "TOK");
    }

    #[test]
    fn registry_remove_drops_slot_and_returns_flow_id() {
        let r = LoginRegistry::new();
        let (tx, rx) = std::sync::mpsc::channel();
        r.insert("alpha", "FLOW-1".into(), tx).unwrap();
        let removed = r.remove("alpha");
        assert_eq!(removed.as_deref(), Some("FLOW-1"));
        // The cloned tx held by the registry slot is dropped; the original tx in this
        // test still exists, so recv stays open until *that* tx is dropped too.
        drop(rx);
        assert!(r.remove("alpha").is_none());
    }

    #[test]
    fn emit_event_records_all_four_kinds() {
        // Sanity: each LoginEvent maps to its expected `quota.login_*` kind. Keeps the
        // wire contract reviewable without spinning up a real broadcast.
        let (tx, mut rx) = broadcast::channel(8);
        emit_event(&tx, "alpha", "F", LoginEvent::Started);
        emit_event(
            &tx,
            "alpha",
            "F",
            LoginEvent::UrlReady {
                url: "https://console.anthropic.com/x".into(),
            },
        );
        emit_event(
            &tx,
            "alpha",
            "F",
            LoginEvent::Complete {
                account: "alpha".into(),
            },
        );
        emit_event(
            &tx,
            "alpha",
            "F",
            LoginEvent::Failed {
                reason: LoginFailure::Cancelled,
            },
        );

        let mut kinds = Vec::new();
        while let Ok(rec) = rx.try_recv() {
            kinds.push(rec.kind);
        }
        assert_eq!(
            kinds,
            vec![
                "quota.login_started",
                "quota.login_url_ready",
                "quota.login_complete",
                "quota.login_failed",
            ]
        );
    }

    #[test]
    fn login_config_from_env_defaults() {
        // Unset both vars so a stray test ordering doesn't bleed env into another case.
        std::env::remove_var("GT_LOGIN_CMD");
        std::env::remove_var("GT_LOGIN_ARGS");
        let cfg = LoginConfig::from_env();
        assert_eq!(cfg.program, "claude");
        assert_eq!(cfg.args, vec!["/login"]);
    }

}
