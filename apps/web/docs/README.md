# `apps/web/docs/` · índice para agentes

Carpeta de planificación del **frontend del town** (dashboard SvelteKit).
Esta carpeta es la **fuente de verdad** del alcance, contrato y arquitectura
del frontend nuevo.

## Si entras a esta carpeta por primera vez

Lee en este orden, 5 min cada uno:

1. **[frontend-migration-sveltekit.md](frontend-migration-sveltekit.md)** — alcance + epics + grafo + tabla de estado (reclamar beads aquí).
2. **[frontend-api-surface.md](frontend-api-surface.md)** — qué expone hoy `gt-api`/`gt-mcp`, qué falta (gap table con bead anchor).
3. **[frontend-architecture.md](frontend-architecture.md)** — estructura SvelteKit, stores, SSE, RBAC, patrones núcleo.
4. **[frontend-features.md](frontend-features.md)** — catálogo navegable de features (qué hace cada vista, qué endpoints, qué scope).

Visual:

- **[Gas Town Redesign Wireframes.html](Gas%20Town%20Redesign%20Wireframes.html)** — wireframe V1 dark con 3 frames (Activity · Work · Crew). Abre en navegador local.
- **[pagina.png](pagina.png)** — render hi-fi (= V1 dark canónico, vista Activity).

Referencias paralelas:

- **[issues-registro-2026-05-27.md](issues-registro-2026-05-27.md)** — snapshot de issues del town hq (foto puntual, no spec).
- **[apps/api/docs/07-frontend.md](../../api/docs/07-frontend.md)** — diseño backend del `gt-web` (contrato visto desde el lado servidor).

## Reglas duras (no negociables sin acuerdo)

1. **`internal/web/` (Go) está RETIRADO del despliegue.** No es spec ni
   referencia de endpoints. Sigue en el árbol como referencia de
   comportamiento histórico, no de diseño. Cutover en commit `c877758e`.

2. **Construye contra la API Rust real**, no contra la Go vieja. Si una
   feature necesita algo que no aparece en
   [frontend-api-surface.md](frontend-api-surface.md), eso es un **gap
   explícito** que requiere bead — no improvises endpoints inventados.

3. **Reclama antes de trabajar.** Tabla de estado en
   [frontend-migration-sveltekit.md](frontend-migration-sveltekit.md). Lock
   cooperativo; si no te listas ahí, otro agente duplica.

4. **Decisiones tomadas en frío** (resumen):
   - Stack: SvelteKit + Svelte 5 (runes) + Tailwind + adapter-static.
   - Tema canónico: **dark**. Toggle light disponible.
   - Variante canónica: **V1** (tabs horizontales).
   - Auth: bearer JWT en `Authorization` header. Sin CSRF.
   - SSE: una conexión global multiplexada por `event.type`.
   - Estado: stores runes en `.svelte.ts` + optimistic + SSE reconcile.
   - Routing: sub-paths bookmarkables (`/activity`, `/work`, …).
   - Command bus: único en `gt-root`, compartido HTTP/MCP/CLI.

5. **Frontend gating sin backend enforce es teatro.** Toda acción
   destructiva: backend valida primero, frontend oculta segundo.

6. **Multi-agente:** rama aparte → merge a main → borra rama. Nunca trabajes
   sobre main del town root (auto-revert).

## Estructura del frontend (resumen)

```
apps/web/                    ← código SvelteKit (post hq-fe-build.1)
apps/web/docs/                   ← esta carpeta (planificación + spec)
apps/api/                         ← backend Rust (gt-web, gt-mcp, gt orchestrator)
internal/web/                     ← Go viejo, retirado, NO TOCAR
```

## Cómo abro un bead nuevo (si encuentro un gap)

**Canal único = `gt-mcp`** (servidor MCP registrado en `~/.claude.json`).
Dentro de Claude Code tus tools ya incluyen `mcp__gt-mcp__*` — llámalos
directamente. Snapshots con `ReadMcpResourceTool(server="gt-mcp", uri="gt://…")`.

Agentes **no** necesitan saber URL, container name, ni CLI binarios. Esos son
detalles de backend. Si la operación que necesitas no está en
`mcp__gt-mcp__*`, **el gap se vuelve un bead** — no se bypassea.

1. Verifica que el gap no esté listado en
   [frontend-api-surface.md](frontend-api-surface.md) o
   [frontend-migration-sveltekit.md](frontend-migration-sveltekit.md).
2. Crea bead vía el tool nativo:
   ```
   mcp__gt-mcp__scheduling_create_bead_validate({
     "id":"hq-fe-…","title":"…","priority":2
   })
   mcp__gt-mcp__scheduling_create_bead_execute({
     "id":"hq-fe-…","title":"…","priority":2
   })
   ```
   **Limitación conocida (2026-05-29):** `scheduling.create_bead` escribe en
   la tabla `beads` (5 columnas: id, title, status, priority, assignee),
   no en `issues` (~25 columnas, leída por el dashboard kanban + el plan
   de épics). Para issues completas con `description` / `external_ref` /
   `acceptance_criteria` / dependencias, la familia `issues.*` MCP está
   pendiente — ver epic `hq-mcp-issues` (`.1..5`). Mientras tanto, escala
   al operador humano para la inserción canónica.
3. Actualiza la tabla de estado en
   [frontend-migration-sveltekit.md](frontend-migration-sveltekit.md).
4. Si abres dependencias entre beads, añádelas al grafo del mismo doc.
5. **Nunca** desde un agente: `docker exec`, acceso directo a Dolt, ni CLI
   binarios externos. Esos son escape hatch operador-only (sin audit, sin
   scope, sin invariantes del reactor). Ver
   [`feedback_mcp_canonical_for_agents`](../../../../.claude/projects/-home-nixos-gastown/memory/feedback_mcp_canonical_for_agents.md).

## Decisiones de scope (NO en MVP)

| Pedido | Decisión | Razón |
|---|---|---|
| Tab Mail / inbox | descartado | Dominio no existe en Rust |
| Tab Git events / log | diferido | No bloquea operación |
| Tab Hooks · Dogs · Polecats separadas | diferido | Overlap con Activity/Sessions filtrado |
| Tab Escalations dedicada | diferido | Activity feed con filtro cubre |
| Command palette `⌘` | diferido | Útil post-cutover |
| Pop-out terminal window | diferido | Bloqueador hq-fe-term primero |
| Spark-timeline visualización | diferido | Activity tabular basta |
| Multi-actor session local | diferido | 1 token = 1 actor por device |

Detalle completo: [frontend-features.md § Decisiones de scope](frontend-features.md#decisiones-de-scope-no-hacer-en-mvp).

## FAQ para agentes

**¿Por qué dark theme?** El mockup hi-fi ([pagina.png](pagina.png)) está en
dark — es el canon. Light se mantiene como toggle por preferencia.

**¿Por qué V1?** Es el de la imagen. V2/V3/V4 fueron exploración inicial,
descartadas del mockup (las variantes ya no están en el HTML wireframe).

**¿Por qué no portar `internal/web/` 1:1?** La API Rust no replica los
endpoints del Go viejo (`/api/run`, `/api/mail/*`, `/api/issues/*`, etc.).
Reescribir 4953 líneas vanilla JS contra una API que no existe = absurdo.
Build contra el contrato real.

**¿Por qué bearer JWT en localStorage si XSS?** No hay alternativa con
bearer puro. HttpOnly cookie requeriría CSRF. Mitigación: scope granular +
audit + rate-limit (backend), `<Guard>` (frontend UX). El bearer es la
identidad humana, no de máquina.

**¿Cómo conecto SSE durante dev local?** Vite proxy `/api → http://localhost:8787`.
Backend en compose (`gt-api` service). Si SSE falla: verifica `GT_WEB_TOKEN`
en `.env` y que el container esté up.

**¿Dónde está el spike de terminal?** `hq-fe-term.0` — bead obligatorio
antes de tocar terminal/dock. Decisión: WebSocket en gt-api vs MCP tool vs
bin separado.

**¿Skills/Roles existen en backend?** No todavía. Dominio NUEVO
(`gt-skills` crate). Plan en `hq-fe-skills.*`. No hay endpoint hasta que
hq-fe-skills.1+.2 cierren.

**¿Account login automático?** Bloqueado por Anthropic (no expone
redirect_uri callback genérico). Plan B: pty driver (`hq-fe-auth.*`) — 2
clicks en lugar de 3.

## Cross-refs externos

- Backend design: [apps/api/docs/](../../api/docs/)
- Compose / deploy: [docker-compose.yml](../../../docker-compose.yml) · [apps/api/docs/deployment/](../../api/docs/deployment/)
- Observabilidad: [deploy/observability/](../../../deploy/observability/) (Prometheus + Tempo + Grafana)
- Memoria del proyecto: [`~/.claude/projects/-home-nixos-gastown/memory/MEMORY.md`](../../../../.claude/projects/-home-nixos-gastown/memory/MEMORY.md)
