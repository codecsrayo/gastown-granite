//! `gt-plugin` — **único** sitio del núcleo donde se permite `dyn` + `#[async_trait]`
//! (watchdogs, sheriffs: conjunto heterogéneo resuelto en runtime; ver `docs/01-architecture.md`).
//!
//! Skeleton (Paso 0): el `trait Plugin` real se define al cablear los plugins. En el resto
//! del núcleo se usa dispatch estático (genéricos), no `dyn`.

// TODO(paso 6+): trait Plugin { async fn on_event(...) } con #[async_trait], registro dinámico.
