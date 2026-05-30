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
use gt_login::{LoginDriver, LoginEvent};
#[cfg(test)]
use gt_login::LoginFailure;
use gt_merge::MergeRepository;

use crate::routes::AppError;
use crate::state::AppState;

/// Configuration for spawning the login child. Boot reads `GT_LOGIN_CMD`
/// (defaults `claude`) and `GT_LOGIN_ARGS` (space-separated, defaults `/login`).
#[derive(Debug, Clone)]
pub struct LoginConfig {
    pub program: String,
    pub args: Vec<String>,
}

impl LoginConfig {
    pub fn from_env() -> Self {
        let program = std::env::var("GT_LOGIN_CMD").unwrap_or_else(|_| "claude".to_string());
        let args = match std::env::var("GT_LOGIN_ARGS") {
            Ok(s) if !s.is_empty() => s.split_whitespace().map(String::from).collect(),
            _ => vec!["/login".to_string()],
        };
        Self { program, args }
    }
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            program: "claude".to_string(),
            args: vec!["/login".to_string()],
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

    tokio::task::spawn_blocking(move || {
        let driver = LoginDriver::new(pty);
        let program = cfg.program.clone();
        let args: Vec<&str> = cfg.args.iter().map(String::as_str).collect();
        // The driver hands the URL to us — we ignore it in the token closure (the URL
        // already flowed out via `LoginEvent::UrlReady` → SSE) and just block on the
        // mpsc channel for the operator-submitted token.
        let event_label = account_label.clone();
        let _outcome = driver.run(
            &program,
            &args,
            &account_label,
            move |_url| rx.recv().ok(),
            move |evt| emit_event(&events_tx, &event_label, &flow_label, evt),
        );
        // Clean up the registry slot when the driver exits. `remove` is best-effort —
        // a successful `cancel` already removed it.
        registry.remove(&account_label);
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
