//! Taxonomy enums for bead/epic creation (hq-taxon.1).
//!
//! Spec: `apps/api/docs/14-bead-taxonomy.md`.
//!
//! Closed set of `Domain` values anchored to the `apps/api/crates/` partition
//! plus the frontend and deployment layers; `Role` mirrors the role crates in
//! `apps/api/crates/domain/roles/`. Validation against the role↔domain
//! cross-product lives in hq-taxon.2; this module only provides the
//! cross-product table via [`Role::allows`] so .2 can plug in without growing
//! the enum API surface again.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Closed set of bead/epic domains. Adding a variant is a deliberate breaking
/// change — the gap-mint path in `meta.report_gap` should surface unknown
/// domains as a `hq-gap-domain-<slug>` bead instead of relaxing this enum.
///
/// Wire form preserves the dotted-namespace shape (`kernel.events`,
/// `orch.merge`, etc.) used in the spec so JSON payloads read like the doc.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum Domain {
    // Kernel — apps/api/crates/kernel/*
    #[serde(rename = "kernel.events")]
    KernelEvents,
    #[serde(rename = "kernel.bus")]
    KernelBus,
    #[serde(rename = "kernel.audit")]
    KernelAudit,
    #[serde(rename = "kernel.telemetry")]
    KernelTelemetry,
    #[serde(rename = "kernel.plugin")]
    KernelPlugin,
    #[serde(rename = "kernel.channel")]
    KernelChannel,
    #[serde(rename = "kernel.root")]
    KernelRoot,

    // Lifecycle — apps/api/crates/domain/lifecycle/*
    #[serde(rename = "lifecycle.agent")]
    LifecycleAgent,
    #[serde(rename = "lifecycle.polecat")]
    LifecyclePolecat,

    // Orchestration — apps/api/crates/domain/orchestration/*
    #[serde(rename = "orch.scheduling")]
    OrchScheduling,
    #[serde(rename = "orch.patrol")]
    OrchPatrol,
    #[serde(rename = "orch.merge")]
    OrchMerge,
    #[serde(rename = "orch.quota")]
    OrchQuota,
    #[serde(rename = "orch.convoy")]
    OrchConvoy,

    // Platform — apps/api/crates/domain/platform/*
    #[serde(rename = "platform.feed")]
    PlatformFeed,
    #[serde(rename = "platform.notify")]
    PlatformNotify,
    #[serde(rename = "platform.rig")]
    PlatformRig,
    #[serde(rename = "platform.wisp")]
    PlatformWisp,

    // Roles — apps/api/crates/domain/roles/*
    #[serde(rename = "role.sheriff")]
    RoleSheriff,
    #[serde(rename = "role.deacon")]
    RoleDeacon,
    #[serde(rename = "role.refinery")]
    RoleRefinery,
    #[serde(rename = "role.witness")]
    RoleWitness,
    #[serde(rename = "role.mayor")]
    RoleMayor,

    // Bins — apps/api/crates/bins/*
    #[serde(rename = "bin.gt")]
    BinGt,
    #[serde(rename = "bin.gt-web")]
    BinGtWeb,
    #[serde(rename = "bin.gt-mcp")]
    BinGtMcp,
    #[serde(rename = "bin.gt-mcp-cli")]
    BinGtMcpCli,

    // Stores — gt-store-* plus the bead port
    #[serde(rename = "store.dolt")]
    StoreDolt,
    #[serde(rename = "store.pg")]
    StorePg,
    #[serde(rename = "store.beads")]
    StoreBeads,

    // Frontend — apps/web/*
    #[serde(rename = "fe.web")]
    FeWeb,
    #[serde(rename = "fe.docs")]
    FeDocs,

    // Deploy and docs
    #[serde(rename = "deploy.compose")]
    DeployCompose,
    #[serde(rename = "deploy.dolt")]
    DeployDolt,
    #[serde(rename = "docs.spec")]
    DocsSpec,

    // Meta — emitted by `meta.report_gap` auto-mints
    #[serde(rename = "meta.gap")]
    MetaGap,
}

impl Domain {
    /// Returns the namespace prefix (the part before the dot) for layer-level
    /// allowances — e.g. `Domain::BinGtWeb` returns `"bin"`. Used by
    /// [`Role::allows`] to grant cross-cutting access to whole layers.
    fn layer(self) -> &'static str {
        match self {
            Domain::KernelEvents
            | Domain::KernelBus
            | Domain::KernelAudit
            | Domain::KernelTelemetry
            | Domain::KernelPlugin
            | Domain::KernelChannel
            | Domain::KernelRoot => "kernel",
            Domain::LifecycleAgent | Domain::LifecyclePolecat => "lifecycle",
            Domain::OrchScheduling
            | Domain::OrchPatrol
            | Domain::OrchMerge
            | Domain::OrchQuota
            | Domain::OrchConvoy => "orch",
            Domain::PlatformFeed
            | Domain::PlatformNotify
            | Domain::PlatformRig
            | Domain::PlatformWisp => "platform",
            Domain::RoleSheriff
            | Domain::RoleDeacon
            | Domain::RoleRefinery
            | Domain::RoleWitness
            | Domain::RoleMayor => "role",
            Domain::BinGt
            | Domain::BinGtWeb
            | Domain::BinGtMcp
            | Domain::BinGtMcpCli => "bin",
            Domain::StoreDolt | Domain::StorePg | Domain::StoreBeads => "store",
            Domain::FeWeb | Domain::FeDocs => "fe",
            Domain::DeployCompose | Domain::DeployDolt => "deploy",
            Domain::DocsSpec => "docs",
            Domain::MetaGap => "meta",
        }
    }
}

/// Role taxonomy. Mirrors the role crates in
/// `apps/api/crates/domain/roles/gt-{sheriff,deacon,refinery,witness,mayor}/`.
/// Serialized as snake_case for symmetry with the rest of the MCP wire format.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Sheriff,
    Deacon,
    Refinery,
    Witness,
    Mayor,
}

impl Role {
    /// True when a bead with `role_scope = self` may declare `domain`. The
    /// allowance combines:
    ///
    /// - The role's per-role-domain (a role can always own its own
    ///   `role.<self>` domain).
    /// - The role's permitted cross-product per docs/14 §3.5.
    /// - Cross-cutting layers any role may touch: `bin.*`, `store.*`,
    ///   `kernel.*`, and `docs.*` (spec edits never need a role to split).
    ///
    /// `meta.gap` is rejected on roles since gap auto-mints are role-less by
    /// construction.
    pub fn allows(self, domain: Domain) -> bool {
        // Anyone-allowed layers.
        match domain.layer() {
            "bin" | "store" | "kernel" | "docs" => return true,
            _ => {}
        }

        // Self ownership: role X may own role.X.
        if matches!(
            (self, domain),
            (Role::Sheriff, Domain::RoleSheriff)
                | (Role::Deacon, Domain::RoleDeacon)
                | (Role::Refinery, Domain::RoleRefinery)
                | (Role::Witness, Domain::RoleWitness)
                | (Role::Mayor, Domain::RoleMayor)
        ) {
            return true;
        }

        // Per-role cross-product.
        matches!(
            (self, domain),
            (Role::Sheriff, Domain::OrchMerge)
                | (Role::Deacon, Domain::OrchScheduling)
                | (Role::Deacon, Domain::OrchPatrol)
                | (Role::Refinery, Domain::OrchMerge)
                | (Role::Refinery, Domain::OrchQuota)
                | (Role::Witness, Domain::KernelTelemetry)
                | (Role::Witness, Domain::KernelAudit)
                | (Role::Mayor, Domain::LifecycleAgent)
                | (Role::Mayor, Domain::LifecyclePolecat)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_serializes_with_dots() {
        let v = serde_json::to_string(&Domain::OrchMerge).unwrap();
        assert_eq!(v, "\"orch.merge\"");
        let v = serde_json::to_string(&Domain::BinGtMcpCli).unwrap();
        assert_eq!(v, "\"bin.gt-mcp-cli\"");
    }

    #[test]
    fn role_serializes_snake_case() {
        let v = serde_json::to_string(&Role::Refinery).unwrap();
        assert_eq!(v, "\"refinery\"");
    }

    #[test]
    fn sheriff_blocked_on_quota() {
        // Reading the spec: sheriff handles merge governance + plugin, not quota.
        assert!(!Role::Sheriff.allows(Domain::OrchQuota));
        assert!(Role::Sheriff.allows(Domain::OrchMerge));
        assert!(Role::Sheriff.allows(Domain::KernelPlugin));
    }

    #[test]
    fn refinery_owns_quota_and_merge() {
        assert!(Role::Refinery.allows(Domain::OrchQuota));
        assert!(Role::Refinery.allows(Domain::OrchMerge));
        assert!(!Role::Refinery.allows(Domain::OrchPatrol));
    }

    #[test]
    fn anyone_layer_passes_for_every_role() {
        for role in [Role::Sheriff, Role::Deacon, Role::Refinery, Role::Witness, Role::Mayor] {
            assert!(role.allows(Domain::BinGt));
            assert!(role.allows(Domain::StoreDolt));
            assert!(role.allows(Domain::KernelRoot));
            assert!(role.allows(Domain::DocsSpec));
        }
    }

    #[test]
    fn role_owns_self_domain() {
        assert!(Role::Mayor.allows(Domain::RoleMayor));
        assert!(!Role::Mayor.allows(Domain::RoleSheriff));
    }

    #[test]
    fn meta_gap_rejected_for_every_role() {
        for role in [Role::Sheriff, Role::Deacon, Role::Refinery, Role::Witness, Role::Mayor] {
            assert!(!role.allows(Domain::MetaGap));
        }
    }
}
