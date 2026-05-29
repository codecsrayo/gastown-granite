-- Outbox telemetry: per-row attempt counter + last error + cross-table lifecycle view.
-- Powers dashboards that answer "is the drain stuck and why" without folding the audit log.

ALTER TABLE outbox_events
    ADD COLUMN attempts        INTEGER     NOT NULL DEFAULT 0,
    ADD COLUMN last_attempt_at TIMESTAMPTZ,
    ADD COLUMN last_error      TEXT;

-- Cheap lookup for the "stuck" panel: pending rows ordered by age.
CREATE INDEX IF NOT EXISTS outbox_events_pending_ts
    ON outbox_events (ts)
    WHERE drained_at IS NULL;

-- Lifecycle view: one row per event joining outbox + audit + the matching feed projection's
-- last update timestamp. `drain_latency_s` and `age_s` give panels a single numeric signal
-- without per-panel CASE/EXTRACT noise.
CREATE OR REPLACE VIEW v_event_lifecycle AS
SELECT
    o.event_id,
    o.kind,
    o.correlation_id,
    o.ts                                                        AS event_ts,
    o.inserted_at                                               AS enqueued_at,
    o.drained_at,
    o.attempts,
    o.last_attempt_at,
    o.last_error,
    a.inserted_at                                               AS audit_inserted_at,
    (
        SELECT MAX(fp.updated_at)
        FROM feed_projections fp
        WHERE fp.scope = 'correlation' AND fp.scope_id = o.correlation_id
    )                                                           AS feed_updated_at,
    EXTRACT(EPOCH FROM o.drained_at - o.inserted_at)            AS drain_latency_s,
    EXTRACT(EPOCH FROM now() - o.inserted_at)                   AS age_s
FROM outbox_events o
LEFT JOIN audit_events a USING (event_id);
