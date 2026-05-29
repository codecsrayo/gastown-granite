//! hq-mcyc.2 regression: a Dolt I/O failure on the bead repo must not strand the dispatcher
//! slot. The reactor's `MergeEvent::Merged` arm releases capacity FIRST, then attempts a
//! best-effort bead status update — so `sched.in_flight` always returns to 0 on completion,
//! even when the repo is broken.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::AppError;
use gt_polecat::{FakeTmux, PolecatLifecycle, SpawnTemplate};
use gt_root::{spawn, RealEffects, RootConfig, SystemClock};
use gt_scheduling::{MarkDispatched, SchedCommand};

/// Repo wrapper that lets the test flip `get` into a hard error after dispatch — emulating a
/// Dolt connection drop between the time the bead was claimed and the merge completed.
struct FailingGet {
    inner: InMemoryBeads,
    fail: AtomicBool,
}

impl FailingGet {
    fn new() -> Self {
        Self {
            inner: InMemoryBeads::default(),
            fail: AtomicBool::new(false),
        }
    }
    fn arm(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }
}

impl BeadRepository for FailingGet {
    fn upsert(
        &self,
        bead: &Bead,
    ) -> impl std::future::Future<Output = Result<(), AppError>> + Send {
        self.inner.upsert(bead)
    }
    fn get(
        &self,
        id: &str,
    ) -> impl std::future::Future<Output = Result<Option<Bead>, AppError>> + Send {
        let fail = self.fail.load(Ordering::SeqCst);
        let inner_fut = self.inner.get(id);
        async move {
            if fail {
                Err(AppError::Other("simulated dolt connection refused".into()))
            } else {
                inner_fut.await
            }
        }
    }
    fn list_by_status(
        &self,
        status: BeadStatus,
    ) -> impl std::future::Future<Output = Result<Vec<Bead>, AppError>> + Send {
        self.inner.list_by_status(status)
    }
    fn cas_claim(
        &self,
        id: &str,
        worker: &str,
    ) -> impl std::future::Future<Output = Result<bool, AppError>> + Send {
        self.inner.cas_claim(id, worker)
    }
    fn cas_release(
        &self,
        id: &str,
        expected_worker: &str,
    ) -> impl std::future::Future<Output = Result<bool, AppError>> + Send {
        self.inner.cas_release(id, expected_worker)
    }
}

#[tokio::test]
async fn merge_merged_releases_capacity_even_when_repo_get_errors() {
    let dir = tempdir();
    let log = dir.join("events.jsonl");

    let lifecycle = PolecatLifecycle::new(Box::new(FakeTmux::new()), template());
    let (effects, quota_slot) = RealEffects::new(lifecycle, test_polecat_supervisor());
    let repo = Arc::new(FailingGet::new());
    repo.inner
        .upsert(&Bead::new("hq-mcyc-2-r1", "regression bead", BeadStatus::Pending, 1))
        .await
        .unwrap();

    let root = spawn(
        repo.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        effects,
        SystemClock,
        &log,
        RootConfig {
            capacity: 1,
            ..RootConfig::default()
        },
    );
    let _ = quota_slot.set(root.quota.clone());

    // Consume the only capacity slot — emulate a dispatcher hand-off without an actual claim.
    root.sched
        .exec(SchedCommand::MarkDispatched(MarkDispatched {
            bead: "hq-mcyc-2-r1".into(),
            worker: "worker-a".into(),
        }))
        .await
        .expect("mark_dispatched");
    let (_, in_flight_after_dispatch) = root.sched.snapshot().await;
    assert_eq!(in_flight_after_dispatch, 1, "dispatch should consume the slot");

    // Trip the repo: any subsequent `repo.get` returns an error. The reactor must not let
    // that error short-circuit `sched.capacity_freed`.
    repo.arm();

    // Drive Submit -> reactor advances to Merging -> Complete -> Merged.
    root.merge
        .submit("hq-mcyc-2-r1", "feat/regression", "msg-r1")
        .await;
    let saw_ready = wait_for(Duration::from_secs(3), || {
        any_kind(root.log_path(), "merge.ready")
    })
    .await;
    assert!(saw_ready, "merge.ready did not appear after submit");

    root.merge
        .complete(
            "hq-mcyc-2-r1",
            "deadbeefcafebabe1234567890abcdef00000002",
        )
        .await;
    let saw_merged = wait_for(Duration::from_secs(3), || {
        any_kind(root.log_path(), "merge.merged")
    })
    .await;
    assert!(saw_merged, "merge.merged did not appear after complete");

    // The capacity must be released even though `repo.get` errored inside the handler.
    let deadline = Instant::now() + Duration::from_secs(3);
    let final_in_flight = loop {
        let (_, in_flight) = root.sched.snapshot().await;
        if in_flight == 0 || Instant::now() >= deadline {
            break in_flight;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        final_in_flight, 0,
        "in_flight stuck at {final_in_flight} after merge.merged with repo error"
    );

    root.shutdown();
}

fn any_kind(path: &std::path::Path, kind: &str) -> bool {
    gt_audit::read_all(path)
        .map(|recs| recs.iter().any(|r| r.kind == kind))
        .unwrap_or(false)
}

fn template() -> SpawnTemplate {
    SpawnTemplate {
        rig: "test".to_string(),
        prefix: "gt".to_string(),
        workdir: std::env::temp_dir(),
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        base_env: vec![("GT_ROLE".to_string(), "polecat".to_string())],
        heartbeat_dir: std::env::temp_dir(),
    }
}

fn test_polecat_supervisor() -> Arc<gt_polecat::PolecatSupervisor> {
    Arc::new(gt_polecat::PolecatSupervisor::new(
        Arc::new(FakeTmux::new()),
        gt_polecat::RestartConfig::default(),
        u32::MAX,
    ))
}

fn tempdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-mcyc-2-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_for(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
