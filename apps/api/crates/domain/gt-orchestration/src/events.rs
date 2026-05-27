use serde::{Deserialize, Serialize};

use gt_events::EventKind;

/// Orchestration domain events. `Serialize`/`Deserialize` for the audit log + replay.
///
/// A **convoy** drives an ordered set of member beads to completion: it feeds the next
/// ready member when the current one finishes (handoff) and closes when all members are
/// done. The events split into two roles, exactly like `gt-patrol` and `gt-merge`:
///
/// - **Inputs** observed at the edge and recorded so replay can rebuild the board:
///   `ConvoyCreated`, `ConvoyLaunched`, `MemberCompleted`, `MemberFailed`.
/// - **Outputs**: domain decisions the composition root reacts to. `MemberDispatched` is
///   the *delegation/handoff* the mayor/deacon turns into a `gt sling`; `ConvoyClosed` /
///   `ConvoyFailed` close the convoy bead. They are recorded too, so replay reconstructs
///   which member is active and how the convoy ended.
///
/// The core never reads the clock here: a convoy advances on *facts* (a member finished),
/// not on elapsed time — so this domain is trivially replay-able (`docs/06-observability.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchEvent {
    /// A convoy was planned: an ordered list of member beads to drive to completion.
    /// Starts `Staged` — not yet feeding crew.
    ConvoyCreated { convoy: String, members: Vec<String> },
    /// Mayor/deacon released the convoy: `Staged → Launched`. The actor reacts by feeding
    /// the first member.
    ConvoyLaunched { convoy: String },
    /// Delegation / handoff: feed this member to crew now. Emitted on launch (first member)
    /// and after each completion (next ready member). The composition root reacts by
    /// slinging the member bead.
    MemberDispatched { convoy: String, member: String },
    /// A crew member finished its bead (observed when the member bead closes).
    MemberCompleted { convoy: String, member: String },
    /// A crew member's bead failed.
    MemberFailed { convoy: String, member: String, reason: String },
    /// All members done: the convoy bead can be closed. `Launched → Closed`.
    ConvoyClosed { convoy: String },
    /// A member failed and halted the convoy. `Launched → Failed`.
    ConvoyFailed { convoy: String, member: String, reason: String },
}

impl EventKind for OrchEvent {
    fn kind(&self) -> &'static str {
        match self {
            OrchEvent::ConvoyCreated { .. } => "orch.convoy_created",
            OrchEvent::ConvoyLaunched { .. } => "orch.convoy_launched",
            OrchEvent::MemberDispatched { .. } => "orch.member_dispatched",
            OrchEvent::MemberCompleted { .. } => "orch.member_completed",
            OrchEvent::MemberFailed { .. } => "orch.member_failed",
            OrchEvent::ConvoyClosed { .. } => "orch.convoy_closed",
            OrchEvent::ConvoyFailed { .. } => "orch.convoy_failed",
        }
    }
}
