# Registro de issues — snapshot 2026-05-27

Documentación de los issues (beads) registrados en el town `hq`, leídos del contenedor
`gastown-sandbox`. Es una **foto puntual**; el estado vive en Dolt y cambia.

## ⚠️ Aviso de fuente (split-brain activo)

Las cuentas **no cuadran entre fuentes** — es un problema conocido, ya registrado como issue
(ver [hq-4dte](#dolt--persistencia), [hq-o16o], [hq-hamg]):

| Fuente | Cuenta | Notas |
|---|---|---|
| `bd list` (embedded) | **55** abiertos | muestra 2× P0 de auth que **no** están en el jsonl |
| `bd list --all` | ~910 líneas | incluye cerrados + formato árbol |
| `/gt/.beads/issues.jsonl` | **716** records | export; los P0 de auth no aparecen aquí todavía |

**Conclusión:** `bd` embedded y el `jsonl` divergen. Para los P0 críticos manda lo que ve
`bd`; para el histórico completo, el `jsonl`. No confiar en un solo conteo.

## Totales (de `issues.jsonl`, 716 records)

| Estado | N | | Prioridad | N | | Tipo | N |
|---|---|---|---|---|---|---|---|
| closed | 555 | | P2 | 631 | | task | 363 |
| open | 139 | | P1 | 62 | | molecule | 303 |
| hooked | 17 | | P3 | 12 | | event | 23 |
| deferred | 3 | | P0 | 8 (cerrados) | | bug | 10 |
| | | | P4 | 1 | | feature | 8 |
| | | | | | | epic | 6 |

De los ~159 activos (open/hooked/deferred): **~49 son trabajo real**, **~110 son ruido
operacional** (wisp lifecycle, convoy-complete, handoffs de patrol, identidades de
agente/rig, tests, compaction reports).

---

## 🔴 P0 — Crítico (visto por `bd` embedded, aún no en jsonl)

| ID | Título |
|---|---|
| **hq-v3cq** | SYSTEMIC AUTH FAILURE: furiosa y nux con `401 Invalid authentication credentials` al reiniciar sesión. Todos los polecats sin poder arrancar Claude Code. Mayor y Deacon caídos. Solo refinery y witness vivos. **Requiere intervención humana**: revisar estado de cuentas Claude, re-autenticar, reiniciar Mayor/Deacon. Bloquea gg-rn5, gg-44k. |
| **hq-va62** | Mayor y Deacon caídos: sesión de Mayor parada, Deacon no encontrado. Cola de nudge del Mayor llena (50/50) antes de morir. furiosa (gg-rn5) con 401. Refinery vivo pero cola estancada. |

> Los 8 P0 que sí están en el jsonl están **todos cerrados** (auth failures previos, export
> wiped, escalations de turquoise, migración Django epic). Los dos de arriba son los vigentes.

---

## 🟠 P1 — Alta prioridad (trabajo real)

### Auth / Quota (raíz de los P0)
| ID | Título |
|---|---|
| hq-ai74 | [HIGH] Quota rotation bloqueada: 5 sesiones rate-limited, **0 cuentas disponibles** (brayan, codecsrayo, fsrb todas limitadas en live scan). Limited: gg-refinery, gg-rictus, gg-witness, hq-boot, hq-mayor. |
| hq-tjf0 | [HIGH] furiosa 401 tras restart: sesión arranca pero Claude Code da 401. Necesita check/rotación de cuenta. |

### Dolt / Persistencia (split-brain — causa raíz de mucho ruido)
| ID | Título |
|---|---|
| hq-4dte | Dual backend mismatch: `bd info` reporta `Mode: direct` pero está **hardcoded** en `cmd/bd/info.go:57`, no refleja el modo real. **3 instancias Dolt coexistiendo.** |
| hq-o16o | [HIGH] gastown_granite auto-importa del JSONL en cada llamada `bd`; writes no persisten entre invocaciones. `bd close` reporta éxito pero el bead sigue open en la siguiente query. |
| hq-hamg | `export.auto:false` en config.yaml → drift silencioso DB↔jsonl. Decidido Option B (server = fuente de verdad), **bloqueado en hq-4dte**. |

### Overseer notify loop (3 beads, mismo bug)
| ID | Título |
|---|---|
| hq-uh9n | [bug] Overseer notify loop: mails duplicados de convoy-complete inundando el inbox del mayor. |
| hq-tpgo | [HIGH] Duplicados (hq-cv-h2bnq, hq-cv-6v7je) cada 1-2 min pese a acks/mark-read. |
| hq-95z (P2) | Overseer re-emite convoy-complete repetidamente (bug de dedupe). |

### Migración Django → Rust (Plane)
| ID | Título |
|---|---|
| hq-akd | terminar de migrar django a rust |
| hq-34h | Migrar Project Templates (4 endpoints) `apps/api/plane/app/urls/template.py` sin equivalente Rust |
| hq-be0 (chore) | Eliminar carpeta `apps/api` (Django) del repo plane |
| hq-79k / pl-3sh | Cerrar TODOs de paridad en api_rust (export, pages, intake, filters) — **duplicados** |

### Otros P1
| ID | Título |
|---|---|
| hq-61v | Review de sesión: split-brain reverts, escalation noise, account scarcity, plane cleanup |

---

## 🟡 P2 — Notables (trabajo real, sin contar ruido)

| ID | Tipo | Título |
|---|---|---|
| hq-68kn | task | Convoy lifecycle desacoplado del estado del bead trackeado (timeline reconstruido: doble sling pl-edd → amber + flint) |
| hq-d412 | task | `gt doctor agent-beads-exist`: cuenta beads cerrados como missing; `--fix` seguro pero inefectivo |
| **hq-0fvy / hq-fime** | feature | Persistir historial de consumo de tokens por cuenta; derivar techos de cuota empíricos de eventos rate-limit. **Duplicados entre sí** y cubiertos por el spec [features/token-tracking-prediction.md](../../api/docs/features/token-tracking-prediction.md) |
| pl-mr9 | task (deferred) | Auto-rebuild del grafo graphify en cada commit (AST + Haiku) |
| pl-j18 | bug (deferred) | ASK_ANYTHING flow incompleto en EditorAIMenu |
| pl-03i | feature (deferred) | Nested Pages parent-child tree |

---

## 🟢 P3 / baja

| ID | Título |
|---|---|
| hq-ikx | Implementar Slack project sync endpoints (3 endpoints, low pri) |
| hq-jaeh | poda las ramas |

---

## Ruido operacional (no enumerado — ~110 beads activos)

No son bugs de ingeniería; son artefactos del orquestador. Conviene **podar/reaper**:

| Categoría | Aprox. | Ejemplos |
|---|---|---|
| `hq-wisp-*` lifecycle | ~50 | "Convoy complete: Work: Fix dual-backend split-brain…", "resolve hq-…", "gt done failing…" |
| Identidades de agente/rig | ~30 | `gg-gastown_granite-polecat-*`, `pl-plane-polecat-*`, `hq-mayor`, `hq-deacon`, `pl-rig-plane` |
| HANDOFF de patrol (hooked) | 7 | hq-3a0a, hq-5h1o, hq-893y, hq-gsc4, hq-qiy, hq-zdr, hq-uop8 |
| Compaction Reports (event) | ~20 | hq-1dk, hq-21g, … (2026-05-23..26) |
| ZOMBIE_DETECTED / POLECAT_DIED | ~5 | hq-3t1e, hq-6j9z, hq-9k7p, hq-pgfr |
| Tests | ~6 | race-test-1..4, http crud test, "test create after restore" |
| Molecules / templates de patrol | 303 (mayoría closed) | pl-2be Deacon Patrol, pl-9of Witness Patrol, pl-c9j Refinery Patrol |

---

## Temas transversales (lo que de verdad importa)

1. **Crisis de auth/cuotas** (P0 + hq-ai74/tjf0): todas las cuentas rate-limited a la vez →
   polecats no arrancan, Mayor/Deacon caen. Cuello de botella operacional #1.
   → Conecta con el spec de **predicción de bloqueo** ([token-tracking-prediction.md](../../api/docs/features/token-tracking-prediction.md))
   y los beads hq-0fvy/hq-fime: rotar **antes** del bloqueo evitaría esta clase de parada.
2. **Split-brain de Dolt** (hq-4dte/o16o/hamg): 3 instancias Dolt, `export.auto:false`,
   auto-import desde jsonl → writes que no persisten. Genera gran parte del ruido wisp.
   Causa raíz a resolver antes de confiar en conteos.
3. **Overseer notify loop** (hq-uh9n/tpgo/95z): bug de dedupe inunda el inbox del mayor →
   llena la cola de nudge (50/50) → contribuye a la caída del Mayor (hq-va62).
4. **Migración Django→Rust** (hq-akd/34h/be0/79k): en curso; alineada con el plan
   `apps/api/docs`.

## Cómo se generó

```sh
docker exec gastown-sandbox bash -lc 'bd list; bd list --all'        # bd embedded
docker exec gastown-sandbox bash -lc 'jq … /gt/.beads/issues.jsonl'  # export
```

Para refrescar este registro: re-correr y re-clasificar. **No** tratar los conteos como
exactos mientras el split-brain (hq-4dte) siga abierto.
