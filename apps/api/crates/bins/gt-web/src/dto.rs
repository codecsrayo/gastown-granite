//! Wire DTOs. The browser frontend never sees domain types directly — DTOs are the stable
//! JSON contract (`docs/07-frontend.md`). Translating here keeps refactors of the domain
//! invisible to clients.

use serde::{Deserialize, Serialize};

use gt_agent::{Session, SessionState};
use gt_beads::{Bead, BeadStatus};
use gt_events::EventKind;
use gt_login::LoginFailure;

/// One row of `GET /api/sessions`. `role`/`crew` (hq-8iur.7) expose the agent kind and the
/// crew running inside a polecat as the flat canonical strings the frontend can filter on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDto {
    pub id: String,
    pub rig: String,
    pub state: String,
    pub role: String,
    pub crew: Option<String>,
}

impl From<Session> for SessionDto {
    fn from(s: Session) -> Self {
        Self {
            id: s.id,
            rig: s.rig,
            state: match s.state {
                SessionState::Spawned => "spawned",
                SessionState::Working => "working",
                SessionState::Done => "done",
                SessionState::Killed => "killed",
            }
            .to_string(),
            role: s.role.as_str().to_string(),
            crew: s.crew,
        }
    }
}

/// Query for `GET /api/sessions[?role=polecat][&rig=hq]`. Both filters are independent and
/// AND together — absent = no constraint on that axis. Unknown values yield an empty result
/// (filter is a view, not a command), matching the `role` semantics in
/// [`crate::routes::list_sessions`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionsQuery {
    pub role: Option<String>,
    pub rig: Option<String>,
}

/// One row of `GET /api/beads`. Mirrors the columns the dashboard already reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BeadDto {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: u8,
    pub assignee: Option<String>,
}

impl From<Bead> for BeadDto {
    fn from(b: Bead) -> Self {
        Self {
            id: b.id,
            title: b.title,
            status: b.status.as_str().to_string(),
            priority: b.priority,
            assignee: b.assignee,
        }
    }
}

/// Body of `POST /api/beads` (hq-fe-api-w.3). Mints a `pending` bead in the dispatcher.
/// Status is fixed by the handler so the kanban can never spawn a bead mid-lifecycle.
#[derive(Debug, Clone, Deserialize)]
pub struct BeadCreateRequest {
    pub id: String,
    pub title: String,
    /// `0..=2` (0 = P0). Defaults to `2`.
    #[serde(default = "default_bead_priority")]
    pub priority: u8,
    /// Optional initial assignee. Empty string clears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

fn default_bead_priority() -> u8 {
    2
}

/// Body of `POST /api/beads/bulk` (hq-fe-api-w.11). Atomic "create N beads" call so the
/// dashboard's import flow does not need N round-trips. The handler validates every
/// item against the same rules `POST /api/beads` enforces (non-empty id+title,
/// `priority 0..=2`) and refuses the whole batch on the first failure — partial
/// success would leave the kanban in a state the operator did not request.
///
/// Hard cap on `beads.len()` prevents one request from monopolizing the dispatcher;
/// rate-limit middleware fronts the route so the cap interacts with per-actor budget.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkBeadCreateRequest {
    pub beads: Vec<BeadCreateRequest>,
}

/// Response of `POST /api/beads/bulk`. Echoes every created row in the same order the
/// request listed them so the caller can pair input ↔ persisted state in one pass.
#[derive(Debug, Clone, Serialize)]
pub struct BulkBeadCreateResponse {
    pub created: Vec<BeadDto>,
}

/// Body of `POST /api/beads/:id/comments` (hq-fe-api-w.5). Appends a free-text
/// comment to `hq.issues.notes`. The route formats a canonical fragment
/// (timestamp + author tag + body + newline) so the column stays parseable
/// even though the storage is flat text; a future migration to a structured
/// `issue_comments` table can split fragments on the same separators.
#[derive(Debug, Clone, Deserialize)]
pub struct BeadCommentRequest {
    /// Comment body. Non-empty; the route rejects whitespace-only payloads so
    /// audit / SSE always carry context for the append.
    pub body: String,
    /// Optional author tag. Empty / absent stores as `@anon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// Response of `POST /api/beads/:id/comments`. Echoes the formatted fragment
/// the route appended so the dashboard can render the new note inline without
/// re-fetching the row.
#[derive(Debug, Clone, Serialize)]
pub struct BeadCommentResponse {
    pub id: String,
    pub appended: String,
    /// RFC3339 timestamp embedded in `appended`. Surfaced separately so the
    /// dashboard can sort comments without re-parsing the fragment.
    pub ts: String,
}

/// Body of `PATCH /api/beads/:id` (hq-fe-api-w.3). Every editable field is `Option`:
/// `None` leaves it alone, `Some(_)` overwrites. Status is deliberately absent — status
/// transitions live on the dispatcher reactor (claim / release / done / failed events).
/// `is_empty` is the route's check that the caller had something to update.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BeadUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// `Some("")` clears the assignee to "unassigned"; `None` leaves the column alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

impl BeadUpdateRequest {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.priority.is_none() && self.assignee.is_none()
    }
}

/// Body of `POST /api/beads/:id/transition` (hq-fe-api-w.4). Manual override for the
/// dispatcher's bead state machine: when a worker dies before the reactor closes the
/// bead, or when the operator wants to mark a `pending` row done/failed without running
/// the dispatch flow, this route flips the status field in-place.
///
/// The set of permitted transitions is intentionally narrower than the full Cartesian
/// product (see [`crate::routes::transition_bead`]): scheduler-owned moves
/// (`pending` → `dispatched`) stay on `scheduling.mark_dispatched`, and crossing
/// `done` ↔ `failed` directly must round-trip through `pending` so the re-open is
/// explicit in the audit trail. Operator overrides do **not** touch dispatcher capacity
/// — a parallel worker holding a real claim must still close it via the reactor path.
#[derive(Debug, Clone, Deserialize)]
pub struct BeadTransitionRequest {
    /// Target status: `pending|dispatched|working|done|failed`. Validated against the
    /// state machine before the upsert; unknown values return 400.
    pub to: String,
}

/// Query for `GET /api/beads?status=pending`. Default = pending (the operator-visible queue).
#[derive(Debug, Clone, Deserialize)]
pub struct BeadsQuery {
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "pending".to_string()
}

impl BeadsQuery {
    pub fn parsed(&self) -> Option<BeadStatus> {
        BeadStatus::parse(&self.status)
    }
}

/// Body of `POST /api/convoys` (hq-fe-api-w.9). Launches a fresh convoy with an ordered
/// member list; the orchestrator dispatches members one at a time as each completes.
/// Mirrors `gt_orchestration::LaunchConvoy` so the route is a thin HTTP transport over
/// `OrchCommand::Launch` — pause/resume stay deferred until the domain ships them
/// (`gap parcial` in the migration plan).
#[derive(Debug, Clone, Deserialize)]
pub struct ConvoyCreateRequest {
    pub convoy: String,
    pub members: Vec<String>,
}

/// Response of `POST /api/convoys`. Echoes the convoy id + members so the dashboard can
/// render the new row without a follow-up `GET /api/convoys`. `launched: true` is a
/// fixed marker — `OrchCommand::Launch` either succeeds (and the first member is already
/// dispatched) or returns a 4xx, so a successful response always means the convoy is live.
#[derive(Debug, Clone, Serialize)]
pub struct ConvoyCreateResponse {
    pub convoy: String,
    pub members: Vec<String>,
    pub launched: bool,
}

/// One row of `GET /api/convoys` (hq-fe-api-r.3). HTTP mirror of the `gt://orch/convoys`
/// MCP resource. `state` is the canonical lifecycle string (`staged|launched|closed|failed`)
/// from `gt_orchestration::state::ConvoyState::as_str()`. Members are surfaced ordered the
/// same way the actor stores them so the dashboard preserves convoy ordering when rendering
/// the e-stop list (hq-fe-view.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvoyDto {
    pub id: String,
    pub state: String,
    pub members: Vec<ConvoyMemberDto>,
}

/// One member of a convoy. `bead` is the issue id this slot drives; `state` is the canonical
/// member lifecycle string (`pending|active|done|failed`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvoyMemberDto {
    pub bead: String,
    pub state: String,
}

/// Query for `GET /api/convoys?state=launched`. Absent = no filter (returns every convoy
/// the actor knows about). Unknown values yield `[]` rather than 400 — same posture as the
/// `?role=` filter on `/api/sessions`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConvoysQuery {
    pub state: Option<String>,
}

/// Query for `GET /api/feed?since=<rfc3339>&limit=<n>` (hq-fe-api-r.5). Historical replay of
/// the same `EventRecord`s the SSE `/api/stream` ships. `since` is RFC3339; absent or empty
/// returns the tail of the log. `limit` caps the response (default 500, max 2000) so the
/// dashboard's first-page seed stays bounded.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FeedQuery {
    pub since: Option<String>,
    pub limit: Option<usize>,
}

/// Body of `POST /api/convoys/:convoy/members/:member/fail` (hq-fe-api-w.9). Halts the
/// convoy with an operator-supplied reason. Path params carry the identifiers so a
/// curl smoke test can omit the body entirely when `reason` is optional — but we keep
/// it required here so audit / SSE always carry context for the failure.
#[derive(Debug, Clone, Deserialize)]
pub struct MemberFailRequest {
    pub reason: String,
}

/// Body of `POST /api/nudge` — a write-side command, not an event. The handler turns it into
/// an `AgentEvent::Heartbeat` on the agent relay; the actor records it and replay sees it.
#[derive(Debug, Clone, Deserialize)]
pub struct NudgeRequest {
    pub session: String,
}

/// Response of `POST /api/nudge`.
#[derive(Debug, Clone, Serialize)]
pub struct NudgeResponse {
    pub accepted: bool,
}

/// Body of `POST /api/quota/accounts/:id/rotate` (hq-fe-api-w.10). The source account
/// is the path segment; the body carries the healthy target. `now_secs` is optional —
/// when absent the route stamps `SystemTime::now()` so curl smoke tests stay one-liners.
#[derive(Debug, Clone, Deserialize)]
pub struct QuotaRotateRequest {
    pub to_account: String,
    #[serde(default)]
    pub now_secs: Option<u64>,
}

/// Response of `POST /api/quota/accounts/:id/retire`. `removed = false` when the id was
/// already absent — the route is idempotent.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaRetireResponse {
    pub account: String,
    pub removed: bool,
}

/// One row of `GET /api/quota/accounts` (hq-fe-api-r.1). Snapshot of every account the
/// quota actor knows about, flattened so the dashboard sidebar (hq-fe-view.10) can render
/// AccountCard + QuotaMeter without joining against the domain types. `tokens_used` /
/// `tokens_cap` / `reset_at` collapse `Account.window` into the three fields the meter
/// needs; `None` when the account has no live window yet (`upsert_account` happened but
/// `WindowReset` did not). `sessions` is reserved for the per-account pin list the actor
/// does not yet expose — wire it once `AccountRegistry` carries a session index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaAccountDto {
    pub id: String,
    /// Operational state collapsed to the three buckets the sidebar groups by:
    /// `active` (Healthy), `inactive` (Cooldown), `blocked` (Limited / Blocked).
    pub state: String,
    pub tokens_used: Option<u64>,
    pub tokens_cap: Option<u64>,
    pub reset_at: Option<u64>,
    pub sessions: Vec<String>,
}

/// One row of `waiting_unlock` on `GET /api/quota/rotation` (hq-fe-api-r.2). An account
/// currently parked in [`gt_quota::AccountQuotaStatus::Cooldown`] (typically the source
/// of a recent rotation) plus the best-effort wall time the dashboard can use to render
/// a countdown chip. The `unlock_at_secs` mirrors `account.window.resets_at_secs` — the
/// rolling-5h boundary the cooldown expires against; `None` when the account has no live
/// window yet (`upsert_account` happened but `WindowReset` did not).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaWaitingUnlockDto {
    pub account: String,
    pub status: String,
    pub unlock_at_secs: Option<u64>,
}

/// One row of `recent_rotations` on `GET /api/quota/rotation` (hq-fe-api-r.2). Surfaces a
/// `quota.rotated` record from the shared `events.jsonl` (same source the SSE feed ships)
/// flattened to the three columns the dashboard renders in the rotation banner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaRotationEntryDto {
    pub from: String,
    pub to: String,
    /// Wall time of the rotation, as RFC3339 (copied from `EventRecord.ts`).
    pub ts: String,
}

/// Response of `GET /api/quota/rotation` (hq-fe-api-r.2). Composite snapshot for the
/// dashboard's rotation panel: live `waiting_unlock` (Cooldown accounts) joined with the
/// last N `quota.rotated` log entries. Empty arrays — never a 404 — when no accounts are
/// in cooldown or no rotations have been logged, so the dashboard can render a stable
/// shell without conditional rendering on the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaRotationDto {
    pub waiting_unlock: Vec<QuotaWaitingUnlockDto>,
    pub recent_rotations: Vec<QuotaRotationEntryDto>,
}

/// Query for `GET /api/quota/rotation?since=<rfc3339>&limit=<n>` (hq-fe-api-r.2).
/// `since` filters `recent_rotations` to events strictly newer than the timestamp; `limit`
/// caps the response (default 50, max 500). `waiting_unlock` ignores both — it is always
/// the current live snapshot.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotaRotationQuery {
    pub since: Option<String>,
    pub limit: Option<usize>,
}

/// One row of `GET /api/worktrees` (hq-fe-api-r.8). Mirrors what VSCode's SCM panel renders
/// per-repo: the worktree path, its current branch and HEAD, the divergence vs. the rig's
/// default branch (`main` in hq), and the dirty file list. The dashboard joins this against
/// `GET /api/sessions` to label each row with the agent on it (branch convention
/// `claim/<bead-id>` per `apps/web/docs/frontend-features.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeDto {
    /// Absolute path of the worktree on disk (matches `git worktree list --porcelain` output).
    pub path: String,
    /// Branch name, or `None` for a detached HEAD (porcelain emits `detached` then).
    pub branch: Option<String>,
    /// 40-char object id at HEAD (porcelain emits `HEAD <sha>`).
    pub head: String,
    /// `true` for the main worktree (porcelain emits `bare` or no `worktree` parent flag).
    pub is_main: bool,
    /// Commits this branch has that `main` does not (right side of `main...HEAD`).
    pub ahead: u32,
    /// Commits `main` has that this branch does not (left side of `main...HEAD`).
    pub behind: u32,
    /// Working-tree changes (porcelain v2 lines), one entry per dirty path. Empty when clean.
    pub dirty: Vec<DirtyFileDto>,
    /// Subject line of the HEAD commit (`git log -1 --format=%s`). `None` when the worktree
    /// has no commits or the lookup failed — the row still renders, just without the hint.
    /// hq-fe-api-r.10.
    pub head_subject: Option<String>,
    /// Author name of the HEAD commit (`git log -1 --format=%an`). Same nullability rule as
    /// `head_subject`; the two are populated by the same `git log` call so they're either
    /// both present or both absent.
    pub head_author: Option<String>,
    /// Commit time of HEAD as Unix seconds (`git log -1 --format=%ct`). `None` when the
    /// worktree has no commits or the lookup failed. hq-fe-api-r.11 — the frontend sorts
    /// active rows by this so the most recently touched worktree rises to the top.
    pub head_time: Option<u64>,
}

/// One dirty path inside a worktree. Mirrors `git status --porcelain=v2` shape — the `xy`
/// 2-char code carries staged (`x`) + unstaged (`y`) state so the frontend can render the
/// same `M`/`U`/`A`/`?` glyphs VSCode does without re-parsing rules client-side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirtyFileDto {
    /// Path relative to the worktree root (matches porcelain v2 final column).
    pub path: String,
    /// Two-letter status code: index state + worktree state. `??` for untracked, `M.` for
    /// staged modify, `.M` for unstaged modify, `A.` for staged add, `UU` for unmerged.
    pub xy: String,
}

/// One row of `GET /api/issues` (hq-fe-api-r.9). Thin HTTP mirror of the `gt://issues`
/// MCP resource: surfaces the canonical `hq.issues` slice the dashboard joins against
/// (e.g. /worktrees cross-link hq-fe-view.15). Heavy fields (description/design/notes)
/// stay off the listing — clients fetch them per-id when a row is opened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueDto {
    pub id: String,
    pub title: String,
    /// Canonical `open|working|closed` from `hq.issues.status`. Distinct from
    /// `BeadDto.status` (which mirrors the dispatcher's `beads` table).
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    /// Parent epic id (the bead's `external_ref` column).
    pub external_ref: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
}

/// Response of `GET /api/whoami` (hq-fe-rbac.4). Surfaces the request actor + the
/// frontier's auth mode so the dashboard can short-circuit RBAC gating in dev
/// (`mode=open` → permissive Guard), enforce a single shared secret (`mode=bearer`), or
/// honour per-actor role/scope claims (`mode=jwt`, hq-fe-rbac.1). `roles`/`scopes` come
/// from the verified JWT claims when in JWT mode and stay empty in the other modes —
/// same wire shape so the FE never special-cases the posture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WhoamiDto {
    pub actor: String,
    /// Frontier posture: `open` (dev, every request passes), `bearer` (single shared
    /// token enforced), or `jwt` (per-actor HS256 token enforced).
    pub mode: String,
    pub roles: Vec<String>,
    pub scopes: Vec<String>,
}

/// One row of `GET /api/merges` (hq-fe-api-r.4). Mirrors `gt_merge::MergeSlot` with the
/// state enum flattened to the canonical string the SSE stream already emits
/// (`ready|merging|merged|failed`). The dashboard joins this against the SSE
/// `merge.*` events to project deltas without re-fetching the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeSlotDto {
    pub bead: String,
    pub branch: String,
    pub state: String,
}

impl From<gt_merge::MergeSlot> for MergeSlotDto {
    fn from(s: gt_merge::MergeSlot) -> Self {
        Self {
            bead: s.bead,
            branch: s.branch,
            state: s.state.as_str().to_string(),
        }
    }
}

/// Snapshot of `GET /api/mayor/status` (hq-fe-api-r.7). The dashboard's mayor strip
/// answers a single question: is the mayor session attached? `attached` reflects whether
/// the active-session registry currently holds a row with role=mayor; `session_id` + `rig`
/// surface the live row when one exists so the UI can deep-link the topbar.
///
/// Heartbeat freshness is intentionally deferred: the agent relay does not yet stamp
/// per-role heartbeats with a wall-clock ts the read side can compare against. When the
/// mayor heartbeat is plumbed, add `last_heartbeat: Option<String>` here without breaking
/// the existing contract (serde tolerates the new field).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MayorStatusDto {
    /// `true` when the session registry currently exposes a mayor.
    pub attached: bool,
    /// Live mayor session id, or `None` when detached.
    pub session_id: Option<String>,
    /// Rig the mayor is anchored to (typically "town"), or `None` when detached.
    pub rig: Option<String>,
    /// Lifecycle state of the mayor session as the registry sees it (spawned/working/done/
    /// killed). Same canonical string [`SessionDto::state`] surfaces.
    pub state: Option<String>,
}

/// Query for `GET /api/issues?status=working&external_ref=hq-fe-view&limit=50`. All fields
/// optional; absent = no filter. `status` accepts a comma-separated list so the dashboard
/// can pull `open,working` in one round-trip (mirrors the MCP resource's grammar).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IssuesQuery {
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub external_ref: Option<String>,
    pub issue_type: Option<String>,
    pub priority_max: Option<u8>,
    pub limit: Option<u32>,
}

/// One row of `GET /api/skills` (hq-fe-skills.2). Flat projection of
/// [`gt_skills::Skill`] — the dashboard renders the catalog as a labelled list with
/// per-skill scope chips, so we surface `default_scopes` (the canonical scope set the
/// skill grants) but not the `registered_at_secs` timestamp the actor stores. Ordering
/// follows the actor snapshot: the catalog is a `BTreeMap` keyed by id, so the response
/// is sorted alphabetically and stable across reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDto {
    pub id: String,
    pub label: String,
    pub description: String,
    pub default_scopes: Vec<String>,
}

impl From<gt_skills::Skill> for SkillDto {
    fn from(s: gt_skills::Skill) -> Self {
        Self {
            id: s.id,
            label: s.label,
            description: s.description,
            default_scopes: s.default_scopes,
        }
    }
}

/// One row of `GET /api/roles` (hq-fe-skills.2). Per-role enabled-skill list, flattened
/// to the id set the dashboard's `RoleList` + `SkillToggle` panels render against.
/// `skills` is alphabetically sorted (the actor stores it in a `BTreeSet`) so the wire
/// shape is deterministic across replay — same posture as [`SkillDto`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleSkillsDto {
    pub role: String,
    pub skills: Vec<String>,
}

impl From<gt_skills::RoleBinding> for RoleSkillsDto {
    fn from(b: gt_skills::RoleBinding) -> Self {
        Self {
            role: b.role,
            skills: b.enabled_skills.into_iter().collect(),
        }
    }
}

/// Canonical wire payloads for `quota.login_*` SSE kinds (hq-fe-auth.3).
///
/// Each struct is the **payload** carried inside the `EventRecord.payload` field;
/// the kind ("quota.login_started" etc.) lives in `EventRecord.type`. Payloads do
/// not duplicate the kind discriminator — the SSE frame is already tagged.
///
/// All payloads carry `account` + `flow_id` so the frontend can demux concurrent
/// flows from a single `/api/stream` connection.

/// `quota.login_started` — driver booted, PTY child is alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLoginStarted {
    pub account: String,
    pub flow_id: String,
}

/// `quota.login_url_ready` — CLI surfaced the OAuth URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLoginUrlReady {
    pub account: String,
    pub flow_id: String,
    pub url: String,
}

/// `quota.login_complete` — CLI exited 0 after the operator-submitted token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLoginComplete {
    pub account: String,
    pub flow_id: String,
}

/// `quota.login_failed` — terminal failure. `reason` is the typed [`LoginFailure`];
/// `message` is its `Display` fallback for clients that don't yet type the union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLoginFailed {
    pub account: String,
    pub flow_id: String,
    pub reason: LoginFailure,
    pub message: String,
}

/// Discriminated envelope used by [`crate::login::emit_event`] to stamp the right
/// kind on the wire. Not a wire shape itself — only its `EventKind` impl and inner
/// payloads cross the SSE boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaLoginEvent {
    Started(QuotaLoginStarted),
    UrlReady(QuotaLoginUrlReady),
    Complete(QuotaLoginComplete),
    Failed(QuotaLoginFailed),
}

impl QuotaLoginEvent {
    /// Stable wire kind — identical to what the SSE consumer sees as `EventRecord.type`.
    pub fn kind_str(&self) -> &'static str {
        match self {
            QuotaLoginEvent::Started(_) => "quota.login_started",
            QuotaLoginEvent::UrlReady(_) => "quota.login_url_ready",
            QuotaLoginEvent::Complete(_) => "quota.login_complete",
            QuotaLoginEvent::Failed(_) => "quota.login_failed",
        }
    }

    /// Encode the inner payload as a generic `serde_json::Value` — the shape
    /// `EventRecord.payload` carries.
    pub fn payload_json(&self) -> serde_json::Value {
        match self {
            QuotaLoginEvent::Started(p) => {
                serde_json::to_value(p).unwrap_or(serde_json::Value::Null)
            }
            QuotaLoginEvent::UrlReady(p) => {
                serde_json::to_value(p).unwrap_or(serde_json::Value::Null)
            }
            QuotaLoginEvent::Complete(p) => {
                serde_json::to_value(p).unwrap_or(serde_json::Value::Null)
            }
            QuotaLoginEvent::Failed(p) => {
                serde_json::to_value(p).unwrap_or(serde_json::Value::Null)
            }
        }
    }
}

impl EventKind for QuotaLoginEvent {
    fn kind(&self) -> &'static str {
        self.kind_str()
    }
}

#[cfg(test)]
mod quota_login_dto_tests {
    use super::*;

    #[test]
    fn kind_strings_match_bead_spec() {
        assert_eq!(
            QuotaLoginEvent::Started(QuotaLoginStarted {
                account: "a".into(),
                flow_id: "F".into(),
            })
            .kind_str(),
            "quota.login_started",
        );
        assert_eq!(
            QuotaLoginEvent::UrlReady(QuotaLoginUrlReady {
                account: "a".into(),
                flow_id: "F".into(),
                url: "https://console.anthropic.com/x".into(),
            })
            .kind_str(),
            "quota.login_url_ready",
        );
        assert_eq!(
            QuotaLoginEvent::Complete(QuotaLoginComplete {
                account: "a".into(),
                flow_id: "F".into(),
            })
            .kind_str(),
            "quota.login_complete",
        );
        assert_eq!(
            QuotaLoginEvent::Failed(QuotaLoginFailed {
                account: "a".into(),
                flow_id: "F".into(),
                reason: LoginFailure::Cancelled,
                message: "login cancelled by caller".into(),
            })
            .kind_str(),
            "quota.login_failed",
        );
    }

    #[test]
    fn url_ready_payload_carries_url() {
        let p = QuotaLoginEvent::UrlReady(QuotaLoginUrlReady {
            account: "a".into(),
            flow_id: "F".into(),
            url: "https://console.anthropic.com/x".into(),
        });
        let v = p.payload_json();
        assert_eq!(v["url"], "https://console.anthropic.com/x");
        assert_eq!(v["account"], "a");
        assert_eq!(v["flow_id"], "F");
        // Payload must NOT contain a redundant `kind` field — that lives on the
        // wire `EventRecord.type`.
        assert!(v.get("kind").is_none());
    }

    #[test]
    fn failed_payload_carries_typed_reason_and_flat_message() {
        let p = QuotaLoginEvent::Failed(QuotaLoginFailed {
            account: "a".into(),
            flow_id: "F".into(),
            reason: LoginFailure::TokenRejected { status: 17 },
            message: "cli rejected token (exit status 17)".into(),
        });
        let v = p.payload_json();
        assert_eq!(v["reason"]["kind"], "token_rejected");
        assert_eq!(v["reason"]["status"], 17);
        assert_eq!(v["message"], "cli rejected token (exit status 17)");
    }
}
