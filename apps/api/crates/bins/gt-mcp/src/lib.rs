//! `gt-mcp` library — MCP-style tool dispatch over a domain actor.
//!
//! See `apps/api/docs/09-llm-integration.md`. The library exposes the dispatch
//! pieces (`McpService`, `Scope` auth, `AuditSink`) on top of the official `rmcp`
//! Rust SDK; `main.rs` mounts the service over stdio. Tests drive
//! [`McpService::run`] directly so the same code path the macro-generated tool
//! router uses at runtime is covered without a real transport.

pub mod audit;
pub mod auth;
pub mod http;
pub mod service;

pub use audit::{AuditEvent, AuditSink, InMemoryAudit, JsonlAudit, Outcome};
pub use auth::{Scope, ScopeConfig, ScopeSpec};
pub use service::{
    CreateBead, IssuesRead, McpService, RegisterAccount, ReportGap, RetireAccount, SessionsRead,
};
