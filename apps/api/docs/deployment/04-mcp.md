# Deployment — acceso MCP (gt-mcp)

`gt-mcp` expone el control de la town como **herramientas MCP** para clientes LLM. Mismo
binario que `gt`/`gt-web` por dentro (boota su propio composition root), pero su frontera es
el protocolo MCP (rmcp) en vez de HTTP-REST.

## Transporte

- `GT_MCP_TRANSPORT=http` → streamable-HTTP en `GT_MCP_HTTP_BIND` (`0.0.0.0:8765`), publicado
  a `127.0.0.1:8765/mcp`.
- Sin esa var → stdio (el modo que usa un Claude Code que spawnea el binario directo).

## Superficie

- **Tools** — pares `validate` / `execute` por dominio:
  `agent.*`, `merge.*`, `orch.launch_convoy.*`, `patrol.*`, `quota.probe.*`, `rig.*`.
  `*.validate` = dry-run (sin cambio de estado); `*.execute` = muta vía el actor.
- **Resources** — snapshots `gt://*`: `agent/sessions`, `scheduling/queue`, `patrol/leases`,
  `merge/slots`, `orch/convoys`, `quota/accounts`, `rigs`.

## Scope (autorización)

No hay admin hardcodeado. Cada conexión usa un **actor** (`GT_MCP_ACTOR`) y su scope sale de
`GT_MCP_SCOPE_CONFIG` (TOML/JSON). Un actor ausente del archivo → deny-all.

```toml
# /etc/gastown/mcp-scope.toml (horneado en la imagen)
[actors.mcp-local]
allow = ["*"]          # full, para dev. Restringir antes de exponer multi-cliente.
```

Formato: `allow` = lista de patrones de tool (`scheduling.*`, `patrol.tick.execute`);
`validate_only = true` bloquea los `execute` de ese actor.

## Cliente local: `gt-mcp-cli`

Binario standalone (`/home/nixos/gt-mcp-cli`, en PATH). Default `--url
http://127.0.0.1:8765/mcp` → pega al **container vivo** (DB `hq`).

```
gt-mcp-cli tools [--full]            # lista tools (+ schemas)
gt-mcp-cli resources                 # lista resources
gt-mcp-cli read gt://agent/sessions  # lee un snapshot
gt-mcp-cli call orch.launch_convoy.execute --json '{...}'
```

## Dos instancias de gt-mcp (no confundir)

| | stdio (Claude Code host) | container (`:8765`) |
|---|---|---|
| def | `.claude.json` server `gt-mcp` | servicio compose `gt-mcp` |
| binario | host `target/debug/gt-mcp` | release en imagen |
| backend | **in-memory** (sin `GT_DOLT_URL`) | **Dolt `hq` + PG** |
| estado | vacío, efímero por spawn | vivo, persistente |
| actor | `dev` | `mcp-local` |

Las tools `mcp__gt-mcp__*` de un Claude Code apuntan al **stdio** (in-memory) salvo que su
config use `type:"http", url:"http://127.0.0.1:8765/mcp"`. `gt-mcp-cli` apunta al **container**
por default. Para tocar la town viva, usa el container.
