//! Line-delimited JSON-RPC 2.0 over stdio. A pragmatic subset of MCP suitable for
//! local development and tests; swap to an official Rust MCP SDK in a follow-up
//! without touching the dispatch layer.
//!
//! Methods handled:
//! - `tools/list` → returns [`ToolDescriptor`]s.
//! - `tools/call` → params: `{ name, arguments }`. Drives the registry through the
//!   scope check, records audit, returns `{}` on success or `error` on failure.
//!
//! Anything else returns JSON-RPC error `-32601 method not found`.

use std::io::{self};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::audit::{AuditEvent, AuditSink, Outcome};
use crate::auth::Scope;
use crate::tools::ToolRegistry;

#[derive(Debug, Deserialize)]
pub struct Request {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// Dispatcher composes the registry, scope and audit sink — the three things the
/// stdio loop and the tests both need.
#[derive(Clone)]
pub struct Dispatcher {
    pub registry: ToolRegistry,
    pub scope: Scope,
    pub audit: Arc<dyn AuditSink>,
}

impl Dispatcher {
    pub fn new(registry: ToolRegistry, scope: Scope, audit: Arc<dyn AuditSink>) -> Self {
        Self { registry, scope, audit }
    }

    /// Handle one decoded JSON-RPC request and produce its response body.
    pub async fn handle(&self, req: Request) -> Response {
        let id = req.id.clone();
        if req.jsonrpc != "2.0" {
            return error_response(id, -32600, "expected jsonrpc 2.0".into());
        }
        match req.method.as_str() {
            "tools/list" => {
                let descriptors = crate::tools::tool_descriptors();
                success(id, json!({ "tools": descriptors }))
            }
            "tools/call" => self.handle_call(id, &req.params).await,
            other => error_response(id, -32601, format!("method not found: {other}")),
        }
    }

    async fn handle_call(&self, id: Option<Value>, params: &Value) -> Response {
        let name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => {
                return error_response(id, -32602, "missing tool name".into());
            }
        };
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        if let Err(err) = self.scope.check(&name) {
            self.audit.record(AuditEvent::Unauthorized {
                actor: self.scope.actor.clone(),
                tool: name.clone(),
                reason: err.to_string(),
            });
            return error_response(id, -32001, err.to_string());
        }

        let outcome = match self.registry.call(&name, &arguments).await {
            Ok(_) => Outcome::Ok,
            Err(err) => Outcome::Failed { error: err.to_string() },
        };

        self.audit.record(AuditEvent::Invoked {
            actor: self.scope.actor.clone(),
            tool: name.clone(),
            arguments: arguments.clone(),
            outcome: outcome.clone(),
        });

        match outcome {
            Outcome::Ok => success(id, json!({})),
            Outcome::Failed { error } => {
                let code = if matches!(
                    classify(&error),
                    ErrorClass::InvalidArguments
                ) {
                    -32602
                } else {
                    -32000
                };
                error_response(id, code, error)
            }
        }
    }
}

fn success(id: Option<Value>, result: Value) -> Response {
    Response {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: Option<Value>, code: i64, message: String) -> Response {
    Response {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(RpcError { code, message }),
    }
}

enum ErrorClass {
    InvalidArguments,
    Other,
}

fn classify(message: &str) -> ErrorClass {
    if message.contains("invalid arguments") {
        ErrorClass::InvalidArguments
    } else {
        ErrorClass::Other
    }
}

/// Drive the dispatcher off stdin/stdout. Each line is one JSON-RPC frame.
/// Returns on EOF or an unrecoverable write error.
pub async fn serve_stdio(dispatcher: Dispatcher) -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatcher.handle(req).await,
            Err(err) => Response {
                jsonrpc: "2.0",
                id: None,
                result: None,
                error: Some(RpcError {
                    code: -32700,
                    message: format!("parse error: {err}"),
                }),
            },
        };
        let line = serde_json::to_string(&response).map_err(io::Error::other)?;
        stdout.write_all(line.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

// Tests for the dispatcher live in `tests/mcp_e2e.rs` — they drive `Dispatcher::handle`
// directly so the wire format and the actor path are both covered without a child
// process. `serve_stdio` is the thin glue used by `main`.

/// Build a request from its parts. Lets tests construct requests without a
/// custom serde-aware impl on `Request`.
pub fn make_request(id: Value, method: &str, params: Value) -> Request {
    Request {
        jsonrpc: "2.0".into(),
        id: Some(id),
        method: method.into(),
        params,
    }
}
