//! Frontier authorization. The domain knows nothing about identity; `gt-mcp` resolves
//! it at the start of the connection and attaches a [`Scope`] to every dispatch.
//!
//! Patterns are exact or trailing-`*` globs over the dotted tool name (e.g. `agent.*`).
//!
//! Scopes are **not hardcoded**: the binary loads a per-actor [`ScopeConfig`] from a TOML or
//! JSON file (`docs/09-llm-integration.md`). An actor with no entry resolves to a closed
//! scope ([`Scope::denied`]) — deny by default, never admin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use gt_events::AppError;

#[derive(Debug, Clone)]
pub struct Scope {
    pub actor: String,
    pub allow: BTreeSet<String>,
    pub validate_only: bool,
}

impl Scope {
    pub fn admin(actor: impl Into<String>) -> Self {
        let mut allow = BTreeSet::new();
        allow.insert("*".into());
        Self {
            actor: actor.into(),
            allow,
            validate_only: false,
        }
    }

    pub fn read_only(actor: impl Into<String>) -> Self {
        let mut allow = BTreeSet::new();
        allow.insert("*".into());
        Self {
            actor: actor.into(),
            allow,
            validate_only: true,
        }
    }

    /// Closed scope: empty allow-list, so [`Scope::check`] rejects every tool. The
    /// default for an unknown actor or a missing config — deny first.
    pub fn denied(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            allow: BTreeSet::new(),
            validate_only: true,
        }
    }

    /// Returns `Ok` if the scope grants this `tool` and the action variant
    /// (`.validate` vs `.execute`) is permitted.
    pub fn check(&self, tool: &str) -> Result<(), AppError> {
        if self.validate_only && tool.ends_with(".execute") {
            return Err(AppError::Validation(format!(
                "scope {} is validate_only; cannot call {tool}",
                self.actor
            )));
        }
        let allowed = self.allow.iter().any(|pat| matches_pattern(pat, tool));
        if !allowed {
            return Err(AppError::Validation(format!(
                "tool {tool} not in scope for {}",
                self.actor
            )));
        }
        Ok(())
    }
}

fn matches_pattern(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return name == prefix || name.starts_with(&format!("{prefix}."));
    }
    pattern == name
}

/// One actor's grant in the config file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeSpec {
    /// Tool patterns the actor may call (exact or trailing-`*` glob). Empty = deny all.
    #[serde(default)]
    pub allow: BTreeSet<String>,
    /// If true, the actor may only call `*.validate` tools.
    #[serde(default)]
    pub validate_only: bool,
}

/// Per-actor scope configuration, loaded from TOML or JSON. There is **no admin default**:
/// an actor absent from the table resolves to [`Scope::denied`].
///
/// ```toml
/// [actors.max]
/// allow = ["*"]
///
/// [actors.watcher]
/// allow = ["*.sessions", "replay"]
/// validate_only = true
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeConfig {
    #[serde(default)]
    actors: BTreeMap<String, ScopeSpec>,
}

impl ScopeConfig {
    pub fn from_toml(s: &str) -> Result<Self, AppError> {
        toml::from_str(s).map_err(|e| AppError::Validation(format!("scope config (toml): {e}")))
    }

    pub fn from_json(s: &str) -> Result<Self, AppError> {
        serde_json::from_str(s).map_err(|e| AppError::Validation(format!("scope config (json): {e}")))
    }

    /// Load from a file, choosing the parser by extension (`.json` → JSON, otherwise TOML).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .map_err(|e| AppError::Other(format!("read scope config {}: {e}", path.display())))?;
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Self::from_json(&raw),
            _ => Self::from_toml(&raw),
        }
    }

    /// Resolve the scope for `actor`. Unknown actor → [`Scope::denied`] (deny by default).
    pub fn resolve(&self, actor: &str) -> Scope {
        match self.actors.get(actor) {
            Some(spec) => Scope {
                actor: actor.to_string(),
                allow: spec.allow.clone(),
                validate_only: spec.validate_only,
            },
            None => Scope::denied(actor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(set: &[&str]) -> BTreeSet<String> {
        set.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn admin_passes_everything() {
        let s = Scope::admin("max");
        s.check("agent.add.execute").unwrap();
        s.check("scheduling.enqueue.validate").unwrap();
    }

    #[test]
    fn validate_only_blocks_execute() {
        let s = Scope::read_only("watcher");
        s.check("agent.transition.validate").unwrap();
        assert!(s.check("agent.transition.execute").is_err());
    }

    #[test]
    fn glob_matches_dotted_prefix() {
        let s = Scope {
            actor: "scoped".into(),
            allow: allow(&["agent.*"]),
            validate_only: false,
        };
        s.check("agent.add.execute").unwrap();
        assert!(s.check("scheduling.enqueue.execute").is_err());
    }

    #[test]
    fn denied_scope_rejects_everything() {
        let s = Scope::denied("ghost");
        assert!(s.check("agent.add.validate").is_err());
        assert!(s.check("agent.add.execute").is_err());
    }

    const TOML_CFG: &str = r#"
[actors.max]
allow = ["*"]

[actors.watcher]
allow = ["agent.*", "scheduling.enqueue.validate"]
validate_only = true
"#;

    const JSON_CFG: &str = r#"
{ "actors": {
    "max": { "allow": ["*"] },
    "watcher": { "allow": ["agent.*", "scheduling.enqueue.validate"], "validate_only": true }
} }
"#;

    fn assert_resolves(cfg: &ScopeConfig) {
        // max: full admin via "*".
        let max = cfg.resolve("max");
        assert!(!max.validate_only);
        max.check("orch.launch_convoy.execute").unwrap();

        // watcher: scoped + validate_only.
        let watcher = cfg.resolve("watcher");
        assert!(watcher.validate_only);
        watcher.check("agent.transition.validate").unwrap();
        assert!(watcher.check("agent.transition.execute").is_err(), "validate_only blocks execute");
        assert!(watcher.check("merge.submit.validate").is_err(), "not in allow list");

        // unknown actor: denied by default — no admin.
        let ghost = cfg.resolve("ghost");
        assert!(ghost.check("agent.add.validate").is_err());
    }

    #[test]
    fn toml_and_json_configs_resolve_per_actor() {
        assert_resolves(&ScopeConfig::from_toml(TOML_CFG).unwrap());
        assert_resolves(&ScopeConfig::from_json(JSON_CFG).unwrap());
    }

    #[test]
    fn empty_config_denies_all_actors() {
        let cfg = ScopeConfig::default();
        assert!(cfg.resolve("anyone").check("agent.add.validate").is_err());
    }
}
