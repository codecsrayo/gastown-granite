//! Paso 10 C11c gate: the **same** `RigRepository` contract runs against two impls:
//!   1. `InMemoryRigs` — always (host, no DB).
//!   2. `PgRigs` — against a real Postgres if `GT_PG_URL` is set. If one passes and the other
//!      fails, the port is mis-defined (same principle as the quota contract).

use gt_rig::{InMemoryRigs, RigEntry, RigRepository};
use gt_store_pg::PgRigs;

async fn rig_repo_contract<R: RigRepository>(repo: &R) {
    let plane = RigEntry::new("plane", "pl", "git@github.com:o/plane.git", "main", 100);
    let mut gastown = RigEntry::new("gastown", "gt", "git@github.com:o/gastown.git", "main", 200);
    // exercise the nullable remote columns
    gastown.push_url = Some("git@github.com:fork/gastown.git".into());
    gastown.upstream_url = Some("git@github.com:up/gastown.git".into());

    // upsert + get round-trip, including the Option remotes
    repo.upsert(&plane).await.unwrap();
    repo.upsert(&gastown).await.unwrap();
    assert_eq!(repo.get("plane").await.unwrap(), Some(plane.clone()));
    assert_eq!(repo.get("gastown").await.unwrap(), Some(gastown.clone()));
    assert_eq!(repo.get("nope").await.unwrap(), None);

    // prefix ownership (the catalog invariant the UNIQUE index backs)
    assert_eq!(repo.prefix_owner("pl").await.unwrap(), Some("plane".into()));
    assert_eq!(repo.prefix_owner("gt").await.unwrap(), Some("gastown".into()));
    assert_eq!(repo.prefix_owner("xx").await.unwrap(), None);

    // list is sorted by name (gastown < plane)
    assert_eq!(
        repo.list().await.unwrap(),
        vec![gastown.clone(), plane.clone()]
    );

    // upsert is idempotent replace: same name overwrites, no duplicate row
    let mut plane_v2 = plane.clone();
    plane_v2.default_branch = "trunk".into();
    repo.upsert(&plane_v2).await.unwrap();
    assert_eq!(
        repo.get("plane").await.unwrap().unwrap().default_branch,
        "trunk"
    );
    assert_eq!(repo.list().await.unwrap().len(), 2);

    // remove: true once, false after, then absent
    assert!(repo.remove("plane").await.unwrap());
    assert!(!repo.remove("plane").await.unwrap());
    assert_eq!(repo.get("plane").await.unwrap(), None);
    assert_eq!(repo.list().await.unwrap(), vec![gastown]);
}

#[tokio::test]
async fn contract_in_memory() {
    let repo = InMemoryRigs::default();
    rig_repo_contract(&repo).await;
}

#[tokio::test]
async fn contract_postgres() {
    // GT_PG_URL = full URL to the server with the test DB created up front, e.g.
    // "postgres://gt:gt@127.0.0.1:5432/gt_rs_test". If unset, skip (host without Postgres).
    let Ok(url) = std::env::var("GT_PG_URL") else {
        eprintln!("GT_PG_URL unset — skipping the Postgres rig contract (run with Postgres available)");
        return;
    };
    let repo = PgRigs::connect(&url).await.expect("connect Postgres");
    gt_store_pg::ensure_schema(repo.pool())
        .await
        .expect("ensure_schema");
    repo.truncate().await.expect("clean tables");
    rig_repo_contract(&repo).await;
}
