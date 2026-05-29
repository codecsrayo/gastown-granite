//! Paso 6.h (hq-7owq) gate: the doc-04 §3 outbox + read-side projections.
//!
//! Covers the four invariants that make the pipeline crash-safe:
//!
//! 1. `PgOutboxWriter::publish` commits the outbox row AND the `token_usage` row in a
//!    single transaction for `quota.tokens_sampled` (so a partial failure cannot leave
//!    the audit ledger and the consumption table out of sync).
//! 2. `event_id` is the dedupe key: re-publishing the same `EventRecord` is a no-op
//!    (no second outbox row, no second `token_usage` row).
//! 3. `PgOutboxDrain::drain_batch` moves pending rows into `audit_events` +
//!    `feed_projections`, marks them drained, and produces panel-ready aggregates
//!    that match a synthetic load's totals.
//! 4. Replaying the drained log through `gt_feed::Curator` produces the same per-kind
//!    counts the SQL projection exposes — proving the SQL view and the in-memory replay
//!    agree (the doc-04 "feed read-only of the stream" rule).
//!
//! Skipped without `GT_PG_URL`, same convention as the existing contract tests.

use serde_json::json;

use gt_audit::EventRecord;
use gt_feed::Curator;
use gt_quota::QuotaRepository;
use gt_store_pg::{PgFeedProjections, PgOutboxDrain, PgOutboxWriter, PgQuota};

#[tokio::test]
async fn outbox_pipeline_round_trip() {
    let Ok(url) = std::env::var("GT_PG_URL") else {
        eprintln!("GT_PG_URL unset — skipping the Postgres outbox contract");
        return;
    };
    let writer = PgOutboxWriter::connect(&url).await.expect("connect writer");
    gt_store_pg::ensure_schema(writer.pool())
        .await
        .expect("ensure_schema");

    let drain = PgOutboxDrain::new(writer.pool().clone());
    let projections = PgFeedProjections::new(writer.pool().clone());
    let quota = PgQuota::new(writer.pool().clone());

    // Fresh slate every run so we can assert on absolute counts.
    writer.truncate().await.unwrap();
    quota.truncate().await.unwrap();
    projections.truncate().await.unwrap();
    sqlx::query("TRUNCATE TABLE audit_events")
        .execute(writer.pool())
        .await
        .unwrap();

    // Synthetic load: three TokensSampled events on two sessions of the same account,
    // plus one unrelated scheduling event. The token totals are the panel's target.
    let acct = "acct-A";
    let sess1 = "sess-1";
    let sess2 = "sess-2";

    let recs = vec![
        sample_record("evt-1", "corr-1", acct, sess1, 100, 200),
        sample_record("evt-2", "corr-1", acct, sess1, 50, 50),
        sample_record("evt-3", "corr-2", acct, sess2, 300, 700),
        EventRecord {
            event_id: "evt-4".into(),
            correlation_id: "corr-3".into(),
            causation_id: None,
            ts: "2026-05-28T10:00:03Z".into(),
            kind: "scheduling.enqueue".into(),
            payload: json!({"bead": "b1", "priority": 1}),
        },
    ];

    for rec in &recs {
        writer.publish(rec).await.expect("publish");
    }
    // Idempotency: re-publishing the first sample must not double-insert.
    writer.publish(&recs[0]).await.expect("re-publish");

    // 1+2) outbox has exactly the unique events; token_usage has exactly the
    // sample events (re-publishing did NOT add a second token_usage row).
    let pending_before = writer.pending_count().await.unwrap();
    assert_eq!(pending_before, 4, "one row per unique event_id");
    let total_tokens_sess1 = quota
        .session_window_tokens(acct, sess1, 0, 4_000_000_000)
        .await
        .unwrap();
    assert_eq!(total_tokens_sess1, 100 + 200 + 50 + 50);
    let total_tokens_acct = quota
        .account_window_tokens(acct, 0, 4_000_000_000)
        .await
        .unwrap();
    assert_eq!(total_tokens_acct, 100 + 200 + 50 + 50 + 300 + 700);

    // 3) Drain all pending — every row should move into audit_events + feed_projections.
    let drained = drain.drain_batch(64).await.unwrap();
    assert_eq!(drained, 4);
    let pending_after = writer.pending_count().await.unwrap();
    assert_eq!(pending_after, 0, "drain marks every row drained");

    // 3a) Outbox telemetry columns must reflect a successful drain on each row:
    // attempts >= 1, last_attempt_at set, last_error NULL (no failure).
    let telemetry: Vec<(i32, Option<time::OffsetDateTime>, Option<String>)> = sqlx::query_as(
        "SELECT attempts, last_attempt_at, last_error FROM outbox_events ORDER BY seq ASC",
    )
    .fetch_all(writer.pool())
    .await
    .unwrap();
    assert_eq!(telemetry.len(), 4);
    for (i, (attempts, last_at, last_err)) in telemetry.iter().enumerate() {
        assert!(*attempts >= 1, "row {i}: attempts must be at least 1, got {attempts}");
        assert!(last_at.is_some(), "row {i}: last_attempt_at must be set after drain");
        assert!(last_err.is_none(), "row {i}: last_error must be NULL on success");
    }

    // 3b) The lifecycle view joins outbox + audit + projections; every drained row should
    // surface with a non-null drain_latency_s (the column the dashboards key off).
    let lifecycle_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM v_event_lifecycle WHERE drained_at IS NOT NULL AND drain_latency_s IS NOT NULL",
    )
    .fetch_one(writer.pool())
    .await
    .unwrap();
    assert_eq!(lifecycle_count, 4, "v_event_lifecycle exposes every drained row with a latency");

    let acct_tokens = projections
        .get("account", acct, "tokens_total")
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(acct_tokens, (100 + 200 + 50 + 50 + 300 + 700) as i64);

    let sess1_tokens = projections
        .get("session", &format!("{acct}|{sess1}"), "tokens_total")
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(sess1_tokens, (100 + 200 + 50 + 50) as i64);

    let sess2_tokens = projections
        .get("session", &format!("{acct}|{sess2}"), "tokens_total")
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(sess2_tokens, (300 + 700) as i64);

    let sampled_total = projections
        .get("kind", "quota.tokens_sampled", "events_total")
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(sampled_total, 3);

    // Idempotent drain: re-running it on an empty pending set is a no-op.
    let extra = drain.drain_batch(64).await.unwrap();
    assert_eq!(extra, 0);

    // 4) Replay the audit log through the gt-feed reducer. The SQL projection's per-kind
    // total has to agree with the in-memory `FeedState.kind_totals` for the same kind.
    let audit_rows: Vec<EventRecord> = sqlx::query_as::<_, AuditRow>(
        "SELECT event_id, correlation_id, causation_id,
                to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS ts,
                kind, payload::text AS payload
         FROM audit_events
         ORDER BY ts ASC, event_id ASC",
    )
    .fetch_all(writer.pool())
    .await
    .unwrap()
    .into_iter()
    .map(EventRecord::from)
    .collect();
    assert_eq!(audit_rows.len(), 4);
    let state = Curator::fold(&audit_rows);
    assert_eq!(state.kind_totals.get("quota.tokens_sampled"), Some(&3));
    assert_eq!(state.kind_totals.get("scheduling.enqueue"), Some(&1));
}

fn sample_record(
    event_id: &str,
    correlation_id: &str,
    account: &str,
    session: &str,
    input: u64,
    output: u64,
) -> EventRecord {
    // The ts format mirrors what `EventRecord::from_envelope` produces — a Z-suffixed Rfc3339
    // string. The `now_secs` inside the payload is what drives the `token_usage.ts` column;
    // it does NOT have to match the outer `ts` field.
    EventRecord {
        event_id: event_id.into(),
        correlation_id: correlation_id.into(),
        causation_id: None,
        ts: "2026-05-28T10:00:00Z".into(),
        kind: "quota.tokens_sampled".into(),
        payload: json!({
            "account": account,
            "session": session,
            "model": "claude-opus-4-7",
            "input": input,
            "output": output,
            "cache_read": 0,
            "cache_creation": 0,
            "now_secs": 1716_891_600u64,
        }),
    }
}

#[derive(sqlx::FromRow)]
struct AuditRow {
    event_id: String,
    correlation_id: String,
    causation_id: Option<String>,
    ts: String,
    kind: String,
    payload: String,
}

impl From<AuditRow> for EventRecord {
    fn from(r: AuditRow) -> Self {
        EventRecord {
            event_id: r.event_id,
            correlation_id: r.correlation_id,
            causation_id: r.causation_id,
            ts: r.ts,
            kind: r.kind,
            payload: serde_json::from_str(&r.payload).unwrap(),
        }
    }
}
