//! Liveness + readiness probes (paso 8.5, hq-8iur.5).
//!
//! Both endpoints sit **outside** the bearer-token middleware in [`crate::router`]: a
//! Kubernetes-style probe (or systemd `ExecStartPost`) does not carry an `Authorization`
//! header, and forcing one would couple the probe to the IAM secret rotation.
//!
//! Contract:
//!
//! - `GET /health` — liveness. The process answered, so `200 OK` always. No checks, no I/O.
//! - `GET /readyz`  — readiness. `200` iff (a) boot hydration is done and (b) every
//!   registered probe returns `Ok`. Otherwise `503` with a JSON body listing which check
//!   failed and why.
//!
//! Boot hydration coordination with paso 8.1 (hq-8iur.1):
//! Today this crate ships with hydration defaulting to `done` — until boot replay lands the
//! API is reachable from the first byte. When 8.1 merges, the boot task will build the gate
//! via [`ReadinessGateBuilder::not_ready`] and flip the flag through
//! [`HydrationHandle::set`] after the replay completes. The signature is stable across that
//! handoff: existing callers keep working.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Serialize;

type ProbeFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;
type ProbeFn = dyn Fn() -> ProbeFuture + Send + Sync + 'static;

struct Probe {
    name: String,
    run: Arc<ProbeFn>,
}

struct Inner {
    hydration: AtomicBool,
    probes: Vec<Probe>,
    timeout: Duration,
}

/// Shared readiness gate carried by the router. Cheap to clone (Arc-backed).
#[derive(Clone)]
pub struct ReadinessGate {
    inner: Arc<Inner>,
}

impl ReadinessGate {
    /// Always-ready gate with no probes — the default for unit tests and dev runs without
    /// real persistence wired.
    pub fn ready() -> Self {
        ReadinessGateBuilder::new().build()
    }

    /// Always-NOT-ready gate, no probes — useful when a caller wants to compose probes via
    /// [`HydrationHandle`] *and* leave hydration false until the boot task flips it.
    pub fn pending_hydration() -> Self {
        ReadinessGateBuilder::new().not_ready().build()
    }

    /// Hand off a cloneable handle the boot/hydration task uses to flip the readiness flag.
    /// Stable across the paso 8.1 handoff: 8.1 calls `handle.set(true)` after replay.
    pub fn hydration_handle(&self) -> HydrationHandle {
        HydrationHandle {
            inner: self.inner.clone(),
        }
    }

    async fn evaluate(&self) -> ReadinessReport {
        let hydration_done = self.inner.hydration.load(Ordering::SeqCst);
        let mut checks = Vec::with_capacity(self.inner.probes.len());
        let mut all_pass = true;
        for probe in &self.inner.probes {
            let fut = (probe.run)();
            let status = match tokio::time::timeout(self.inner.timeout, fut).await {
                Ok(Ok(())) => CheckStatus::Pass,
                Ok(Err(reason)) => CheckStatus::Fail { reason },
                Err(_) => CheckStatus::Fail {
                    reason: format!("timeout after {:?}", self.inner.timeout),
                },
            };
            if matches!(status, CheckStatus::Fail { .. }) {
                all_pass = false;
            }
            checks.push(NamedCheck {
                name: probe.name.clone(),
                status,
            });
        }
        ReadinessReport {
            ready: hydration_done && all_pass,
            hydration_done,
            checks,
        }
    }
}

impl Default for ReadinessGate {
    fn default() -> Self {
        Self::ready()
    }
}

/// Builder for [`ReadinessGate`]. Mutates owned state, then `build()` freezes it into the
/// shared `Arc<Inner>` — no `Arc::get_mut` gymnastics at registration time.
pub struct ReadinessGateBuilder {
    hydration_default: bool,
    probes: Vec<Probe>,
    timeout: Duration,
}

impl ReadinessGateBuilder {
    pub fn new() -> Self {
        Self {
            hydration_default: true,
            probes: Vec::new(),
            timeout: Duration::from_secs(2),
        }
    }

    /// Start in the "not yet hydrated" state — the producer flips the flag via
    /// [`HydrationHandle::set`] once replay completes.
    pub fn not_ready(mut self) -> Self {
        self.hydration_default = false;
        self
    }

    /// Override the per-probe timeout (default `2s`). Probes are run concurrently with the
    /// HTTP request, but a probe that exceeds the timeout counts as a fail — `readyz` must
    /// never block on a hung backend.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Register one async probe. `name` identifies it in the failing-checks JSON body.
    pub fn with_probe<F, Fut>(mut self, name: impl Into<String>, probe: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let probe = Arc::new(probe);
        let run: Arc<ProbeFn> = Arc::new(move || {
            let p = probe.clone();
            Box::pin(async move { p().await })
        });
        self.probes.push(Probe {
            name: name.into(),
            run,
        });
        self
    }

    pub fn build(self) -> ReadinessGate {
        ReadinessGate {
            inner: Arc::new(Inner {
                hydration: AtomicBool::new(self.hydration_default),
                probes: self.probes,
                timeout: self.timeout,
            }),
        }
    }
}

impl Default for ReadinessGateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Cloneable producer-side handle. The hydrating task (paso 8.1) flips readiness through this
/// without touching the gate itself.
#[derive(Clone)]
pub struct HydrationHandle {
    inner: Arc<Inner>,
}

impl HydrationHandle {
    pub fn set(&self, done: bool) {
        self.inner.hydration.store(done, Ordering::SeqCst);
    }

    pub fn is_done(&self) -> bool {
        self.inner.hydration.load(Ordering::SeqCst)
    }
}

#[derive(Serialize)]
struct ReadinessReport {
    ready: bool,
    hydration_done: bool,
    checks: Vec<NamedCheck>,
}

#[derive(Serialize)]
struct NamedCheck {
    name: String,
    #[serde(flatten)]
    status: CheckStatus,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CheckStatus {
    Pass,
    Fail { reason: String },
}

/// `GET /health` — liveness. Cheap, allocation-light: the process answered.
pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

/// `GET /readyz` — readiness. Runs every registered probe with the gate's timeout and
/// returns `200` iff hydration is done AND every probe passes; `503` otherwise.
pub async fn readyz(State(gate): State<ReadinessGate>) -> impl IntoResponse {
    let report = gate.evaluate().await;
    let status = if report.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ready_by_default() {
        let gate = ReadinessGate::ready();
        let report = gate.evaluate().await;
        assert!(report.ready);
        assert!(report.hydration_done);
        assert!(report.checks.is_empty());
    }

    #[tokio::test]
    async fn pending_hydration_blocks_ready() {
        let gate = ReadinessGate::pending_hydration();
        let report = gate.evaluate().await;
        assert!(!report.ready);
        assert!(!report.hydration_done);
    }

    #[tokio::test]
    async fn handle_flips_hydration_flag() {
        let gate = ReadinessGate::pending_hydration();
        let handle = gate.hydration_handle();
        assert!(!handle.is_done());
        handle.set(true);
        assert!(handle.is_done());
        assert!(gate.evaluate().await.ready);
    }

    #[tokio::test]
    async fn failing_probe_blocks_ready() {
        let gate = ReadinessGateBuilder::new()
            .with_probe("dolt", || async { Err("connect refused".into()) })
            .build();
        let report = gate.evaluate().await;
        assert!(!report.ready);
        assert_eq!(report.checks.len(), 1);
        assert!(matches!(report.checks[0].status, CheckStatus::Fail { .. }));
    }

    #[tokio::test]
    async fn passing_probe_keeps_ready() {
        let gate = ReadinessGateBuilder::new()
            .with_probe("pg", || async { Ok(()) })
            .build();
        let report = gate.evaluate().await;
        assert!(report.ready);
        assert!(matches!(report.checks[0].status, CheckStatus::Pass));
    }

    #[tokio::test]
    async fn slow_probe_times_out_and_fails() {
        let gate = ReadinessGateBuilder::new()
            .with_timeout(Duration::from_millis(20))
            .with_probe("slow", || async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            })
            .build();
        let report = gate.evaluate().await;
        assert!(!report.ready);
        let CheckStatus::Fail { reason } = &report.checks[0].status else {
            panic!("expected timeout fail");
        };
        assert!(reason.contains("timeout"), "got: {reason}");
    }
}
