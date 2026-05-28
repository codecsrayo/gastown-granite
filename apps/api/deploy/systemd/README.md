# Gas Town systemd units (paso 8.5, hq-8iur.5)

Two managed services, one for each Rust binary in `bins/`:

| Unit                   | Binary  | Purpose                                                    |
|------------------------|---------|------------------------------------------------------------|
| `gastown-api.service`  | `gt-web`| HTTP/SSE read-side API (doc 07). Liveness + readiness probes. |
| `gastown-gt.service`   | `gt`    | Composition root: actor fan-out + audit log + PG outbox.   |

Both files are templates — adjust `User=`, `Group=`, `ExecStart=` and `EnvironmentFile=`
to match your fleet. They are deliberately conservative on isolation (`ProtectSystem=strict`,
`NoNewPrivileges=true`) so a panicking binary cannot scribble outside `/var/lib/gastown`.

## Install

```bash
sudo install -m 0644 gastown-api.service gastown-gt.service /etc/systemd/system/
sudo useradd --system --home /var/lib/gastown --shell /usr/sbin/nologin gastown
sudo mkdir -p /etc/gastown
sudo install -m 0640 -o gastown -g gastown env.example /etc/gastown/gastown-api.env
sudo install -m 0640 -o gastown -g gastown env.example /etc/gastown/gastown-gt.env
sudo systemctl daemon-reload
sudo systemctl enable --now gastown-api.service gastown-gt.service
```

## Probe checklist (kube/LB compatible)

`/health` and `/readyz` sit **outside** the bearer-token middleware. Probes carry no
`Authorization` header by design — that is the standard contract for k8s liveness /
readiness and for LB health checks.

```bash
curl -fsS http://127.0.0.1:8787/health    # 200 once the process answers
curl -fsS http://127.0.0.1:8787/readyz    # 200 once hydration + PG + Dolt are ready
```

`/readyz` returns `503` with a JSON body that names the failing probe:

```json
{
  "ready": false,
  "hydration_done": true,
  "checks": [
    {"name": "pg-audit", "status": "pass"},
    {"name": "dolt", "status": "fail", "reason": "connect refused (127.0.0.1:3307)"}
  ]
}
```

## Graceful shutdown — what `kill -TERM` actually does

The binaries install SIGTERM + SIGINT handlers and drain in dependency order:

**`gt-web`** (`bins/gt-web/src/main.rs`):

1. SIGTERM → axum `with_graceful_shutdown` stops accepting new connections, finishes
   in-flight HTTP/SSE.
2. `root.shutdown()` drops the broadcast `Sender` — the PG audit task receives
   `Closed`, finishes the last `append`, exits.
3. `audit_task.await` blocks until that final `append` lands.
4. Telemetry guard drops → OTLP batch exporter flushes its pending spans.

**`gt`** (`bins/gt/src/main.rs`):

1. SIGTERM → `root.shutdown()` drops the broadcast `Sender`.
2. The outbox **writer** task drains the broadcast backlog into `outbox_events`,
   then exits on `Closed`. We `await` it.
3. The outbox **drain** task is signalled (via an `AtomicBool`) to switch from its
   periodic 200ms tick to a final pass-until-empty: it drains every pending row into
   `audit_events` + `feed_projections`, then exits. We `await` it.
4. Telemetry guard drops → OTLP batch flush.

Gate (doc-04 §"Outbox por cada store que escribe" + paso 8.5):
`kill -TERM` → 0 rows lost from `outbox_events`.

## `TimeoutStopSec` tuning

- `gastown-api`: 30s. Tail of in-flight HTTP requests + last PG audit append.
- `gastown-gt`:  45s. Same plus the final outbox drain. Raise if the outbox lag
  graph in Grafana (`outbox_events.drained_at IS NULL` count) trends high — the
  drain pulls 256 rows per pass during shutdown, so the budget grows with backlog.

## Restart policy

`Restart=on-failure` is deliberate. A clean SIGTERM (exit 0) is the operator's
intent — never auto-restart. A panic or non-zero exit triggers a 5s backoff;
`StartLimitBurst=5` over a 60s window prevents a panic-loop from masking the
real cause in the journal.
