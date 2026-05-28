//! Gate for Paso 6.h epic A (hq-u955): the same `SessionQueries` contract runs against
//! both impls of the port:
//!   1. `InMemorySessions` — always (host, no DB).
//!   2. `DoltSessions` — against a real Dolt server when `GT_DOLT_URL` is set (validated
//!      inside the container). If one passes and the other does not, the port is wrong.

use gt_agent::{DogKind, InMemorySessions, Session, SessionQueries, SessionRole, SessionState};
use gt_store_dolt::DoltSessions;

fn fixture() -> Vec<Session> {
    vec![
        Session {
            id: "p1".into(),
            rig: "granite".into(),
            state: SessionState::Spawned,
            role: SessionRole::Dog(DogKind::Witness),
            crew: None,
        },
        Session {
            id: "p2".into(),
            rig: "granite".into(),
            state: SessionState::Working,
            role: SessionRole::Polecat,
            crew: Some("atom".into()),
        },
        Session {
            id: "p3".into(),
            rig: "basalt".into(),
            state: SessionState::Done,
            role: SessionRole::Polecat,
            crew: None,
        },
        Session {
            id: "p4".into(),
            rig: "basalt".into(),
            state: SessionState::Killed,
            role: SessionRole::Mayor,
            crew: None,
        },
    ]
}

fn assert_active(active: Vec<Session>) {
    let mut active = active;
    active.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(active.len(), 2, "Done/Killed must be filtered out");
    assert_eq!(active[0].id, "p1");
    assert_eq!(active[0].state, SessionState::Spawned);
    assert_eq!(active[0].role, SessionRole::Dog(DogKind::Witness), "role round-trips");
    assert_eq!(active[1].id, "p2");
    assert_eq!(active[1].state, SessionState::Working);
    assert_eq!(active[1].role, SessionRole::Polecat);
    assert_eq!(active[1].crew.as_deref(), Some("atom"), "crew round-trips");
}

#[tokio::test]
async fn contract_in_memory() {
    let repo = InMemorySessions::new(fixture());
    assert_active(repo.active_sessions().await.unwrap());
}

#[tokio::test]
async fn contract_dolt() {
    let Ok(base) = std::env::var("GT_DOLT_URL") else {
        eprintln!("GT_DOLT_URL unset — skipping Dolt SessionQueries contract (run inside container)");
        return;
    };
    let base = base.trim_end_matches('/').to_string();

    // Bootstrap the test database via the MySQL-wire client (no dolt CLI needed).
    {
        use mysql_async::prelude::Queryable;
        let pool = gt_store_dolt::connect(&base).expect("connect to server");
        let mut conn = pool.get_conn().await.expect("admin conn");
        conn.query_drop("CREATE DATABASE IF NOT EXISTS gt_rs_test")
            .await
            .expect("create gt_rs_test");
    }

    let repo = DoltSessions::connect(&format!("{base}/gt_rs_test"))
        .expect("connect to gt_rs_test");
    repo.ensure_schema().await.expect("create schema");
    repo.truncate().await.expect("clean table");
    for s in fixture() {
        repo.upsert(&s).await.expect("upsert");
    }

    assert_active(repo.active_sessions().await.unwrap());
}
