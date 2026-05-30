use mysql_async::prelude::*;
use mysql_async::Pool;

use gt_events::AppError;
use serde::{Deserialize, Serialize};

use crate::conn::map_err;

/// Status states the `issues.transition` tool (hq-mcp-issues.4) understands.
/// `bd`'s lifecycle uses additional internal labels (`hooked`, etc.) but those
/// are owned by the polecat actor — the user-facing surface stays open/working/
/// closed for predictable kanban semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueStatus {
    Open,
    Working,
    Closed,
}

impl IssueStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "working" => Some(Self::Working),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Working => "working",
            Self::Closed => "closed",
        }
    }

    /// Legal transitions in the issue state machine. `open ↔ working`, plus
    /// either side may close; `closed` re-opens through `open` but never jumps
    /// straight back to `working` — matches the example the bead description
    /// calls out (`closed -> working` is rejected).
    pub fn can_transition_to(self, target: Self) -> bool {
        use IssueStatus::*;
        matches!(
            (self, target),
            (Open, Working) | (Open, Closed) | (Working, Open) | (Working, Closed) | (Closed, Open)
        )
    }
}

/// Filters applied when listing issues for the `gt://issues` MCP resource
/// (hq-mcp-issues.1). All fields are optional and combined with `AND`; `None`
/// means "no filter on this column". `limit` caps the result set so a noisy
/// query can't dump the whole table over the MCP wire.
#[derive(Debug, Default, Clone)]
pub struct IssueFilter {
    /// Match `status` exactly against any of the values (typically
    /// `open`/`working`/`closed`). Empty vec = no filter.
    pub status: Vec<String>,
    /// Match `priority <= priority_max` (0 = highest priority).
    pub priority_max: Option<u8>,
    /// Match `assignee` exactly. `""` (empty string) matches the canonical
    /// "unassigned" value the schema stores as `''`.
    pub assignee: Option<String>,
    /// Match `external_ref` exactly (used for epic linkage by `hq-fe-*`).
    pub external_ref: Option<String>,
    /// Match `issue_type` exactly (`epic`, `task`, `spike`, ...).
    pub issue_type: Option<String>,
    /// Row cap. Defaults to 200 in [`DoltIssues::list`] when `None`.
    pub limit: Option<u32>,
}

/// Snapshot row returned by [`DoltIssues::list`]. Mirrors the columns dashboards
/// and `bd list` consume; the heavy text bodies (`description`/`design`/
/// `acceptance_criteria`/`notes`) live on the per-issue `issues.get` tool added
/// by the rest of the epic so listings stay cheap.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IssueRow {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub external_ref: Option<String>,
    pub spec_id: Option<String>,
    /// JSON array of taxonomy domains (hq-taxon.3). Serialised as a raw JSON
    /// string so consumers (`gt-mcp` resources, `bd` mirrors) re-parse without
    /// the store needing to know the closed-set `Domain` enum.
    #[serde(default = "default_json_array")]
    pub domain_json: String,
    /// JSON array of impact surfaces (crate names or repo paths).
    #[serde(default = "default_json_array")]
    pub surface_json: String,
    /// JSON array of bead ids this bead is blocked on (forward edges).
    #[serde(default = "default_json_array")]
    pub depends_on_json: String,
    /// Optional `role_scope` discriminator (e.g. `sheriff`); `None` when no
    /// role owns the bead. Stored as `VARCHAR(32)` so `bd` legacy callers can
    /// keep filtering with plain string equality.
    pub role_scope: Option<String>,
}

fn default_json_array() -> String {
    "[]".to_string()
}

/// Patch payload for [`DoltIssues::update`] (hq-mcp-issues.3). Every field is
/// `Option<T>`: `None` leaves the column untouched, `Some(_)` overwrites it.
/// Status changes belong to [`DoltIssues::transition`] (hq-mcp-issues.4) so they
/// are deliberately absent here — keeping write paths separable for audit /
/// scope grants ("read + edit-fields" vs "transition").
#[derive(Debug, Default, Clone)]
pub struct IssuePatch {
    /// New title. `Some("")` is rejected upstream — schema is `NOT NULL`.
    pub title: Option<String>,
    /// New description. Empty allowed.
    pub description: Option<String>,
    /// New design notes. Empty allowed.
    pub design: Option<String>,
    /// New acceptance criteria. Empty allowed.
    pub acceptance_criteria: Option<String>,
    /// New free-form notes. Empty allowed.
    pub notes: Option<String>,
    /// New priority `0..=2` (0 = P0).
    pub priority: Option<u8>,
    /// New `issue_type` (`epic`/`task`/`spike`/...).
    pub issue_type: Option<String>,
    /// New assignee. `Some(String::new())` stores `''` (canonical "unassigned"
    /// for the column when nullable-with-default is in play; schema accepts
    /// either form). `None` leaves the column alone.
    pub assignee: Option<String>,
    /// New owner. Same nullability shape as `assignee`.
    pub owner: Option<String>,
    /// New epic linkage. `Some(String::new())` clears to empty string (schema
    /// is nullable, so the frontier can map to `NULL` if desired).
    pub external_ref: Option<String>,
}

impl IssuePatch {
    /// True when no field is set — the caller has nothing to patch, which the
    /// frontier should treat as `Validation` rather than emitting a no-op
    /// `UPDATE issues SET WHERE id = ?` (parses as a syntax error).
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.design.is_none()
            && self.acceptance_criteria.is_none()
            && self.notes.is_none()
            && self.priority.is_none()
            && self.issue_type.is_none()
            && self.assignee.is_none()
            && self.owner.is_none()
            && self.external_ref.is_none()
    }
}

/// Insert payload for [`DoltIssues::insert`] (hq-mcp-issues.2). Mirrors the
/// required columns of `hq.issues`; the optional fields fall back to schema
/// defaults so callers only have to supply what the bead's design lists as
/// required (`id`, `title`, `priority`, `issue_type`, `created_by`).
#[derive(Debug, Clone, Default)]
pub struct NewIssue {
    /// Stable bead id. Must be unique; non-empty.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Free-text body. Empty string is allowed and stored verbatim — the
    /// schema marks the column `NOT NULL` so `None` here defaults to `""`.
    pub description: String,
    /// Design notes. `NOT NULL` in schema; empty allowed.
    pub design: String,
    /// Acceptance criteria. `NOT NULL` in schema; empty allowed.
    pub acceptance_criteria: String,
    /// Free-form notes. `NOT NULL` in schema; empty allowed.
    pub notes: String,
    /// Priority `0..=2` (0 = P0). Schema default is `2`.
    pub priority: u8,
    /// `epic`/`task`/`spike`/... — domain string.
    pub issue_type: String,
    /// Bead creator. Maps to `created_by`.
    pub created_by: String,
    /// Optional epic linkage. `None` stores `NULL`.
    pub external_ref: Option<String>,
    /// Optional assignee. `None` stores `NULL`.
    pub assignee: Option<String>,
    /// Optional initial owner. `None` stores schema default `''`.
    pub owner: Option<String>,
    /// Raw JSON array of `Domain` discriminators (e.g. `["orch.merge"]`).
    /// Empty string is normalised to `[]` by [`DoltIssues::insert`] so the
    /// schema's `NOT NULL` constraint is honoured even with a default-built
    /// `NewIssue` (hq-taxon.3).
    pub domain_json: String,
    /// Raw JSON array of impact surfaces (free-form strings).
    pub surface_json: String,
    /// Raw JSON array of bead ids this bead is blocked on.
    pub depends_on_json: String,
    /// Optional `role_scope` discriminator. `None` stores `NULL`.
    pub role_scope: Option<String>,
}

/// Read-only Dolt adapter for the `issues` table. The canonical bead table is
/// `issues` (~25 cols), distinct from `beads` (5 cols, dispatcher-facing). The
/// MCP `gt://issues` resource (hq-mcp-issues.1) snapshots it; the write-side
/// tools (`.2`-`.5`) layer on top once `hq-fe-api-w.1` lands the command-bus.
pub struct DoltIssues {
    pool: Pool,
}

impl DoltIssues {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url)?))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Confirm the `issues` table exists and adds the taxonomy columns the
    /// hq-taxon family layered on top (`domain_json`, `surface_json`,
    /// `depends_on_json`, `role_scope`). The table itself is owned by `bd` and
    /// pre-existing in hq; the column adds are idempotent — second runs are
    /// no-ops once `information_schema.columns` already lists them.
    ///
    /// Adds default `'[]'` for JSON arrays so existing rows backfill without a
    /// follow-up `UPDATE`; the actual `Domain` typing lives in `gt-mcp` and is
    /// re-validated on the write path.
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let present: Option<i64> = conn
            .query_first(
                "SELECT 1 FROM information_schema.tables
                 WHERE table_schema = DATABASE() AND table_name = 'issues' LIMIT 1",
            )
            .await
            .map_err(map_err)?;
        if present.is_none() {
            return Err(AppError::Other(
                "issues table missing in current Dolt database".into(),
            ));
        }

        let taxonomy_columns: &[(&str, &str)] = &[
            ("domain_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("surface_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("depends_on_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("role_scope", "VARCHAR(32) NULL"),
        ];

        let mut added_any = false;
        for (name, ddl) in taxonomy_columns {
            let exists: Option<i64> = conn
                .exec_first(
                    "SELECT 1 FROM information_schema.columns
                     WHERE table_schema = DATABASE()
                       AND table_name = 'issues'
                       AND column_name = :col LIMIT 1",
                    mysql_async::params! { "col" => *name },
                )
                .await
                .map_err(map_err)?;
            if exists.is_none() {
                // Column-name is not a bind parameter — only the closed-set
                // string literals from `taxonomy_columns` ever reach `format!`,
                // so there's no caller-controlled SQL here.
                let sql = format!("ALTER TABLE issues ADD COLUMN {name} {ddl}");
                conn.query_drop(sql).await.map_err(map_err)?;
                added_any = true;
            }
        }

        if added_any {
            conn.exec_drop(
                "CALL DOLT_COMMIT('-A', '-m', :msg)",
                mysql_async::params! {
                    "msg" => "hq-taxon.3: add taxonomy columns to issues".to_string(),
                },
            )
            .await
            .map_err(map_err)?;
        }

        // hq-taxon.6 — backfill live root epics so the dependency-graph
        // resources have meaningful anchor points the first time they are
        // queried. Idempotent by construction: the `WHERE` clause only
        // touches rows whose `domain_json` is still the default empty array
        // (or the legacy empty-string case Dolt occasionally hands us).
        //
        // Coverage chosen per `apps/api/docs/14-bead-taxonomy.md` §8 — the
        // well-known root epics that already exist plus the hq-taxon family
        // itself (minted via the legacy tool before `domain[]` was a field).
        // Ordinary tasks continue to backfill on their next `issues.update`.
        let backfill: &[(&str, &str)] = &[
            ("hq-fe-svelte", r#"["fe.web","fe.docs"]"#),
            ("hq-fe-api-w", r#"["kernel.root","bin.gt-web"]"#),
            ("hq-fe-api-r", r#"["bin.gt-web"]"#),
            ("hq-fe-cut", r#"["fe.web","bin.gt-web"]"#),
            ("hq-fe-build", r#"["fe.web"]"#),
            ("hq-fe-view", r#"["fe.web"]"#),
            ("hq-fe-auth", r#"["fe.web","bin.gt-web"]"#),
            ("hq-fe-rbac", r#"["fe.web","bin.gt-web"]"#),
            (
                "hq-fe-skills",
                r#"["fe.web","role.sheriff","role.deacon","role.refinery","role.witness","role.mayor"]"#,
            ),
            ("hq-fe-term", r#"["fe.web","bin.gt-web"]"#),
            ("hq-mcp-issues", r#"["bin.gt-mcp","store.dolt"]"#),
            ("hq-oap5", r#"["deploy.compose","lifecycle.polecat"]"#),
            ("hq-63az", r#"["lifecycle.polecat"]"#),
            ("hq-03aw", r#"["store.dolt","store.pg"]"#),
            ("hq-mc72", r#"["bin.gt"]"#),
            ("hq-taxon", r#"["docs.spec","bin.gt-mcp","store.dolt"]"#),
            ("hq-taxon.1", r#"["bin.gt-mcp"]"#),
            ("hq-taxon.2", r#"["bin.gt-mcp"]"#),
            ("hq-taxon.3", r#"["store.dolt"]"#),
            ("hq-taxon.4", r#"["bin.gt-mcp"]"#),
            ("hq-taxon.5", r#"["bin.gt-mcp"]"#),
            ("hq-taxon.6", r#"["store.dolt"]"#),
        ];

        let mut backfilled_any = false;
        for (id, domain) in backfill {
            let result = conn
                .exec_iter(
                    "UPDATE issues
                     SET domain_json = :domain
                     WHERE id = :id
                       AND (domain_json = '[]' OR domain_json = '' OR domain_json IS NULL)",
                    mysql_async::params! {
                        "domain" => *domain,
                        "id" => *id,
                    },
                )
                .await
                .map_err(map_err)?;
            let affected = result.affected_rows();
            let _ = result.drop_result().await.map_err(map_err)?;
            if affected > 0 {
                backfilled_any = true;
            }
        }

        if backfilled_any {
            conn.exec_drop(
                "CALL DOLT_COMMIT('-A', '-m', :msg)",
                mysql_async::params! {
                    "msg" => "hq-taxon.6: backfill live root epics with domain[]".to_string(),
                },
            )
            .await
            .map_err(map_err)?;
        }

        Ok(())
    }

    /// Insert a new row into `hq.issues` and stamp it as a Dolt commit so the
    /// write is visible to downstream readers (`bd`, the dashboard, replication)
    /// without waiting for an external commit (hq-mcp-issues.2).
    ///
    /// Atomicity: the `INSERT` and the `CALL DOLT_COMMIT` run on the same
    /// connection; a failure on the `INSERT` aborts before any commit. The
    /// `DOLT_COMMIT('-A', '-m', ...)` includes every uncommitted change on the
    /// working set — mirroring the `docker exec dolt sql -q "...; CALL
    /// DOLT_COMMIT(...)"` recipe operators ran by hand pre-MCP.
    ///
    /// Returns the duplicate-key error path verbatim so the frontier can
    /// translate it to a `Validation` outcome (the caller already validated
    /// non-empty fields; only DB-level uniqueness can race here).
    pub async fn insert(&self, row: &NewIssue) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        // Normalise default-built `NewIssue` (Default derive leaves the JSON
        // strings as `""`) so the NOT NULL columns honour their `[]` invariant.
        let domain_json = if row.domain_json.is_empty() { "[]" } else { row.domain_json.as_str() };
        let surface_json = if row.surface_json.is_empty() { "[]" } else { row.surface_json.as_str() };
        let depends_on_json = if row.depends_on_json.is_empty() { "[]" } else { row.depends_on_json.as_str() };
        conn.exec_drop(
            "INSERT INTO issues
                (id, title, description, design, acceptance_criteria, notes,
                 status, priority, issue_type, assignee, owner, created_by, external_ref,
                 domain_json, surface_json, depends_on_json, role_scope)
             VALUES
                (:id, :title, :description, :design, :acceptance_criteria, :notes,
                 'open', :priority, :issue_type, :assignee, :owner, :created_by, :external_ref,
                 :domain_json, :surface_json, :depends_on_json, :role_scope)",
            mysql_async::params! {
                "id" => &row.id,
                "title" => &row.title,
                "description" => &row.description,
                "design" => &row.design,
                "acceptance_criteria" => &row.acceptance_criteria,
                "notes" => &row.notes,
                "priority" => row.priority as i32,
                "issue_type" => &row.issue_type,
                "assignee" => row.assignee.clone(),
                "owner" => row.owner.clone().unwrap_or_default(),
                "created_by" => &row.created_by,
                "external_ref" => row.external_ref.clone(),
                "domain_json" => domain_json,
                "surface_json" => surface_json,
                "depends_on_json" => depends_on_json,
                "role_scope" => row.role_scope.clone(),
            },
        )
        .await
        .map_err(map_err)?;

        // Atomic Dolt commit so the row lands in history immediately. Message
        // mirrors the operator's pre-MCP recipe (`docker exec dolt sql -q
        // "INSERT ...; CALL DOLT_COMMIT('-A','-m','create <id>')"`). Failure
        // here is fatal — the INSERT already landed in the working set and
        // would be picked up by the next commit silently.
        let commit_msg = format!("create {}", row.id);
        conn.exec_drop(
            "CALL DOLT_COMMIT('-A', '-m', :msg)",
            mysql_async::params! {
                "msg" => commit_msg,
            },
        )
        .await
        .map_err(map_err)?;

        Ok(())
    }

    /// Apply a partial patch to an existing row in `hq.issues` and stamp the
    /// change as a Dolt commit (hq-mcp-issues.3). Returns `AppError::NotFound`
    /// when no row matches `id` so the frontier can translate to a clean MCP
    /// `not found`.
    ///
    /// `updated_at = NOW()` is always set so dashboards reorder the row.
    /// `IssuePatch::is_empty` is the caller's responsibility — passing an empty
    /// patch here produces an `UPDATE ... SET updated_at = NOW() WHERE id = :id`,
    /// which is wasted churn; the frontier validates before delegating.
    pub async fn update(&self, id: &str, patch: &IssuePatch) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;

        let mut set_parts: Vec<&str> = Vec::new();
        let mut params_vec: Vec<(String, mysql_async::Value)> =
            vec![("id".to_string(), mysql_async::Value::from(id.to_string()))];

        if let Some(v) = &patch.title {
            set_parts.push("title = :title");
            params_vec.push(("title".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = &patch.description {
            set_parts.push("description = :description");
            params_vec.push(("description".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = &patch.design {
            set_parts.push("design = :design");
            params_vec.push(("design".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = &patch.acceptance_criteria {
            set_parts.push("acceptance_criteria = :acceptance_criteria");
            params_vec.push((
                "acceptance_criteria".to_string(),
                mysql_async::Value::from(v.clone()),
            ));
        }
        if let Some(v) = &patch.notes {
            set_parts.push("notes = :notes");
            params_vec.push(("notes".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = patch.priority {
            set_parts.push("priority = :priority");
            params_vec.push(("priority".to_string(), mysql_async::Value::from(v as i32)));
        }
        if let Some(v) = &patch.issue_type {
            set_parts.push("issue_type = :issue_type");
            params_vec.push(("issue_type".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = &patch.assignee {
            set_parts.push("assignee = :assignee");
            params_vec.push(("assignee".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = &patch.owner {
            set_parts.push("owner = :owner");
            params_vec.push(("owner".to_string(), mysql_async::Value::from(v.clone())));
        }
        if let Some(v) = &patch.external_ref {
            set_parts.push("external_ref = :external_ref");
            params_vec.push((
                "external_ref".to_string(),
                mysql_async::Value::from(v.clone()),
            ));
        }

        set_parts.push("updated_at = NOW()");
        let sql = format!(
            "UPDATE issues SET {} WHERE id = :id",
            set_parts.join(", "),
        );

        let result = conn
            .exec_iter(sql, mysql_async::Params::from(params_vec))
            .await
            .map_err(map_err)?;
        let affected = result.affected_rows();
        // Drain the result-set handle before issuing the commit on the same conn.
        let _ = result.drop_result().await.map_err(map_err)?;

        if affected == 0 {
            return Err(AppError::NotFound(format!("issue {id}")));
        }

        let commit_msg = format!("update {id}");
        conn.exec_drop(
            "CALL DOLT_COMMIT('-A', '-m', :msg)",
            mysql_async::params! {
                "msg" => commit_msg,
            },
        )
        .await
        .map_err(map_err)?;

        Ok(())
    }

    /// Read the current status of `id`. `None` when the row does not exist.
    /// Used by [`Self::transition`] to distinguish `NotFound` from
    /// `InvalidTransition` after a status-guarded UPDATE fails to match.
    pub async fn current_status(&self, id: &str) -> Result<Option<String>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        let row: Option<String> = conn
            .exec_first(
                "SELECT status FROM issues WHERE id = :id LIMIT 1",
                mysql_async::params! { "id" => id },
            )
            .await
            .map_err(map_err)?;
        Ok(row)
    }

    /// Move an issue across the [`IssueStatus`] state machine (hq-mcp-issues.4).
    /// Uses a status-guarded `UPDATE` so a concurrent transition cannot land an
    /// illegal jump under us — the `affected_rows == 0` path then falls back to
    /// a `current_status` read to tell `NotFound` from `InvalidTransition`.
    /// Atomic Dolt commit on success.
    pub async fn transition(
        &self,
        id: &str,
        target: IssueStatus,
    ) -> Result<(), AppError> {
        let legal_sources: Vec<&'static str> = [
            IssueStatus::Open,
            IssueStatus::Working,
            IssueStatus::Closed,
        ]
        .into_iter()
        .filter(|s| s.can_transition_to(target))
        .map(|s| s.as_str())
        .collect();

        let mut conn = self.pool.get_conn().await.map_err(map_err)?;

        let placeholders: Vec<String> = legal_sources
            .iter()
            .enumerate()
            .map(|(i, _)| format!(":src_{i}"))
            .collect();
        let mut params_vec: Vec<(String, mysql_async::Value)> = vec![
            ("id".to_string(), mysql_async::Value::from(id.to_string())),
            (
                "target".to_string(),
                mysql_async::Value::from(target.as_str().to_string()),
            ),
        ];
        for (i, s) in legal_sources.iter().enumerate() {
            params_vec.push((format!("src_{i}"), mysql_async::Value::from(s.to_string())));
        }

        let closed_at_set = match target {
            IssueStatus::Closed => "closed_at = NOW(),",
            IssueStatus::Open => "closed_at = NULL,",
            IssueStatus::Working => "",
        };

        let where_status = if placeholders.is_empty() {
            // No legal source -> impossible to satisfy. Skip the UPDATE.
            String::from("1 = 0")
        } else {
            format!("status IN ({})", placeholders.join(", "))
        };

        let sql = format!(
            "UPDATE issues
             SET status = :target,
                 {closed_at_set}
                 updated_at = NOW()
             WHERE id = :id AND {where_status}"
        );

        let result = conn
            .exec_iter(sql, mysql_async::Params::from(params_vec))
            .await
            .map_err(map_err)?;
        let affected = result.affected_rows();
        let _ = result.drop_result().await.map_err(map_err)?;

        if affected == 0 {
            // Disambiguate NotFound vs InvalidTransition for the frontier.
            return match self.current_status(id).await? {
                None => Err(AppError::NotFound(format!("issue {id}"))),
                Some(current) => Err(AppError::Validation(format!(
                    "invalid transition: {current} -> {}",
                    target.as_str()
                ))),
            };
        }

        let commit_msg = format!("transition {id} -> {}", target.as_str());
        conn.exec_drop(
            "CALL DOLT_COMMIT('-A', '-m', :msg)",
            mysql_async::params! {
                "msg" => commit_msg,
            },
        )
        .await
        .map_err(map_err)?;

        Ok(())
    }

    /// Close an issue with attribution (hq-mcp-issues.5). Sets `status='closed'`,
    /// `closed_at=NOW()`, `closed_by_session=:session`, `updated_at=NOW()` in a
    /// single status-guarded UPDATE so only `open`/`working` rows actually
    /// close — a row already `closed` rejects as `InvalidTransition` rather
    /// than silently bumping the timestamp.
    ///
    /// Differs from `transition(id, IssueStatus::Closed)`: that path leaves
    /// `closed_by_session` untouched. The dedicated `close` tool exists so the
    /// attribution column gets populated atomically with the lifecycle move.
    pub async fn close(&self, id: &str, closed_by_session: &str) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;

        let result = conn
            .exec_iter(
                "UPDATE issues
                 SET status = 'closed',
                     closed_at = NOW(),
                     closed_by_session = :session,
                     updated_at = NOW()
                 WHERE id = :id AND status IN ('open', 'working')",
                mysql_async::params! {
                    "id" => id,
                    "session" => closed_by_session,
                },
            )
            .await
            .map_err(map_err)?;
        let affected = result.affected_rows();
        let _ = result.drop_result().await.map_err(map_err)?;

        if affected == 0 {
            // Distinguish missing row from already-closed.
            return match self.current_status(id).await? {
                None => Err(AppError::NotFound(format!("issue {id}"))),
                Some(current) => Err(AppError::Validation(format!(
                    "invalid transition: {current} -> closed"
                ))),
            };
        }

        let commit_msg = format!("close {id} by {closed_by_session}");
        conn.exec_drop(
            "CALL DOLT_COMMIT('-A', '-m', :msg)",
            mysql_async::params! {
                "msg" => commit_msg,
            },
        )
        .await
        .map_err(map_err)?;

        Ok(())
    }

    /// List issues matching `filter`, newest-updated first. Datetime columns
    /// are formatted server-side to ISO 8601 strings — the workspace pins
    /// `mysql_async` with `minimal` features (no `time`/`chrono` integration),
    /// so converting in SQL keeps the rust deserialization to plain `String`.
    pub async fn list(&self, filter: &IssueFilter) -> Result<Vec<IssueRow>, AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;

        let mut where_parts: Vec<String> = Vec::new();
        let mut params_vec: Vec<(String, mysql_async::Value)> = Vec::new();

        if !filter.status.is_empty() {
            let placeholders: Vec<String> = filter
                .status
                .iter()
                .enumerate()
                .map(|(i, _)| format!(":status_{i}"))
                .collect();
            where_parts.push(format!("status IN ({})", placeholders.join(", ")));
            for (i, s) in filter.status.iter().enumerate() {
                params_vec.push((format!("status_{i}"), mysql_async::Value::from(s.clone())));
            }
        }
        if let Some(p) = filter.priority_max {
            where_parts.push("priority <= :priority_max".to_string());
            params_vec.push(("priority_max".to_string(), mysql_async::Value::from(p as i32)));
        }
        if let Some(a) = &filter.assignee {
            where_parts.push("assignee = :assignee".to_string());
            params_vec.push(("assignee".to_string(), mysql_async::Value::from(a.clone())));
        }
        if let Some(r) = &filter.external_ref {
            where_parts.push("external_ref = :external_ref".to_string());
            params_vec.push((
                "external_ref".to_string(),
                mysql_async::Value::from(r.clone()),
            ));
        }
        if let Some(t) = &filter.issue_type {
            where_parts.push("issue_type = :issue_type".to_string());
            params_vec.push(("issue_type".to_string(), mysql_async::Value::from(t.clone())));
        }

        let limit = filter.limit.unwrap_or(200).min(1000);

        let where_clause = if where_parts.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_parts.join(" AND "))
        };

        let sql = format!(
            "SELECT id, title, status, priority, issue_type, assignee, owner,
                    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%SZ') AS created_at,
                    DATE_FORMAT(updated_at, '%Y-%m-%dT%H:%i:%SZ') AS updated_at,
                    DATE_FORMAT(closed_at,  '%Y-%m-%dT%H:%i:%SZ') AS closed_at,
                    external_ref, spec_id,
                    domain_json, surface_json, depends_on_json, role_scope
             FROM issues
             {where_clause}
             ORDER BY updated_at DESC, id ASC
             LIMIT {limit}"
        );

        let params = if params_vec.is_empty() {
            mysql_async::Params::Empty
        } else {
            mysql_async::Params::from(params_vec)
        };

        // 16 columns exceeds mysql_async's `FromRow` tuple impls (12), so we
        // pull each row by ordinal index — keeps the code branchless and the
        // SELECT order is the single source of truth for the field mapping.
        let rows: Vec<mysql_async::Row> = conn.exec(sql, params).await.map_err(map_err)?;

        rows.into_iter().map(row_to_issue).collect()
    }
}

fn row_to_issue(row: mysql_async::Row) -> Result<IssueRow, AppError> {
    let mut row = row;
    let take_string = |row: &mut mysql_async::Row, i: usize| -> Result<String, AppError> {
        row.take::<String, _>(i)
            .ok_or_else(|| AppError::Other(format!("issues row missing column {i}")))
    };
    let take_i32 = |row: &mut mysql_async::Row, i: usize| -> Result<i32, AppError> {
        row.take::<i32, _>(i)
            .ok_or_else(|| AppError::Other(format!("issues row missing column {i}")))
    };
    // `take::<Option<String>, _>` returns `Some(None)` for SQL NULL, so the
    // outer `unwrap_or(None)` collapses both "absent column" and "NULL" into
    // the same `None` — matches the previous tuple path's semantics.
    let take_opt = |row: &mut mysql_async::Row, i: usize| -> Option<String> {
        row.take::<Option<String>, _>(i).unwrap_or(None)
    };

    Ok(IssueRow {
        id: take_string(&mut row, 0)?,
        title: take_string(&mut row, 1)?,
        status: take_string(&mut row, 2)?,
        priority: take_i32(&mut row, 3)?,
        issue_type: take_string(&mut row, 4)?,
        assignee: take_opt(&mut row, 5),
        owner: take_opt(&mut row, 6),
        created_at: take_opt(&mut row, 7),
        updated_at: take_opt(&mut row, 8),
        closed_at: take_opt(&mut row, 9),
        external_ref: take_opt(&mut row, 10),
        spec_id: take_opt(&mut row, 11),
        domain_json: take_string(&mut row, 12)?,
        surface_json: take_string(&mut row, 13)?,
        depends_on_json: take_string(&mut row, 14)?,
        role_scope: take_opt(&mut row, 15),
    })
}
