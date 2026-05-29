-- hq-mysw (Paso 9.F): read-side activity projection (SQL twin of `gt-feed::activity_view`).
--
-- One row per correlation lifeline tracking its most recent activity. Panels (and Grafana's
-- Postgres datasource) read this directly and color-code with a single SQL `CASE` on the age,
-- or via the Rust `PgActivity` helper which reuses `gt_feed::activity` so the thresholds match
-- the in-memory view exactly.
--
-- Idempotency: the drain re-derives `last_activity_secs` from the event's own `ts`, never a
-- counter, and the UPSERT keeps the MAX (`GREATEST`). So a redelivered or out-of-order event
-- converges to the same row — an older event can never roll the activity backwards.
CREATE TABLE IF NOT EXISTS activity_projections (
    subject            TEXT        NOT NULL PRIMARY KEY,
    last_activity_secs BIGINT      NOT NULL,
    last_kind          TEXT        NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Panels list "most recently active first"; the index keeps that ordering cheap.
CREATE INDEX IF NOT EXISTS activity_projections_recent
    ON activity_projections (last_activity_secs DESC);
