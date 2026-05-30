use mysql_async::prelude::*;
use mysql_async::Pool;

use gt_events::AppError;
use serde::{Deserialize, Serialize};

use crate::conn::map_err;

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

    /// Confirm the `issues` table exists. This crate never creates it — the
    /// schema is owned by `bd` and pre-existing in hq; this is just a probe so
    /// the gt-mcp boot fails loud against an empty DB.
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
        conn.exec_drop(
            "INSERT INTO issues
                (id, title, description, design, acceptance_criteria, notes,
                 status, priority, issue_type, assignee, owner, created_by, external_ref)
             VALUES
                (:id, :title, :description, :design, :acceptance_criteria, :notes,
                 'open', :priority, :issue_type, :assignee, :owner, :created_by, :external_ref)",
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
                    external_ref, spec_id
             FROM issues
             {where_clause}
             ORDER BY updated_at DESC, id ASC
             LIMIT {limit}"
        );

        type RowTuple = (
            String,
            String,
            String,
            i32,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );

        let params = if params_vec.is_empty() {
            mysql_async::Params::Empty
        } else {
            mysql_async::Params::from(params_vec)
        };

        let rows: Vec<RowTuple> = conn.exec(sql, params).await.map_err(map_err)?;

        Ok(rows.into_iter().map(row_to_issue).collect())
    }
}

fn row_to_issue(
    (
        id,
        title,
        status,
        priority,
        issue_type,
        assignee,
        owner,
        created_at,
        updated_at,
        closed_at,
        external_ref,
        spec_id,
    ): (
        String,
        String,
        String,
        i32,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
) -> IssueRow {
    IssueRow {
        id,
        title,
        status,
        priority,
        issue_type,
        assignee,
        owner,
        created_at,
        updated_at,
        closed_at,
        external_ref,
        spec_id,
    }
}
