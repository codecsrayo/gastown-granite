-- Step 6.f.13 (hq-j9ou.2): canonical audit log in Postgres JSONB (docs/04-persistence.md).
-- `gt-audit::EventRecord` has been the wire/log shape since Step 3; this table is its
-- durable home. The `payload` column is JSONB so Grafana/SQL can index and query the
-- type-erased domain payload directly (operators `->` / `@>`, GIN if needed later).
--
-- `event_id` is the dedupe key for at-least-once relays (docs/04 §Idempotencia): the
-- outbox can re-publish after a crash; `INSERT ... ON CONFLICT (event_id) DO NOTHING`
-- on the audit side turns the relay into exactly-once at the store level.

CREATE TABLE IF NOT EXISTS audit_events (
    event_id        TEXT        PRIMARY KEY,
    correlation_id  TEXT        NOT NULL,
    causation_id    TEXT        NULL,
    ts              TIMESTAMPTZ NOT NULL,
    kind            TEXT        NOT NULL,
    payload         JSONB       NOT NULL,
    inserted_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Time-ordered scans (Grafana time-series panels) + per-kind filters.
CREATE INDEX IF NOT EXISTS audit_events_ts        ON audit_events (ts);
CREATE INDEX IF NOT EXISTS audit_events_kind_ts   ON audit_events (kind, ts);
CREATE INDEX IF NOT EXISTS audit_events_corr      ON audit_events (correlation_id);
