//! hq-mysw gate: the SQL activity projection agrees with the in-memory `gt_feed::activity_view`.
//!
//! Drives a synthetic log through the real outbox → drain pipeline (the same path production
//! uses), then asserts the `PgActivity` read-side color-codes each correlation identically to
//! folding the same records with `gt_feed::Curator` + `activity_view` at the same `now`. If the
//! SQL and in-memory thresholds ever diverge, this gate fails — they share `gt_feed::activity`.
//!
//! Skipped without `GT_PG_URL`, same convention as the other contract tests.

use serde_json::json;

use gt_audit::EventRecord;
use gt_feed::{activity, Curator};
use gt_store_pg::{PgActivity, PgOutboxDrain, PgOutboxWriter};

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

#[tokio::test]
async fn pg_activity_projection_matches_feed_view() {
    let Ok(url) = std::env::var("GT_PG_URL") else {
        eprintln!("GT_PG_URL unset — skipping the Postgres activity contract");
        return;
    };
    let writer = PgOutboxWriter::connect(&url).await.expect("connect writer");
    gt_store_pg::ensure_schema(writer.pool())
        .await
        .expect("ensure_schema");
    let drain = PgOutboxDrain::new(writer.pool().clone());
    let pg_activity = PgActivity::new(writer.pool().clone());

    // Fresh slate.
    writer.truncate().await.unwrap();
    pg_activity.truncate().await.unwrap();
    sqlx::query("TRUNCATE TABLE audit_events")
        .execute(writer.pool())
        .await
        .unwrap();

    // Monotonic per-correlation, in log order (the single-writer audit log is append-only and
    // ordered, and the drain processes rows by `seq` — so "last in the log" == "most recent").
    // c-fresh ends at 10:09 (1m old → green); c-stuck ends at 09:50 (20m old → red).
    let log = vec![
        record("e1", "c-fresh", "2026-05-27T10:00:00Z", "agent.spawned"),
        record("e2", "c-fresh", "2026-05-27T10:09:00Z", "agent.heartbeat"),
        record("e3", "c-stuck", "2026-05-27T09:50:00Z", "scheduling.dispatched"),
    ];
    for rec in &log {
        writer.publish(rec).await.unwrap();
    }
    let drained = drain.drain_batch(64).await.unwrap();
    assert_eq!(drained as usize, log.len(), "every row drains");

    let now = activity::parse_epoch_secs("2026-05-27T10:10:00Z").unwrap();

    // GATE: the SQL read-side color-codes each correlation identically to the in-memory
    // `activity_view` over the same log at the same `now` — they share `gt_feed::activity`.
    let state = Curator::fold(&log);
    let view = activity::activity_view(&state, now);
    let expect = |subject: &str| {
        view.iter()
            .find(|r| r.subject == subject)
            .unwrap()
            .activity
            .color
            .clone()
    };

    let fresh = pg_activity.status("c-fresh", now).await.unwrap().unwrap();
    assert_eq!(fresh.color, expect("c-fresh"), "c-fresh color matches feed view");
    assert!(fresh.is_active(), "1m old → green");

    let stuck = pg_activity.status("c-stuck", now).await.unwrap().unwrap();
    assert_eq!(stuck.color, expect("c-stuck"), "c-stuck color matches feed view");
    assert!(stuck.is_stuck(), "20m old → red");

    // `all` returns both, most-recent first.
    let all = pg_activity.all(now).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].subject, "c-fresh", "most-recently-active first");

    // PG-only robustness (beyond the feed view's last-ingested simplification): an out-of-order
    // *older* event for c-fresh must NOT roll its activity backwards — `GREATEST` keeps the max.
    // This guards a future multi-worker drain (`FOR UPDATE SKIP LOCKED`) applying rows out of
    // `seq` order; the single-writer feed view does not model that case.
    writer
        .publish(&record(
            "e4",
            "c-fresh",
            "2026-05-27T10:01:00Z",
            "agent.heartbeat",
        ))
        .await
        .unwrap();
    let _ = drain.drain_batch(64).await.unwrap();
    let after = pg_activity.status("c-fresh", now).await.unwrap().unwrap();
    assert_eq!(after.color, "green", "older event must not roll activity back (GREATEST)");
}
