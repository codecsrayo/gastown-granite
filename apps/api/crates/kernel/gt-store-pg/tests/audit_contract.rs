//! Step 6.f.13 (hq-j9ou.2) gate: the Postgres-backed audit store round-trips an
//! `EventRecord` byte-identically and deduplicates by `event_id` on at-least-once relays.
//!
//! Same convention as the quota contract: skipped on hosts without `GT_PG_URL`, runs
//! against a real Postgres in the container.

use serde_json::json;

use gt_audit::EventRecord;
use gt_store_pg::PgAudit;

#[tokio::test]
async fn audit_postgres_round_trip_and_dedupe() {
    let Ok(url) = std::env::var("GT_PG_URL") else {
        eprintln!("GT_PG_URL unset — skipping the Postgres audit contract");
        return;
    };
    let audit = PgAudit::connect(&url).await.expect("connect Postgres");
    gt_store_pg::ensure_schema(audit.pool())
        .await
        .expect("ensure_schema");
    audit.truncate().await.expect("clean audit_events");

    let rec_a = EventRecord {
        event_id: "evt-a".into(),
        correlation_id: "corr-1".into(),
        causation_id: None,
        ts: "2026-05-28T10:00:00Z".into(),
        kind: "scheduling.enqueue".into(),
        payload: json!({"bead": "b1", "priority": 1}),
    };
    let rec_b = EventRecord {
        event_id: "evt-b".into(),
        correlation_id: "corr-1".into(),
        causation_id: Some("evt-a".into()),
        ts: "2026-05-28T10:00:01Z".into(),
        kind: "scheduling.dispatched".into(),
        payload: json!({"bead": "b1", "worker": "w-1"}),
    };

    audit.append(&rec_a).await.unwrap();
    audit.append(&rec_b).await.unwrap();
    // Idempotent on event_id: replaying the first record must not double-insert.
    audit.append(&rec_a).await.unwrap();

    let rows = audit.read_all().await.unwrap();
    assert_eq!(rows.len(), 2, "dedupe keeps exactly two distinct events");
    assert_eq!(rows[0], rec_a, "round-trip is byte-identical");
    assert_eq!(rows[1], rec_b);
}
