//! Tool registry and dispatch.
//!
//! Each domain command exposes two MCP tool variants: `<name>.validate` and
//! `<name>.execute`. The registry parses the dotted tool id, decodes the arguments
//! into the typed `AgentCommand`, and forwards through the actor handle. The actor
//! revalidates inside the same tick on `Exec`, so any state shift between a prior
//! `.validate` and a later `.execute` surfaces as an error from `.execute` — not as a
//! silent mismatch.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use gt_agent::actor::AgentHandle;
use gt_agent::{AddSession, AgentCommand, RemoveSession, SessionState, TransitionSession};
use gt_events::AppError;

use crate::schema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema")]
    pub output_schema: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid arguments for {tool}: {reason}")]
    InvalidArguments { tool: String, reason: String },
    #[error("domain error: {0}")]
    Domain(#[from] AppError),
}

/// Static descriptors for `tools/list`. Generated from the inline schemas in
/// [`schema`]. Order is stable so MCP clients can rely on it.
pub fn tool_descriptors() -> Vec<ToolDescriptor> {
    let pairs: &[(&str, &str, fn() -> Value)] = &[
        ("agent.add", "Add a new session to the registry.", schema::agent_add_input),
        ("agent.remove", "Remove a session by id.", schema::agent_remove_input),
        (
            "agent.transition",
            "Transition a session to a new lifecycle state.",
            schema::agent_transition_input,
        ),
    ];
    let mut out = Vec::with_capacity(pairs.len() * 2);
    for (base, desc, input) in pairs {
        for variant in ["validate", "execute"] {
            out.push(ToolDescriptor {
                name: format!("{base}.{variant}"),
                description: format!("{desc} (`{variant}` variant — see docs/09)."),
                input_schema: input(),
                output_schema: schema::empty_output(),
            });
        }
    }
    out
}

/// Outcome shape returned by both `.validate` and `.execute` variants — empty success
/// payload, the failure mode rides on `ToolError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationOk {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Validate,
    Execute,
}

/// Decode a `<base>.<variant>` tool id. Returns the base name and the variant.
pub fn split_tool_id(tool: &str) -> Result<(&str, Variant), ToolError> {
    let (base, variant) = tool
        .rsplit_once('.')
        .ok_or_else(|| ToolError::UnknownTool(tool.into()))?;
    let variant = match variant {
        "validate" => Variant::Validate,
        "execute" => Variant::Execute,
        _ => return Err(ToolError::UnknownTool(tool.into())),
    };
    Ok((base, variant))
}

/// Decode the typed `AgentCommand` from a base tool name + arguments JSON.
pub fn decode_command(base: &str, arguments: &Value) -> Result<AgentCommand, ToolError> {
    let invalid = |reason: String| ToolError::InvalidArguments {
        tool: base.to_string(),
        reason,
    };
    match base {
        "agent.add" => {
            let cmd: AddSession =
                serde_json::from_value(arguments.clone()).map_err(|e| invalid(e.to_string()))?;
            Ok(AgentCommand::Add(cmd))
        }
        "agent.remove" => {
            let cmd: RemoveSession =
                serde_json::from_value(arguments.clone()).map_err(|e| invalid(e.to_string()))?;
            Ok(AgentCommand::Remove(cmd))
        }
        "agent.transition" => {
            #[derive(Deserialize)]
            struct Args {
                id: String,
                to: SessionState,
            }
            let args: Args =
                serde_json::from_value(arguments.clone()).map_err(|e| invalid(e.to_string()))?;
            Ok(AgentCommand::Transition(TransitionSession {
                id: args.id,
                to: args.to,
            }))
        }
        other => Err(ToolError::UnknownTool(other.into())),
    }
}

/// Live registry bound to an `AgentHandle`. Holds no state itself; the actor owns
/// the registry. Cheap to clone.
#[derive(Clone)]
pub struct ToolRegistry {
    agent: AgentHandle,
}

impl ToolRegistry {
    pub fn new(agent: AgentHandle) -> Self {
        Self { agent }
    }

    pub async fn call(&self, tool: &str, arguments: &Value) -> Result<InvocationOk, ToolError> {
        let (base, variant) = split_tool_id(tool)?;
        let cmd = decode_command(base, arguments)?;
        match variant {
            Variant::Validate => self.agent.validate(cmd).await?,
            Variant::Execute => self.agent.exec(cmd).await?,
        }
        Ok(InvocationOk {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_cover_validate_and_execute() {
        let names: Vec<String> = tool_descriptors().into_iter().map(|d| d.name).collect();
        for base in ["agent.add", "agent.remove", "agent.transition"] {
            assert!(names.contains(&format!("{base}.validate")));
            assert!(names.contains(&format!("{base}.execute")));
        }
    }

    #[test]
    fn decode_rejects_unknown_state() {
        let err = decode_command(
            "agent.transition",
            &serde_json::json!({"id": "p1", "to": "exploded"}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
