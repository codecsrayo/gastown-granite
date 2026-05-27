//! Paso 6 gate (docs/08-getting-started.md): the lease promised by Paso 5 actually works.
//! Three domains + a Dolt-port (in-memory) + replay:
//! `enqueue → claim → spawn → polecat dies → patrol tick → LeaseExpired → cas_release →
//! re-enqueue → claim by a fresh worker → completion`. Replay of each domain's log slice
//! rebuilds the same trajectory (determinism rule, docs/06).
//!
//! This test plays the composition-root role: it receives events from both domain relays
//! and wires the cross-domain reaction (`gt-patrol` never imports `gt-scheduling` or
//! `gt-beads`; everything is integration-by-events, as in Paso 5).

use std::sync::Arc;

use tokio::sync::mpsc;

use gt_agent::{actor as agent_actor, Session, SessionState};
use gt_audit::{read_all, replay, EventRecord, EventStore, JsonlWriter};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::{Envelope, EventKind};
use gt_patrol::{actor as patrol, PatrolEvent, PatrolState};
use gt_scheduling::{actor as sched, SchedEvent, SchedState};

const LEASE_TIMEOUT: u64 = 30;

#[tokio::test]
async fn orchestrated_flow_with_stale_polecat_recovers_via_patrol() {
    let repo = Arc::new(InMemoryBeads::default());
    repo.upsert(&Bead::new("b1", "work", BeadStatus::Pending, 1)).await.unwrap();

    let (sched_tx, mut sched_rx) = mpsc::channel::<Envelope<SchedEvent>>(64);
    let (patrol_tx, mut patrol_rx) = mpsc::channel::<Envelope<PatrolEvent>>(64);

    // capacity = 1 so only one polecat in flight; forces the second dispatch to wait for
    // the lease to expire and free the slot.
    let dispatcher = sched::spawn(repo.clone(), sched_tx, 1);
    let patroller = patrol::spawn(patrol_tx);
    let agent = agent_actor::spawn(32);

    let log_path = std::env::temp_dir()
        .join(format!("gt-patrol-orch-{}.events.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log_path);
    let writer = JsonlWriter::new(&log_path);

    // T=10s: edge enqueues the bead.
    dispatcher.enqueue("b1", 1).await;

    // Drive the event loop until the bead has been dispatched twice (first to the doomed
    // worker, second to a fresh one after patrol reclaims the lease).
    let mut dispatched_workers: Vec<String> = Vec::new();
    let mut now_secs: u64 = 10;

    loop {
        tokio::select! {
            Some(env) = sched_rx.recv() => {
                writer.append(&EventRecord::from_envelope(&env).unwrap()).unwrap();
                if let SchedEvent::Dispatched { bead, worker } = &env.payload {
                    dispatched_workers.push(worker.clone());

                    if dispatched_workers.len() == 1 {
                        // first dispatch: register the lease but never heartbeat → goes stale.
                        patroller.register(bead.clone(), worker.clone(), 1, now_secs).await;
                    } else {
                        // second dispatch (after reclaim): register, then run the bead to
                        // completion and close the lease cleanly.
                        patroller.register(bead.clone(), worker.clone(), 1, now_secs).await;

                        agent.add(Session::new(bead.clone(), "granite")).await;
                        agent.transition(bead.clone(), SessionState::Working).await.unwrap();
                        agent.transition(bead.clone(), SessionState::Done).await.unwrap();

                        let mut b = repo.get(bead).await.unwrap().unwrap();
                        b.status = BeadStatus::Done;
                        repo.upsert(&b).await.unwrap();

                        patroller.close(bead.clone()).await;
                        dispatcher.capacity_freed().await;
                        break;
                    }
                }
            }
            Some(env) = patrol_rx.recv() => {
                writer.append(&EventRecord::from_envelope(&env).unwrap()).unwrap();
                if let PatrolEvent::LeaseExpired { bead, worker, priority } = &env.payload {
                    // Composition-root reaction: release the lease in the repo (CAS only
                    // wins if the bead is still dispatched + owned by `worker`) and
                    // re-enqueue. Then free capacity so the dispatcher can pick it up.
                    let released = repo.cas_release(bead, worker).await.unwrap();
                    assert!(released, "patrol's expired lease must still match the repo");
                    dispatcher.capacity_freed().await;
                    dispatcher.enqueue(bead.clone(), *priority).await;
                }
            }
            else => panic!("relays closed before flow completed"),
        }

        // After the first dispatch is in flight and no heartbeat is coming, advance the
        // edge clock past the timeout and fire a patrol tick. Idempotent: extra ticks at
        // the same `now_secs` find no leases to expire.
        if dispatched_workers.len() == 1 {
            now_secs = 10 + LEASE_TIMEOUT + 5;
            patroller.tick(now_secs, LEASE_TIMEOUT).await;
        }
    }

    // First dispatch went to a doomed worker; the second got a fresh one.
    assert_eq!(dispatched_workers.len(), 2);
    assert_ne!(dispatched_workers[0], dispatched_workers[1]);

    // Bead landed on done; no live leases left in the patrol.
    let final_bead = repo.get("b1").await.unwrap().unwrap();
    assert_eq!(final_bead.status, BeadStatus::Done);
    let (live_leases, expired_total) = patroller.snapshot().await;
    assert_eq!(live_leases, 0);
    assert_eq!(expired_total, 1, "exactly one lease should have expired");

    // Dispatcher idle: queue empty, no in-flight.
    let (queued, in_flight) = dispatcher.snapshot().await;
    assert_eq!((queued, in_flight), (0, 0));

    // REPLAY: each domain replays the slice of the log it owns; the log is the single
    // source of truth, like in production.
    let records = read_all(&log_path).unwrap();
    let sched_records: Vec<EventRecord> = records
        .iter()
        .filter(|r| r.kind.starts_with("scheduling."))
        .cloned()
        .collect();
    let patrol_records: Vec<EventRecord> = records
        .iter()
        .filter(|r| r.kind.starts_with("patrol."))
        .cloned()
        .collect();

    let sched_state = replay(&sched_records, SchedState::default(), |s, e: &SchedEvent| s.apply(e)).unwrap();
    assert_eq!(sched_state.enqueued.len(), 2, "initial + reclaim");
    assert_eq!(sched_state.dispatched.len(), 2);
    assert!(sched_state.failed.is_empty());
    assert!(sched_state.timed_out.is_empty(), "scheduling-level timeout is unused; patrol owns it");
    let replayed_workers: Vec<&str> = sched_state.dispatched.iter().map(|(_, w)| w.as_str()).collect();
    assert_eq!(replayed_workers, dispatched_workers.iter().map(|s| s.as_str()).collect::<Vec<_>>());

    let patrol_state = replay(&patrol_records, PatrolState::default(), |s, e: &PatrolEvent| s.apply(e)).unwrap();
    assert_eq!(patrol_state.expired.len(), 1);
    assert_eq!(patrol_state.expired[0].0, "b1");
    assert!(
        patrol_state.tracker.is_empty(),
        "after expire + close, tracker must rebuild to empty",
    );

    // Sanity: every patrol record carries a kind from the patrol prefix.
    for r in &patrol_records {
        assert!(r.kind.starts_with("patrol."), "patrol prefix on {}", r.kind);
    }
    // And every payload round-trips back through EventKind without panic.
    for r in &sched_records {
        let decoded: SchedEvent = r.decode().unwrap();
        assert!(!decoded.kind().is_empty());
    }

    let _ = std::fs::remove_file(&log_path);
}
