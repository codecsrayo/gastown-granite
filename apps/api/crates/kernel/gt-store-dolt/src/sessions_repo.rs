use mysql_async::prelude::*;
use mysql_async::{params, Pool};

use gt_agent::{Session, SessionQueries, SessionRole, SessionState, SessionWriter};
use gt_events::AppError;

use crate::conn::map_err;

/// Dolt-backed adapter for the `SessionQueries` port (Paso 6.h epic A, hq-u955).
///
/// Reads the canonical `sessions` table — the same one polecats write to via
/// `gt sling`. Per `docs/04-persistence.md`, beads/sessions live in Dolt; the
/// read-side exposes them, never the in-memory actor snapshot.
pub struct DoltSessions {
    pool: Pool,
}

impl DoltSessions {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn connect(url: &str) -> Result<Self, AppError> {
        Ok(Self::new(crate::conn::connect(url)?))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        // Fresh DBs (prod has no `sessions` table yet) get role/crew straight from CREATE.
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS sessions (
                id    VARCHAR(64)  PRIMARY KEY,
                rig   VARCHAR(128) NOT NULL,
                state VARCHAR(32)  NOT NULL,
                role  VARCHAR(32)  NOT NULL DEFAULT 'polecat',
                crew  VARCHAR(128) NULL
            )",
        )
        .await
        .map_err(map_err)?;
        // Migrate a pre-8.7 table. Dolt has no `ADD COLUMN IF NOT EXISTS` (MariaDB-only), so
        // guard each ALTER with an information_schema existence check — idempotent across boots.
        ensure_column(&mut conn, "role", "VARCHAR(32) NOT NULL DEFAULT 'polecat'").await?;
        ensure_column(&mut conn, "crew", "VARCHAR(128) NULL").await?;
        Ok(())
    }

    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM sessions")
            .await
            .map_err(map_err)
    }

    /// Idempotent full-row write (used for `Spawned`, where rig/role/crew are known). The
    /// Rust sessions projector (hq-8iur.2) drives this from `AgentEvent::Spawned`.
    pub async fn upsert(&self, session: &Session) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "REPLACE INTO sessions (id, rig, state, role, crew)
             VALUES (:id, :rig, :state, :role, :crew)",
            params! {
                "id" => &session.id,
                "rig" => &session.rig,
                "state" => state_as_str(session.state),
                "role" => session.role.as_str(),
                "crew" => session.crew.clone(),
            },
        )
        .await
        .map_err(map_err)
    }

    /// Idempotent state-only update by id (used for `SessionEnd`/`Killed`, where only the
    /// lifecycle state changes and the rig/role/crew were set at spawn). No-op if the row
    /// is absent — the projector tolerates a terminal event for an unseen session.
    pub async fn set_state(&self, id: &str, state: SessionState) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "UPDATE sessions SET state = :state WHERE id = :id",
            params! {
                "id" => id,
                "state" => state_as_str(state),
            },
        )
        .await
        .map_err(map_err)
    }
}

/// Add `name`/`ddl` to the `sessions` table only if the column is missing. `name`/`ddl` are
/// crate-internal constants (no injection surface). Works around Dolt lacking
/// `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`.
async fn ensure_column(
    conn: &mut mysql_async::Conn,
    name: &str,
    ddl: &str,
) -> Result<(), AppError> {
    let present: Option<i64> = conn
        .exec_first(
            "SELECT 1 FROM information_schema.columns
             WHERE table_schema = DATABASE() AND table_name = 'sessions' AND column_name = :c
             LIMIT 1",
            params! { "c" => name },
        )
        .await
        .map_err(map_err)?;
    if present.is_none() {
        conn.query_drop(format!("ALTER TABLE sessions ADD COLUMN {name} {ddl}"))
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

fn state_as_str(s: SessionState) -> &'static str {
    match s {
        SessionState::Spawned => "spawned",
        SessionState::Working => "working",
        SessionState::Done => "done",
        SessionState::Killed => "killed",
    }
}

fn parse_state(s: &str) -> SessionState {
    match s {
        "spawned" => SessionState::Spawned,
        "working" => SessionState::Working,
        "done" => SessionState::Done,
        "killed" => SessionState::Killed,
        // Unknown values fall back to Done so they're filtered from `active_sessions`
        // rather than masquerading as live work. A write path that produced this
        // would be a bug, but the read-side must not panic on it.
        _ => SessionState::Done,
    }
}

fn row_to_session((id, rig, state, role, crew): (String, String, String, String, Option<String>)) -> Session {
    Session {
        id,
        rig,
        state: parse_state(&state),
        role: SessionRole::parse(&role).unwrap_or_default(),
        crew,
    }
}

impl SessionWriter for DoltSessions {
    fn upsert(&self, session: &Session) -> impl std::future::Future<Output = Result<(), AppError>> + Send {
        let session = session.clone();
        async move { DoltSessions::upsert(self, &session).await }
    }

    fn set_state(
        &self,
        id: &str,
        state: SessionState,
    ) -> impl std::future::Future<Output = Result<(), AppError>> + Send {
        let id = id.to_string();
        async move { DoltSessions::set_state(self, &id, state).await }
    }
}

impl SessionQueries for DoltSessions {
    fn active_sessions(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<Session>, AppError>> + Send {
        let pool = self.pool.clone();
        async move {
            let mut conn = pool.get_conn().await.map_err(map_err)?;
            let rows: Vec<(String, String, String, String, Option<String>)> = conn
                .exec(
                    "SELECT id, rig, state, role, crew FROM sessions
                     WHERE state NOT IN ('done', 'killed')",
                    (),
                )
                .await
                .map_err(map_err)?;
            Ok(rows.into_iter().map(row_to_session).collect())
        }
    }
}
