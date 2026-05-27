# 05 — Colas

## Taxonomía: cuatro capas con garantías distintas

No hay "una cola". Hay cuatro, y conflictarlas es el error clásico:

| Cola | Sustrato | Durable | Entrega | Orden | Para qué |
|---|---|---|---|---|---|
| **Mailbox de actor** | `tokio::mpsc` | No (RAM) | at-most-once | FIFO por actor | coordinación *dentro* del proceso |
| **Cola de trabajo** (dispatch) | Dolt | **Sí** | at-least-once | prioridad (P0/P1/P2) | beads esperando polecat |
| **Log de eventos** | `.events.jsonl` | **Sí** | replay por offset | append-only | auditoría + replay determinista |
| **Channel events** | archivos `*.event` | Sí-ish | at-least-once | por canal | mailbox *entre* procesos (await MERGE_READY) |

Consecuencia: **no se necesita un broker tipo Kafka/RabbitMQ.** El tráfico es (a) canales
in-process y (b) una cola de trabajo durable. Meter Kafka aquí sería sobre-ingeniería para
un orquestador local-first.

## Cola de trabajo sobre Dolt

El bead **es** el job. No hay tabla de cola aparte: la "cola" es una vista sobre los beads
en cierto estado.

```
status:   pending → dispatched → working → done | failed
severity: P0 | P1 | P2          (de los escalamientos)
assignee: worker_id?
```

Encolar = crear/marcar bead `pending`. Desencolar = tomar el de mayor prioridad.

### Claim atómico por CAS (no por locks)

Dolt **no implementa `SELECT … FOR UPDATE`**. Su modelo es optimista (ver más abajo).
Pero el CAS funciona en cualquier motor:

```sql
-- gana quien encuentra el bead aún 'pending'
UPDATE beads
   SET status='dispatched', worker=?, dispatched_at=NOW()
 WHERE id=? AND status='pending';
-- affected_rows == 1 → es nuestro; == 0 → otro lo reclamó
```

Esto es **portable** y, además, alineado con el grano nativo de Dolt (que valida
transacciones con CAS internamente).

### Dispatcher serializado (un solo actor)

Como `gt-scheduling::actor` **posee** el dequeue, se serializa el claim dentro del proceso.
El CAS es la red para crash/recuperación o un segundo dispatcher, no el camino feliz.

### Lease / visibility-timeout = heartbeat + witness

Un bead `dispatched` cuyo polecat muere (heartbeat stale) **vuelve a `pending`**. Eso ya lo
hace `gt-patrol` (detecta stale → emite evento → handler en `gt-scheduling` re-encola). Es
el visibility-timeout de SQS, implementado con la maquinaria de heartbeat existente. Vive
en `gt-scheduling/src/expectations.rs`.

### Despertar sin polling

Dolt no tiene `LISTEN/NOTIFY`. Pero el dispatcher es un actor: lo despiertan los mensajes
`Enqueue` del bus. No hay busy-poll. Un tick periódico (en el productor async, no en el
núcleo) solo revisa leases expirados y, si vence alguno, **emite** un evento de timeout al
bus — no muta estado leyendo el reloj. Así el re-encolado queda en el log y es replay-able
(regla de determinismo, [06-observability.md](06-observability.md)).

### Backpressure = capacity governor

El dispatcher solo desencola si hay capacidad (`max_polecats`). Loop:

```
on Enqueue | on capacity-freed | on tick
  → dequeue hasta capacidad disponible
  → CAS-claim
  → spawn
```

## Dolt vs InnoDB (resumen práctico)

La diferencia de fondo: **InnoDB es pesimista (locks de fila), Dolt es optimista
(merge + CAS al commit).**

| | InnoDB | Dolt |
|---|---|---|
| Concurrencia | Locks de fila + MVCC | Sin locks; CAS al commit |
| `SELECT FOR UPDATE` / `SKIP LOCKED` | Sí | **No existe** |
| Choque misma celda | Serializa / last-write-wins | La 2ª transacción **falla** (conflicto) |
| Aislamiento | hasta SERIALIZABLE | solo REPEATABLE READ |
| Almacenamiento | B+tree | Prolly tree (Merkle) → diffs baratos |
| Versionado | ninguno | branch/merge/commit/diff |

Fuente: documentación de DoltHub (modelo de concurrencia y transacciones).

### Restricciones que esto impone

1. **Dispatcher único serializado (no opcional).** Como Dolt castiga las escrituras
   concurrentes a la misma fila con commits fallidos, el actor único no es solo
   ergonómico: es la estrategia correcta. Múltiples dispatchers paralelos requerirían
   `SKIP LOCKED`, que Dolt no ofrece.
2. **Retry-on-conflict si hay más de un escritor.** El segundo commit sobre la misma
   celda falla; envolver el claim en loop de retry si esa concurrencia es posible.
3. **Nunca read-modify-write bajo concurrencia.** Capacity governor, contadores: prohibido
   "leo, sumo, escribo" en Dolt — es el `lost update` clásico. Updates atómicos de una
   sola sentencia, **o** estado en el actor (RAM) y solo se persiste el efecto.

## Política de backpressure por canal

| Canal | Productor | Política | Razón |
|---|---|---|---|
| Audit drain (→ Mongo) | handler **sync** del bus | `mpsc` bounded grande + `try_send`; overflow ⇒ spill a `.events.jsonl` local + evento `AuditOverflow` | el handler es sync, no puede `.await`; no se pierde en silencio, pero la contrapresión **no** bloquea al bus |
| SSE broadcast (→ navegador) | handler sync del bus | `try_send` / `broadcast` lossy | si un cliente se queda atrás, que pierda frames, no que tumbe el sistema |
| Actor mailbox | task async (supervisor, dispatcher) | bounded + `send().await` | el emisor **sí** es async aquí; bloquearlo cuando el actor satura es correcto |

La asimetría es deliberada: solo bloquea con `send().await` quien ya está en contexto async
(productores → mailbox de actor). El fan-out del bus es **sync**, así que sus handlers solo
pueden `try_send`; por eso el sink durable (audit) necesita buffer holgado **y** un fallback
de overflow explícito, no `.await`.

**Prohibido `unbounded`** salvo prueba de que el consumidor siempre gana. Canal sin
límite = bomba de memoria diferida. (El append a `.events.jsonl` local es justamente ese
caso probado y sirve de red de spill para el drain a Mongo.)

## Veredicto para este caso

Dolt es buena elección para la cola **porque la carga es baja-concurrencia y la prioridad
es auditoría/versionado**, no throughput. El `dolt log` sobre la tabla `beads` es la cola
entera reconstruible en el tiempo: "por qué este job fue a ese worker a esa hora" tiene
respuesta para siempre, sin tablas de auditoría manuales.

Si en el futuro se necesitaran dispatchers paralelos con `SKIP LOCKED`, habría que mover la
cola a Postgres (apalis encaja, ya conocido del Plane). Pero entonces se pierde el
versionado, que es por lo que se eligió Dolt.
