//! Hand-written JSON Schemas for the tools exposed by `gt-mcp`. The doc envisions
//! generating these from the command enums (`docs/09-llm-integration.md`); for now we
//! ship the schemas inline so the registry has stable contracts to publish via
//! `tools/list`. Adding a new command means adding its schema here and a dispatch
//! arm in `tools.rs`.

use serde_json::{json, Value};

pub fn agent_add_input() -> Value {
    json!({
        "type": "object",
        "required": ["id", "rig"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "rig": { "type": "string", "minLength": 1 }
        },
        "additionalProperties": false
    })
}

pub fn agent_remove_input() -> Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "minLength": 1 }
        },
        "additionalProperties": false
    })
}

pub fn agent_transition_input() -> Value {
    json!({
        "type": "object",
        "required": ["id", "to"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "to": { "type": "string", "enum": ["spawned", "working", "done", "killed"] }
        },
        "additionalProperties": false
    })
}

pub fn empty_output() -> Value {
    json!({ "type": "object", "additionalProperties": false })
}
