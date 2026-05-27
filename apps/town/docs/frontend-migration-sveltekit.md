# Migración del frontend a SvelteKit

Plan por fases para reemplazar el frontend actual del dashboard (`internal/web/`) por
una SPA en **SvelteKit**. Cada fase entrega algo verificable y tiene un **gate**; no se
cruza al siguiente sin el anterior en verde.

---

> ## ⚠️ AVISO PARA AGENTES — leer antes de tocar el frontend
>
> **Hay varios agentes trabajando en este repo.** Este documento es la **fuente de verdad**
> de la migración del frontend. Sigue estas reglas para no perder ni pisar trabajo:
>
> 1. **Esto es un plan, no permiso para borrar nada.** El frontend viejo
>    (`internal/web/templates/convoy.html`, `static/dashboard.js`, `static/dashboard.css`)
>    **debe seguir vivo y funcionando** hasta la **Fase 5**, y solo se borra con paridad
>    probada. No lo toques "de paso" en otras fases.
> 2. **Reclama antes de trabajar.** Antes de empezar una fase o región, márcala ocupada
>    (bead en estado busy) y anótala en la **tabla de estado** de abajo con tu identidad.
>    Al terminar, cierra el bead y actualiza la tabla. Así nadie duplica.
> 3. **Respeta el orden de fases.** No te saltes la Fase 0 (endpoint JSON snapshot): todo
>    lo demás depende de ella. Las fases tienen dependencia, no son paralelas entre sí
>    (las *regiones* de la Fase 4 sí son paralelizables entre agentes).
> 4. **Rama aparte → merge a main → borra la rama.** Nunca trabajes directo sobre main
>    (el town root revierte). Usa worktree.
> 5. **Decisiones ya tomadas (no re-litigar sin acuerdo):** framework = **SvelteKit**;
>    Astro descartado para el dash; el backend Go pasa a **API+SSE puro**; el contrato
>    `/api/snapshot` + SSE debe quedar compatible con el `gt-web` Rust de
>    [07-frontend.md](../../api/docs/07-frontend.md).
>
> ### Tabla de estado (actualizar al reclamar/terminar)
>
> | Fase | Estado | Agente | Bead | Notas |
> |---|---|---|---|---|
> | 0 — `/api/snapshot` JSON | PLANEADO | — | — | backend Go puro; bajo riesgo |
> | 1 — scaffold SvelteKit + SSE | PLANEADO | — | — | bloqueada por Fase 0 |
> | 2 — CSRF + escritura | PLANEADO | — | — | |
> | 3 — terminales xterm | PLANEADO | — | — | |
> | 4 — descomponer por región | PLANEADO | — | — | regiones paralelizables |
> | 5 — cutover + borrado | PLANEADO | — | — | **solo con paridad probada** |
>
> Estado global: **PLANEADO, no iniciado** (al 2026-05-27). Mantén esta tabla viva.

---

## Punto de partida (qué hay hoy)

| Pieza | Estado |
|---|---|
| `internal/web/templates/convoy.html` | Template Go (`html/template`), **236 directivas `{{…}}`** — snapshot inicial server-side |
| `internal/web/static/dashboard.js` | **4953 líneas vanilla JS, sin módulos, sin bundler** — capa viva |
| SSE | 3 streams: `/api/events`, `/api/git/events`, `/api/quota/stream` |
| Acciones | decenas de `fetch('/api/…')` (run, crew, mail, issues, options, session/kill…) |
| Terminales | xterm.js (CDN) attachadas a tmux (`terminal-attach.js`, `console.html` pop-out) |
| CSRF | token inyectado en el template, validado en POST |
| Build | **ninguno** — Go sirve archivos crudos |
| Diseño objetivo | `apps/town/docs/Gas Town Redesign Wireframes.html` + `pagina.png` |

Patrón actual = **snapshot HTML (template) + capa viva JS sobre SSE**. La migración lo
convierte en **snapshot JSON + SPA**, que es justo el contrato de `gt-web` en el plan Rust
([apps/api/docs/07-frontend.md](../../api/docs/07-frontend.md)) — el frontend nuevo no
necesitará retrabajo cuando el backend pase de Go a Rust.

## Por qué SvelteKit (y por qué no Astro)

- El dash es una **app interactiva en vivo** (SSE persistente, terminales, estado denso),
  no contenido. Astro (static-first, islands, ship-zero-JS) no aporta aquí; se usaría como
  un Vite glorificado. Astro solo valdría para un sitio de **docs/contenido** aparte.
- Svelte: stores reactivos mapean directo al patrón `store.js` + parcheo-por-evento;
  runtime mínimo; "actualiza esta fila al llegar el evento" es su caso natural.

## Principio rector

**El framework es ergonomía; el contrato es lo que importa.** Si un endpoint JSON snapshot
+ los SSE existentes pueden mover la UI, el resto es construcción incremental. Por eso la
Fase 0 es backend puro y de-risk antes de tocar SvelteKit.

---

## Fase 0 — Endpoint JSON snapshot (backend, sin frontend)

**Objetivo:** desacoplar el snapshot del template. Convertir lo que renderizan las 236
`{{…}}` en datos.

**Entregable:**
- `GET /api/snapshot` (o `/api/convoy`) que devuelve en JSON la misma estructura que hoy
  alimenta `convoy.html`: mayor, rigs, crew, hooks, escalations, health, ages, progress.
- Reutilizar los structs Go que ya construyen el view-model del template (no duplicar).

**Gate:**
- `curl /api/snapshot` devuelve JSON que cubre todo lo que pinta el template.
- El dashboard actual sigue intacto (no se toca `convoy.html` ni `dashboard.js`).

---

## Fase 1 — Scaffold SvelteKit + contrato de datos

**Objetivo:** probar que JSON snapshot + SSE existentes mueven una UI Svelte.

**Entregable:**
- `frontend/` con SvelteKit + Tailwind. Adapter **static** (SPA) → Go sirve el build; la
  API y los SSE quedan en el mismo origen (sin CORS). En dev, proxy a Go.
- Tipos TS que reflejan `/api/snapshot` y la forma del `EventRecord` de los 3 SSE.
- Store SSE: suscribe `/api/events`, `/api/git/events`, `/api/quota/stream` → stores Svelte.
- xterm.js y addons vía **npm** (versiones pineadas), no CDN.

**Gate:**
- El dev server renderiza una página desde `/api/snapshot` **y** parchea en vivo un widget
  (p. ej. lista de sesiones) desde `/api/events`. Esto prueba SSE+JSON → UI.

---

## Fase 2 — CSRF + camino de escritura

**Objetivo:** las acciones (`POST /api/…`) funcionan desde la SPA.

**Entregable:**
- Decidir CSRF para SPA: **cookie double-submit** o `GET /api/csrf` que entrega el token.
  (Hoy el token va incrustado en el template — eso desaparece con la SPA.)
- Cablear una acción de escritura (p. ej. `run` / `nudge`) con el token.

**Gate:**
- Una acción de escritura se ejecuta desde SvelteKit con CSRF validado server-side.

---

## Fase 3 — Terminales xterm

**Objetivo:** portar las terminales tmux (lo más específico del dash).

**Entregable:**
- Componente Svelte que envuelve xterm.js (montaje DOM agnóstico — `terminal-attach.js`
  porta casi tal cual).
- Attach a sesión tmux; ruta de pop-out equivalente a `console.html`.

**Gate:**
- Attach a una sesión tmux viva desde la UI nueva; el pop-out funciona idéntico.

---

## Fase 4 — Descomponer el dashboard, región por región

**Objetivo:** reconstruir las 4953 líneas como componentes, contra el **wireframe**, no 1:1
de la UI vieja.

**Inventario previo (el contrato real):** listar todos los `/api/*` y todos los tipos de
evento SSE que consume `dashboard.js` — es la fuente de verdad de qué debe replicar la SPA.

**Regiones (mapear a componentes):**
- grid de convoy / rigs · crew · mail · issues · quota cards · escalations · paleta de
  comandos (`run`) · health/heartbeat.

**Entregable:**
- Componentes por región, construidos según el wireframe.
- **Correr en paralelo** al template Go (SvelteKit bajo otra ruta/puerto) hasta paridad.

**Gate (por región):** checklist de paridad contra el dashboard viejo antes de marcar la
región como hecha.

---

## Fase 5 — Cutover + borrado

**Objetivo:** la SPA reemplaza al template; Go queda como API+SSE puro.

**Entregable:**
- `/` sirve el build de SvelteKit (Go sirve los assets estáticos del build).
- Borrar `convoy.html`, `dashboard.js`, `dashboard.css` y el render `html/template`.
- `internal/web` queda como handlers `/api/*` + SSE + static-del-build.

**Gate:**
- Template y JS viejos eliminados; toda la funcionalidad vive en la UI nueva; CSRF intacto;
  terminales y los 3 SSE operativos.

---

## Transversal

- **Alineación con Rust.** El contrato `/api/snapshot` + SSE es el mismo que `gt-web`
  ([07-frontend.md](../../api/docs/07-frontend.md)). Hacer esta migración ahora deja el
  frontend listo para el backend Rust sin retrabajo.
- **Riesgo principal:** el monolito de 4953 líneas tiene comportamiento oculto. El inventario
  de `/api/*` + tipos de evento (Fase 4) es obligatorio antes de reconstruir — es el contrato.
- **No portar 1:1.** El wireframe es el objetivo; la UI vieja es referencia de comportamiento,
  no de diseño.

## Resumen visual

```
Fase 0  ── /api/snapshot JSON ───────────────  backend, dash viejo intacto
Fase 1  ── scaffold SvelteKit + SSE store ───  JSON+SSE mueven una UI Svelte
Fase 2  ── CSRF + escritura ─────────────────  una acción POST funciona
Fase 3  ── terminales xterm ─────────────────  attach tmux + pop-out
Fase 4  ── descomponer por región ───────────  paridad vs wireframe, en paralelo
Fase 5  ── cutover + borrado ────────────────  Go = API+SSE puro
```
