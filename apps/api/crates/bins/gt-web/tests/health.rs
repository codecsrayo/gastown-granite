//! Gate test for paso 8.5 (hq-8iur.5). Verifies the operational-readiness contract end-to-end
//! over a real HTTP listener:
//!
//! 1. `/health` returns 200 with no auth header (liveness, doc 07 §"single entry point").
//! 2. `/readyz` returns 200 with no auth header when the gate is ready + probes pass.
//! 3. `/readyz` returns 503 when the hydration flag is `false` (paso 8.1 handoff).
//! 4. `/readyz` returns 503 when a probe fails — and lists the failing probe by name.
//! 5. The probes endpoints bypass the bearer-token middleware (kube/systemd carry no token).
//!
//! Doc rule (07 §"frontier única"): the auth middleware lives at the gateway. We assert
//! that lifting the probes outside that middleware does not also lift the API routes —
//! `/api/sessions` still returns 401 without a token even when `/health` does not.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::{BeadRepository, InMemoryBeads};
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::{
    router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, ReadinessGateBuilder,
    WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

fn tempfile(stem: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gt-web-health-{stem}-{}.jsonl", ulid::Ulid::new()))
}

async fn boot(auth: AuthConfig, gate: ReadinessGate) -> String {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        tempfile("log"),
        RootConfig::default(),
    );

    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
    };

    let audit = InMemoryWebAudit::new();
    let sink: Arc<dyn WebAuditSink> = Arc::new(audit);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, auth, sink, gate);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Keep the root alive for the duration of the test by leaking the handle into the spawned
    // closure; the OS reclaims everything when the test process exits.
    std::mem::forget(root);
    format!("http://{addr}")
}

#[tokio::test]
async fn health_returns_ok_without_auth() {
    // Auth enabled — but /health must NOT be behind it.
    let base = boot(AuthConfig::bearer("s3cret"), ReadinessGate::ready()).await;
    let res = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readyz_returns_ok_when_ready() {
    let base = boot(AuthConfig::bearer("s3cret"), ReadinessGate::ready()).await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["ready"], true);
    assert_eq!(body["hydration_done"], true);
}

#[tokio::test]
async fn readyz_returns_503_when_hydration_pending() {
    let base = boot(
        AuthConfig::bearer("s3cret"),
        ReadinessGate::pending_hydration(),
    )
    .await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 503);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["ready"], false);
    assert_eq!(body["hydration_done"], false);
}

#[tokio::test]
async fn readyz_flips_to_ok_when_handle_set() {
    let gate = ReadinessGate::pending_hydration();
    let handle = gate.hydration_handle();
    let base = boot(AuthConfig::bearer("s3cret"), gate).await;

    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 503);

    handle.set(true);
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn readyz_reports_failing_probe_by_name() {
    let gate = ReadinessGateBuilder::new()
        .with_probe("dolt", || async {
            Err("connect refused (test)".to_string())
        })
        .with_probe("pg-audit", || async { Ok(()) })
        .build();
    let base = boot(AuthConfig::bearer("s3cret"), gate).await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 503);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["ready"], false);
    let checks = body["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    let dolt = &checks[0];
    assert_eq!(dolt["name"], "dolt");
    assert_eq!(dolt["status"], "fail");
    let pg = &checks[1];
    assert_eq!(pg["name"], "pg-audit");
    assert_eq!(pg["status"], "pass");
}

#[tokio::test]
async fn slow_probe_times_out() {
    let gate = ReadinessGateBuilder::new()
        .with_timeout(Duration::from_millis(50))
        .with_probe("hung", || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        })
        .build();
    let base = boot(AuthConfig::bearer("s3cret"), gate).await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 503);
    let body: serde_json::Value = res.json().await.unwrap();
    let reason = body["checks"][0]["reason"].as_str().unwrap();
    assert!(reason.contains("timeout"), "expected timeout in {reason}");
}

#[tokio::test]
async fn probes_bypass_iam_but_api_routes_do_not() {
    let base = boot(AuthConfig::bearer("s3cret"), ReadinessGate::ready()).await;

    // No Authorization header — health + readyz still pass.
    assert_eq!(
        reqwest::get(format!("{base}/health")).await.unwrap().status(),
        200,
    );
    assert_eq!(
        reqwest::get(format!("{base}/readyz")).await.unwrap().status(),
        200,
    );

    // /api/* without a token is still 401 — auth is not globally lifted.
    let res = reqwest::get(format!("{base}/api/sessions")).await.unwrap();
    assert_eq!(res.status(), 401);

    // /metrics also bypasses auth (Prometheus scrapes anonymously).
    let res = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert_eq!(res.status(), 200);
}

// Suppress unused-import lints — BeadRepository is brought in only to enforce the bound on
// boot's generic; the test body itself does not call any of its methods.
#[allow(dead_code)]
fn _bounds_anchor(_: Arc<InMemoryBeads>) -> impl BeadRepository {
    InMemoryBeads::default()
}
