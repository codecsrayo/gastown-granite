//! Gate tests for `gt-feed` (Paso 6.g).
//!
//! 1. Deterministic replay — `Curator::fold` over the same log twice yields byte-identical
//!    `FeedState` (Paso 3 gate carried into the feed).
//! 2. `detect` flags `UnhandledEvent` when the dead-letter source has one.
//! 3. `detect` flags `DeadLetterDrain` for a handler error.
//! 4. `detect` flags `TimeoutMissed` when a correlation closes on an explicit failure marker
//!    without any success marker.
//! 5. `detect` stays quiet when a failure marker is paired with a later success marker
//!    (recovery — re-enqueue + merged).

use serde_json::json;

use gt_audit::EventRecord;
use gt_feed::{detect, Curator, DeadEntry, FeedProblem, FeedState};

fn record(event_id: &str, correlation_id: &str, ts: &str, kind: &str) -> EventRecord {
    EventRecord {
        event_id: event_id.into(),
        correlation_id: correlation_id.into(),
        causation_id: None,
        ts: ts.into(),
        kind: kind.into(),
        payload: json!({}),
    }
}

#[test]
fn replay_is_byte_identical() {
    let log = vec![
        record("e1", "c1", "2026-05-27T10:00:00Z", "agent.spawned"),
        record("e2", "c1", "2026-05-27T10:00:01Z", "agent.heartbeat"),
        record("e3", "c2", "2026-05-27T10:00:02Z", "scheduling.enqueue"),
        record("e4", "c2", "2026-05-27T10:00:03Z", "scheduling.dispatched"),
    ];

    let a = Curator::fold(&log);
    let b = Curator::fold(&log);

    assert_eq!(a, b, "FeedState must be deterministic over the same log");
    let a_json = serde_json::to_string(&a).unwrap();
    let b_json = serde_json::to_string(&b).unwrap();
    assert_eq!(a_json, b_json, "serialized FeedState must be byte-identical");

    assert_eq!(a.total_events, 4);
    assert_eq!(a.correlations.len(), 2);
    assert_eq!(a.kind_totals.get("agent.heartbeat"), Some(&1));
}

#[test]
fn fold_equals_streaming_apply() {
    let log = vec![
        record("e1", "c1", "2026-05-27T10:00:00Z", "agent.spawned"),
        record("e2", "c1", "2026-05-27T10:00:01Z", "agent.heartbeat"),
    ];

    let folded = Curator::fold(&log);
    let mut streamed = FeedState::new();
    for rec in &log {
        Curator::apply(&mut streamed, rec);
    }
    assert_eq!(folded, streamed);
}

#[test]
fn detect_flags_unhandled_event() {
    let state = FeedState::new();
    let dead = vec![DeadEntry::Unhandled {
        kind: "agent.spawned".into(),
        event_id: "e1".into(),
        correlation_id: Some("c1".into()),
    }];

    let problems = detect(&state, &dead);

    assert_eq!(problems.len(), 1);
    assert!(matches!(
        problems[0],
        FeedProblem::UnhandledEvent { ref kind, .. } if kind == "agent.spawned"
    ));
}

#[test]
fn detect_flags_handler_error_as_dead_letter_drain() {
    let state = FeedState::new();
    let dead = vec![DeadEntry::HandlerError {
        kind: "merge.ready".into(),
        error: "repo upsert failed: disk full".into(),
    }];

    let problems = detect(&state, &dead);

    assert_eq!(problems.len(), 1);
    assert!(matches!(
        problems[0],
        FeedProblem::DeadLetterDrain { ref kind, .. } if kind == "merge.ready"
    ));
}

#[test]
fn detect_flags_timeout_missed_when_failure_has_no_success() {
    let log = vec![
        record("e1", "c1", "2026-05-27T10:00:00Z", "scheduling.enqueue"),
        record("e2", "c1", "2026-05-27T10:00:01Z", "scheduling.dispatched"),
        record("e3", "c1", "2026-05-27T10:00:30Z", "scheduling.dispatch_timeout"),
    ];

    let state = Curator::fold(&log);
    let problems = detect(&state, &[]);

    assert_eq!(problems.len(), 1, "timeout without success should flag");
    match &problems[0] {
        FeedProblem::TimeoutMissed {
            correlation_id,
            terminal_kind,
            ..
        } => {
            assert_eq!(correlation_id, "c1");
            assert_eq!(terminal_kind, "scheduling.dispatch_timeout");
        }
        other => panic!("expected TimeoutMissed, got {other:?}"),
    }
}

#[test]
fn detect_quiet_when_failure_followed_by_success_recovery() {
    let log = vec![
        record("e1", "c1", "2026-05-27T10:00:00Z", "patrol.lease_registered"),
        record("e2", "c1", "2026-05-27T10:00:30Z", "patrol.lease_expired"),
        record("e3", "c1", "2026-05-27T10:00:40Z", "patrol.lease_closed"),
    ];

    let state = Curator::fold(&log);
    let problems = detect(&state, &[]);

    assert!(
        problems.is_empty(),
        "recovery (expired + later success) must not flag; got {problems:?}",
    );
}
