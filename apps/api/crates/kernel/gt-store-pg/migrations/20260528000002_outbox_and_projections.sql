-- Paso 6.h (hq-7owq): durable outbox + read-side feed projections.
--
-- Doc 04 §3 ("Outbox por cada store que escribe"): every event-producing write happens
-- inside a single PG transaction that also lands the outbox row. A drain task fans the
-- outbox out to `audit_events` (canonical JSONB log) and `feed_projections` (panel-ready
-- aggregates) atomically per row. Replay determinism is preserved because the outbox is
-- byte-identical to what the in-process broadcast delivered: the audit log is still the
-- single source of truth; the outbox just guarantees at-least-once handoff across a crash.
--
-- Idempotency rules:
--   * `outbox_events.event_id` is UNIQUE so producers retrying on transient PG errors do
--     not duplicate rows (matches the doc-04 dedupe rule on `event_id`).
--   * `audit_events` already uses `ON CONFLICT (event_id) DO NOTHING`; the drain task
--     marks the outbox row drained AFTER both audit + projection writes succeed, so a
--     crash mid-drain re-runs them safely.
--   * `feed_projections` is a UPSERT keyed by (scope, scope_id, metric); the math is
--     additive so a re-application by the drain after a crash converges to the same
--     value (we always re-derive the delta from the source payload, never from a counter).

CREATE TABLE IF NOT EXISTS outbox_events (
    seq            BIGSERIAL    PRIMARY KEY,
    event_id       TEXT         NOT NULL UNIQUE,
    correlation_id TEXT         NOT NULL,
    causation_id   TEXT         NULL,
    ts             TIMESTAMPTZ  NOT NULL,
    kind           TEXT         NOT NULL,
    payload        JSONB        NOT NULL,
    drained_at     TIMESTAMPTZ  NULL,
    inserted_at    TIMESTAMPTZ  NOT NULL DEFAULT now()
);

-- Partial index keeps the drain hot-path cheap: only pending rows are scanned.
CREATE INDEX IF NOT EXISTS outbox_events_pending
    ON outbox_events (seq) WHERE drained_at IS NULL;

CREATE INDEX IF NOT EXISTS outbox_events_kind_ts
    ON outbox_events (kind, ts);

-- Read-side projection over the audit log (gt-feed style, but in SQL). Panels read this
-- table directly — Grafana's Postgres datasource gives `SUM`/`MAX`/`COUNT` natively.
-- `scope` is the projection family ('account' tokens, 'session' tokens, 'kind' totals,
-- 'correlation' count); `metric` discriminates what is being aggregated inside that
-- family. Kept narrow on purpose: one row per (scope, scope_id, metric) so an UPSERT is
-- O(1) and a panel `WHERE scope = 'account' AND metric = 'tokens_total'` hits the index.
CREATE TABLE IF NOT EXISTS feed_projections (
    scope       TEXT        NOT NULL,
    scope_id    TEXT        NOT NULL,
    metric      TEXT        NOT NULL,
    value_num   BIGINT      NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, scope_id, metric)
);

CREATE INDEX IF NOT EXISTS feed_projections_scope
    ON feed_projections (scope, updated_at DESC);
