//! Frontier authorization. The domain knows nothing about identity; `gt-mcp` resolves
//! it at the start of the connection and attaches a [`Scope`] to every dispatch.
//!
//! Patterns are exact or trailing-`*` globs over the dotted tool name (e.g. `agent.*`).

use std::collections::BTreeSet;

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
}
