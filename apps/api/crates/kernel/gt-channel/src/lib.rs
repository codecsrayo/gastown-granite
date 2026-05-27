//! `gt-channel` — channel events por archivo (`*.event`): mailbox **entre procesos**
//! (await MERGE_READY, etc.; ver `docs/05-queues.md`). Entrega at-least-once por canal.
//!
//! Skeleton (Paso 0): se implementa con `gt-merge` (Paso 6+), usando `notify` para despertar
//! sin polling.

// TODO(paso 6+): emit(channel, event) + await_event(channel) sobre archivos, watcher notify.
