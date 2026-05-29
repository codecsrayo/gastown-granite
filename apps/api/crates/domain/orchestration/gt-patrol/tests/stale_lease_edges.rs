//! hq-mcyc.4: edge coverage for `patrol.tick` that the existing stale-lease golden path in
//! `orchestration.rs` doesn't assert directly.
//!
//! The MCP full-cycle test session uncovered two safety-relevant paths:
//!
//! 1. `tick` with no leases at all must be a no-op (the dispatcher has nothing to reclaim).
//! 2. `tick` after a clean `close` must NOT fire `LeaseExpired` even when the wall clock has
//!    moved past the timeout — closing the lease removes it from the tracker before the
//!    deadline check.
//!
//! Both are necessary preconditions for the reactor: a spurious `LeaseExpired` would trigger
//! `cas_release` + `enqueue` on a bead the dispatcher has already retired (silent double-work
//! or, worse, a no-op release that masks a real bug).

use std::time::Duration;

use tokio::sync::mpsc;

use gt_events::Envelope;
use gt_patrol::{actor as patrol, InMemoryPatrolRepo, PatrolEvent};

const LEASE_TIMEOUT: u64 = 30;

#[tokio::test]
async fn tick_with_no_leases_emits_nothing() {
    let (tx, mut rx) = mpsc::channel::<Envelope<PatrolEvent>>(8);
    let patroller = patrol::spawn(InMemoryPatrolRepo::default(), tx);

    // No `register` calls. Tick at any wall clock past the timeout.
    patroller.tick(10 + LEASE_TIMEOUT + 5, LEASE_TIMEOUT).await;

    // Give the actor a beat to drain.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        rx.try_recv().is_err(),
        "tick with empty tracker must not emit any PatrolEvent"
    );

    let (live, expired) = patroller.snapshot().await;
    assert_eq!((live, expired), (0, 0));
}

#[tokio::test]
async fn close_before_tick_suppresses_expiration() {
    let (tx, mut rx) = mpsc::channel::<Envelope<PatrolEvent>>(8);
    let patroller = patrol::spawn(InMemoryPatrolRepo::default(), tx);

    // Register at T=10, close at T=15 (before any heartbeat), tick at T=45 (past timeout).
    patroller.register("b-mcyc-4", "worker-x", 1, 10).await;

    // Drain the register event so the channel isn't blocking later.
    let first = rx.recv().await.expect("register relay open");
    assert!(matches!(first.payload, PatrolEvent::LeaseRegistered { .. }));

    patroller.close("b-mcyc-4").await;
    let closed = rx.recv().await.expect("close relay open");
    assert!(matches!(closed.payload, PatrolEvent::LeaseClosed { .. }));

    patroller.tick(10 + LEASE_TIMEOUT + 5, LEASE_TIMEOUT).await;

    // The tick must NOT fire a LeaseExpired — the tracker is empty after close.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let next = rx.try_recv();
    assert!(
        next.is_err(),
        "expected no further events after close + tick past timeout, got: {next:?}"
    );

    let (live, expired) = patroller.snapshot().await;
    assert_eq!((live, expired), (0, 0));
}

#[tokio::test]
async fn heartbeat_resets_age_so_tick_does_not_expire() {
    let (tx, mut rx) = mpsc::channel::<Envelope<PatrolEvent>>(8);
    let patroller = patrol::spawn(InMemoryPatrolRepo::default(), tx);

    // Register at T=10. Heartbeat at T=25 (15s in, well before the 30s timeout).
    patroller.register("b-mcyc-4-hb", "worker-y", 1, 10).await;
    let _ = rx.recv().await;
    patroller.heartbeat("worker-y", 25).await;
    let _ = rx.recv().await;

    // Tick at T=40: the lease is 30s old by wall clock but only 15s old by last_seen, so
    // strictly within the timeout. No expiration.
    patroller.tick(40, LEASE_TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        rx.try_recv().is_err(),
        "heartbeat should reset the age window; tick at T=40 must not expire"
    );

    let (live, expired) = patroller.snapshot().await;
    assert_eq!(live, 1, "lease still live after heartbeat + early tick");
    assert_eq!(expired, 0);
}
