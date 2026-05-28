//! `gt-mcp` library — MCP-style tool dispatch over a domain actor.
//!
//! See `apps/api/docs/09-llm-integration.md`. The library exposes the pure dispatch
//! pieces (registry, schema, scope auth, audit sink); `main.rs` wires them onto stdio
//! JSON-RPC. Tests drive `dispatch` directly without spawning the binary so the
//! protocol layer stays thin and replaceable.

pub mod audit;
pub mod auth;
pub mod schema;
pub mod server;
pub mod tools;

pub use audit::{AuditEvent, AuditSink, InMemoryAudit};
pub use auth::Scope;
pub use server::{serve_stdio, Dispatcher};
pub use tools::{tool_descriptors, ToolError, ToolRegistry};
