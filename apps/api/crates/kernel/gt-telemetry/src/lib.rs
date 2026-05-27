//! `gt-telemetry` — `tracing` + OTEL, `#[instrument]` por workflow. Es la vía de
//! trazabilidad hacia Grafana (OTEL→Tempo + Prometheus), NO una BD de documentos
//! (ver `docs/06-observability.md`).
//!
//! Skeleton (Paso 0): el `correlation_id` del envelope se propaga como span attribute; el
//! export OTEL se cablea en los bins.

// TODO: init_tracing() + capa tracing-opentelemetry; helpers de correlación.
