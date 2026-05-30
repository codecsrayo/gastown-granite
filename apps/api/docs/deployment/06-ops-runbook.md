# Ops runbook — token bootstrap, RBAC, troubleshooting

Runbook for operating the live `gastown.codecsrayo.com` deploy after the SvelteKit
cutover (epic `hq-fe-cut`). Covers how to mint and rotate auth credentials, how to
add or change actors and their grants, and the failure modes you'll see at the
edge (`gt-api` + traefik) and inside the compose network.

Companion docs:
[`00-overview.md`](00-overview.md), [`01-services.md`](01-services.md),
[`04-mcp.md`](04-mcp.md), [`../07-frontend.md`](../07-frontend.md) (auth design).

---

## 1. Auth modes — what `gt-web` accepts at boot

`gt-web` selects exactly one auth mode at startup, in this priority order
(`bins/gt-web/src/main.rs`):

| Priority | Env var | Mode | Use |
|---|---|---|---|
| 1 | `GT_WEB_JWT_SECRET` | `Jwt` (HS256 + RBAC) | **production / multi-actor** |
| 2 | `GT_WEB_TOKEN` | `Bearer` (single shared secret) | legacy, single client |
| 3 | `GT_WEB_AUTH=disabled` | `Open` | **dev only** — loud warning on boot |

Anything else aborts with exit 2 — there is no implicit open mode. The `.env`
shipped in compose sets `GT_WEB_TOKEN`; switch to `GT_WEB_JWT_SECRET` when more
than one actor needs differentiated scopes (see §3 RBAC).

Static assets (`/`, `/_app/*`, `/login/`, deep SPA routes) and the probes
(`/health`, `/readyz`) are **outside** the auth layer — the login page has to load
without an `Authorization` header. Everything under `/api/*` is gated, plus
`/api/whoami` returns the resolved actor/mode/scopes for the **caller** so the SPA
can decide which controls to render.

## 2. Bootstrapping tokens

### 2.1 First-time bring-up (single bearer)

Smallest setup that works end-to-end:

```bash
# 1. Mint a secret. Treat it like a database password.
openssl rand -hex 32

# 2. Drop it into .env at the town root.
echo "GT_WEB_TOKEN=<paste-the-hex>" >> .env

# 3. Recreate the gt-api service so the binary re-reads .env.
task deploy-gt-api
```

Verify:

```bash
curl -sk -H "Host: gastown.codecsrayo.com" \
     --resolve gastown.codecsrayo.com:443:<traefik-ip> \
     https://gastown.codecsrayo.com/api/whoami
# → 401  (probe without token)

curl -sk -H "Host: gastown.codecsrayo.com" \
     -H "Authorization: Bearer $GT_WEB_TOKEN" \
     --resolve gastown.codecsrayo.com:443:<traefik-ip> \
     https://gastown.codecsrayo.com/api/whoami
# → 200  { "actor": "web:<12hex>", "mode": "bearer", ... }
```

The bearer actor tag is `web:<first 12 hex of SHA-256(token)>` — derived, not
named. That's intentional: bearer mode has no actor table, so audit records get a
stable but anonymous handle.

### 2.2 Per-actor JWTs (production)

Once you need more than one client (host operator, scheduled agent, CI runner,
sheriff) bound to different scopes, switch to JWT mode.

```bash
# 1. Generate a signing secret. HS256 is symmetric — the secret signs AND
#    verifies, so anything that holds it can mint a token. Treat as a master key.
openssl rand -hex 64

# 2. Wire .env (remove or comment GT_WEB_TOKEN to avoid silent fallback).
echo "GT_WEB_JWT_SECRET=<paste-the-hex>" >> .env
echo "GT_WEB_RBAC_CONFIG=/etc/gastown/mcp-scope.toml" >> .env  # default path

# 3. Edit the RBAC config (§3) so actors map to roles/scopes.

# 4. Recreate.
task deploy-gt-api
```

There is **no operator mint CLI today** (gap — file a bead if you need one). To
hand-mint a token for an actor, use the smallest possible Rust shim against
`JwtIssuer::sign_for_actor`:

```rust
// scratch_mint.rs — run with `cargo run --bin scratch_mint`.
use gt_rbac::RbacConfig;
use gt_web::JwtIssuer;
use std::sync::Arc;
fn main() {
    let secret  = std::env::var("GT_WEB_JWT_SECRET").expect("GT_WEB_JWT_SECRET");
    let rbac    = RbacConfig::from_path(std::env::var("GT_WEB_RBAC_CONFIG")
                    .unwrap_or_else(|_| "/etc/gastown/mcp-scope.toml".into()))
                    .expect("rbac load");
    let issuer  = JwtIssuer::from_secret(secret).with_rbac(Arc::new(rbac));
    let actor   = std::env::args().nth(1).expect("usage: scratch_mint <actor>");
    println!("{}", issuer.sign_for_actor(actor).expect("sign"));
}
```

The shim consults the same RBAC config the running `gt-web` reads, so the minted
token's `roles[]` + `scopes[]` track whatever the file currently says.

For interim one-offs, any HS256 minter works (e.g. `jwt-cli`) as long as the
payload matches `Claims { sub, iss, iat, exp, roles, scopes }` and `iss` is
`gt-web`. `gt-web` rejects on any of `signature`/`exp`/`iss` mismatch; the
specific reason lands in `web.unauthorized.reason` in the audit log but the 401
body is opaque.

### 2.3 Rotation

| Mode | What to swap | Impact |
|---|---|---|
| Bearer (`GT_WEB_TOKEN`) | replace the env var, `task deploy-gt-api` | all clients re-auth with the new token; no overlap window |
| JWT (`GT_WEB_JWT_SECRET`) | new secret, redeploy, re-mint every actor token | every outstanding JWT 401s immediately — coordinate with the agent fleet |

Both modes are zero-downtime for the **container** (the recreate is sub-second)
but not for **callers** — there is no dual-key window today. Drain agent sessions
before rotating in JWT mode, or accept a flash of 401s the agents retry through.

## 3. RBAC bootstrap

One file rules both bins:

- **File**: `apps/api/deploy/mcp-scope.toml` (baked into the image at
  `/etc/gastown/mcp-scope.toml`).
- **Read by**: `gt-mcp` via `GT_MCP_SCOPE_CONFIG`, `gt-web` via
  `GT_WEB_RBAC_CONFIG` (fallback: `GT_MCP_SCOPE_CONFIG`).
- **Override**: bind-mount a different file at `/etc/gastown/mcp-scope.toml`
  (compose-level) or repoint the env var.

### 3.1 Schema

```toml
# Actor: who's calling. Identified by GT_MCP_ACTOR (MCP) or JWT `sub` (gt-web).
[actors.<actor-id>]
allow         = ["scheduling.*", "patrol.tick.execute", "issues.*"]  # MCP tools
validate_only = false                                                # MCP-only
roles         = ["sheriff"]                                          # gt-web

# Role: bundle of gt-web scopes. Many actors → role → scopes.
[roles.<role-name>]
scopes = ["beads.write", "merge.read", "merge.submit"]
```

Deny-by-default: an actor absent from `[actors.*]` resolves to a closed MCP scope
and an empty JWT grant. There is no hardcoded admin.

Wildcard `*` in `allow` means "every MCP tool". Use sparingly — the only legit
example is the dev actor `mcp-local`.

### 3.2 Adding a new actor

1. Append `[actors.<id>]` with the minimum `allow` set (MCP) and `roles` (gt-web)
   it needs.
2. If introducing a new role, append `[roles.<name>] scopes=[...]` and reference
   it from one or more actors.
3. Validate locally: `cargo test -p gt-rbac` covers schema; `cargo test -p
   gt-mcp` exercises the resolver.
4. `task deploy-gt-api && task deploy-gt-mcp` so both bins reload the file.
5. Mint the actor's JWT (§2.2) and hand it off.

### 3.3 Auditing who can do what

```bash
# What gt-mcp will let an actor call:
GT_MCP_ACTOR=<actor> gt-mcp-cli list-tools
# What gt-web sees in a token:
curl -sk -H "Authorization: Bearer <jwt>" \
     --resolve gastown.codecsrayo.com:443:<traefik-ip> \
     https://gastown.codecsrayo.com/api/whoami
# → { actor, mode: "jwt", roles: [...], scopes: [...] }
```

`/api/whoami` is the canonical "what did the server resolve" probe — trust it
over reading the config by eye.

## 4. Troubleshooting

### 4.1 Edge / traefik

| Symptom | Likely cause | Fix |
|---|---|---|
| `curl` to `gastown.codecsrayo.com` resolves to 0.0.0.0 / fails | host has no DNS for the public domain | use `--resolve gastown.codecsrayo.com:443:<proxy-ip>` and `-H "Host: ..."` — see §2.1 |
| 404 from traefik (no `server: gt-api` header) | gt-api not on `proyectos-bi-quare` network or labels removed | `docker network inspect proyectos-bi-quare \| grep gt-api`; check `traefik.enable=true` label is still in compose |
| 502 / "bad gateway" | gt-api container down or just restarted | `docker ps \| grep gt-api`; `docker logs --tail 50 gastown-gt-api` |
| HTTP→HTTPS redirect loops | `gastown-https` middleware misordered | confirm `traefik.http.routers.gastown-http.middlewares=gastown-https` and `redirectscheme.scheme=https` |

### 4.2 SPA serving (post hq-fe-cut.1/.2)

| Symptom | Likely cause | Fix |
|---|---|---|
| `GET /` returns 404 | `GT_WEB_DIST` points to a missing dir or build wasn't baked | inside the container: `ls /srv/web` should show `index.html`; if empty, `task deploy-gt-api` to rebuild the image — the web stage runs `pnpm install && pnpm run build` and copies to `/srv/web` |
| `GET /_app/version.json` is HTML instead of JSON | `ServeDir` falling through to `index.html` | the file's missing from the build — rebuild and confirm the `vite build` output includes `_app/version.json` |
| `GET /login` returns 307 | `ServeDir` trailing-slash redirect for directory-style routes | expected — clients should follow redirects (`curl -L`); SvelteKit adapter-static emits dir-per-route |
| Deep SPA route (e.g. `/sessions`) returns 404 | static fallback not configured (`fallback: 'index.html'` missing in `svelte.config.js`) | confirm the adapter config, rebuild |

### 4.3 Auth (`/api/*`)

| Symptom | Likely cause | Fix |
|---|---|---|
| 401 with valid-looking token | mode mismatch — token is JWT, server in bearer mode (or vice versa) | check `eprintln!` boot line in `docker logs gastown-gt-api`; reconcile `GT_WEB_*` env |
| 401 just after rotation | old token in client | re-mint per §2.2/2.3 |
| 401 with no obvious cause | check the audit log: `docker exec gastown-gt-api tail -n 50 /var/lib/gastown/events.jsonl \| grep web.unauthorized` — `reason` distinguishes `expired` / `invalid signature` / `malformed` / `issuer mismatch` |
| 403 on a route that worked before | scope guard rejected — actor lost the scope after an RBAC edit | `GET /api/whoami` to see resolved scopes; re-check `[roles.*]` and `[actors.<id>].roles` |
| Boot exit 2 with "refusing to start" | no `GT_WEB_JWT_SECRET`, no `GT_WEB_TOKEN`, no `GT_WEB_AUTH=disabled` | set one |

### 4.4 Internal state (Dolt / PG / event log)

| Symptom | Likely cause | Fix |
|---|---|---|
| `/readyz` reports `dolt: fail` | `gastown@%` user lost or dolt restarted mid-write | `docker compose restart dolt`; container entrypoint re-runs `CREATE USER IF NOT EXISTS` on every boot |
| `/readyz` reports `pg-audit: fail` | sqlx migrations failed or PG just bounced | `docker logs gastown-pg`; `docker compose restart postgres` then `gt-api` |
| MCP issue counts disagree with `bd` CLI | embedded vs server mode (see [bd embedded vs server mode](../../../.claude/projects/-home-nixos-gastown/memory) or §2.5 of `02-data-stores.md`) | agents must call MCP (`gt-mcp-cli`), not `bd` via docker exec |
| Event log corrupted / unreadable | rare — `events.jsonl` is append-only newline-delimited JSON; a torn tail line will skip on replay but newer events keep landing | `tail -n 1` to inspect; the orchestrator tolerates a partial last line on hydrate |

### 4.5 Quick smoke recipe

After any deploy, run this minimum sweep against the live proxy:

```bash
PROXY_IP=$(docker inspect proxy --format '{{(index .NetworkSettings.Networks "proyectos-bi-quare").IPAddress}}')
TOKEN=$(grep ^GT_WEB_TOKEN .env | cut -d= -f2)   # or your minted JWT

for path in / /_app/version.json /login/ /sessions/ /health /readyz; do
  printf "%-22s " "$path"
  curl -sk -o /dev/null -w "%{http_code}\n" \
       --resolve gastown.codecsrayo.com:443:$PROXY_IP \
       https://gastown.codecsrayo.com$path
done

printf "%-22s " "/api/quota/accounts"
curl -sk -o /dev/null -w "%{http_code}\n" \
     -H "Authorization: Bearer $TOKEN" \
     --resolve gastown.codecsrayo.com:443:$PROXY_IP \
     https://gastown.codecsrayo.com/api/quota/accounts
```

Expected: `/` `/_app/version.json` `/login/` `/sessions/` `/health` `/readyz`
return `200`; `/api/quota/accounts` returns `200` with a valid token or `401`
without.

## 5. Known gaps

- **No operator mint CLI** — §2.2 falls back to a Rust shim. File a bead under
  `hq-fe-rbac` if this hurts.
- **No dual-key rotation window** — §2.3 is a hard cutover.
- **`GT_TOWN_ROOT` not wired in compose for gt-api** — `/api/worktrees` returns
  empty in the container until the host-bind + UID switch are sorted (see the
  comment in `docker-compose.yml`). Workaround: run `gt-web` host-side against
  the host town root.
- **`internal/web/` (Go dashboard) still in the tree** — retirement moved to
  epic `hq-oap5` (Retire Go orchestrator).
