use mysql_async::prelude::*;
use mysql_async::{params, Pool};

use gt_agent::{Session, SessionQueries, SessionState};
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
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS sessions (
                id    VARCHAR(64)  PRIMARY KEY,
                rig   VARCHAR(128) NOT NULL,
                state VARCHAR(32)  NOT NULL
            )",
        )
        .await
        .map_err(map_err)
    }

    pub async fn truncate(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.query_drop("DELETE FROM sessions")
            .await
            .map_err(map_err)
    }

    /// Idempotent write — kept here for tests and for any future mirror path from
    /// the Rust actor. Production writes today come from `gt sling` (Go side).
    pub async fn upsert(&self, session: &Session) -> Result<(), AppError> {
        let mut conn = self.pool.get_conn().await.map_err(map_err)?;
        conn.exec_drop(
            "REPLACE INTO sessions (id, rig, state)
             VALUES (:id, :rig, :state)",
            params! {
                "id" => &session.id,
                "rig" => &session.rig,
                "state" => state_as_str(session.state),
            },
        )
        .await
        .map_err(map_err)
    }
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

fn row_to_session((id, rig, state): (String, String, String)) -> Session {
    Session {
        id,
        rig,
        state: parse_state(&state),
    }
}

impl SessionQueries for DoltSessions {
    fn active_sessions(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<Session>, AppError>> + Send {
        let pool = self.pool.clone();
        async move {
            let mut conn = pool.get_conn().await.map_err(map_err)?;
            let rows: Vec<(String, String, String)> = conn
                .exec(
                    "SELECT id, rig, state FROM sessions
                     WHERE state NOT IN ('done', 'killed')",
                    (),
                )
                .await
                .map_err(map_err)?;
            Ok(rows.into_iter().map(row_to_session).collect())
        }
    }
}
