//! Gate for hq-mcp-issues.1: `DoltIssues::list` against a real Dolt server.
//!
//! Skipped unless `GT_DOLT_URL` is set so host CI without a Dolt sidecar still
//! compiles. The container/dev runs invoke it via `cargo test -p gt-store-dolt`.
//!
//! The fixture writes into a throwaway `gt_rs_issues_test` database so the
//! production `hq.issues` is never touched.

use mysql_async::prelude::Queryable;

use gt_store_dolt::{DoltIssues, IssueFilter, NewIssue};

const TEST_DB: &str = "gt_rs_issues_test";

async fn seed(base: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pool = gt_store_dolt::connect(base)?;
    let mut conn = pool.get_conn().await?;
    conn.query_drop(format!("CREATE DATABASE IF NOT EXISTS {TEST_DB}"))
        .await?;
    conn.query_drop(format!("USE {TEST_DB}")).await?;
    conn.query_drop(
        "CREATE TABLE IF NOT EXISTS issues (
            id                  VARCHAR(255) PRIMARY KEY,
            content_hash        VARCHAR(64),
            title               VARCHAR(500) NOT NULL,
            description         TEXT NOT NULL,
            design              TEXT NOT NULL,
            acceptance_criteria TEXT NOT NULL,
            notes               TEXT NOT NULL,
            status              VARCHAR(32) NOT NULL DEFAULT 'open',
            priority            INT NOT NULL DEFAULT 2,
            issue_type          VARCHAR(32) NOT NULL DEFAULT 'task',
            assignee            VARCHAR(255),
            estimated_minutes   INT,
            created_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            created_by          VARCHAR(255) DEFAULT '',
            owner               VARCHAR(255) DEFAULT '',
            updated_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            closed_at           DATETIME,
            closed_by_session   VARCHAR(255) DEFAULT '',
            external_ref        VARCHAR(255),
            spec_id             VARCHAR(1024)
        )",
    )
    .await?;
    conn.query_drop("DELETE FROM issues").await?;
    conn.query_drop(
        "INSERT INTO issues (id, title, description, design, acceptance_criteria, notes,
            status, priority, issue_type, assignee, external_ref) VALUES
            ('hq-a',  'alpha',  '', '', '', '', 'open',    0, 'epic',  'alice', 'hq-root'),
            ('hq-b',  'beta',   '', '', '', '', 'working', 1, 'task',  'bob',   NULL),
            ('hq-c',  'gamma',  '', '', '', '', 'closed',  2, 'task',  NULL,    'hq-root'),
            ('hq-d',  'delta',  '', '', '', '', 'open',    2, 'spike', 'alice', NULL)",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn list_filters_combine() {
    let Ok(base) = std::env::var("GT_DOLT_URL") else {
        eprintln!("GT_DOLT_URL unset — skipping DoltIssues contract");
        return;
    };
    let base = base.trim_end_matches('/').to_string();
    seed(&base).await.expect("seed");

    let repo = DoltIssues::connect(&format!("{base}/{TEST_DB}")).expect("connect");
    repo.ensure_schema().await.expect("schema present");

    let all = repo.list(&IssueFilter::default()).await.expect("list all");
    assert_eq!(all.len(), 4, "all 4 rows visible by default");

    let open_only = repo
        .list(&IssueFilter {
            status: vec!["open".into()],
            ..Default::default()
        })
        .await
        .expect("list open");
    let ids: Vec<&str> = open_only.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"hq-a") && ids.contains(&"hq-d"));
    assert_eq!(open_only.len(), 2);

    let p_le_1 = repo
        .list(&IssueFilter {
            priority_max: Some(1),
            ..Default::default()
        })
        .await
        .expect("list priority<=1");
    let ids: Vec<&str> = p_le_1.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids.len(), 2, "hq-a (p0) + hq-b (p1)");
    assert!(ids.contains(&"hq-a"));
    assert!(ids.contains(&"hq-b"));

    let alice = repo
        .list(&IssueFilter {
            assignee: Some("alice".into()),
            ..Default::default()
        })
        .await
        .expect("list alice");
    assert_eq!(alice.len(), 2);

    let by_ref = repo
        .list(&IssueFilter {
            external_ref: Some("hq-root".into()),
            ..Default::default()
        })
        .await
        .expect("list external_ref");
    assert_eq!(by_ref.len(), 2);

    let epics = repo
        .list(&IssueFilter {
            issue_type: Some("epic".into()),
            ..Default::default()
        })
        .await
        .expect("list epics");
    assert_eq!(epics.len(), 1);
    assert_eq!(epics[0].id, "hq-a");

    let capped = repo
        .list(&IssueFilter {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .expect("list limit");
    assert_eq!(capped.len(), 2);

    let combined = repo
        .list(&IssueFilter {
            status: vec!["open".into()],
            priority_max: Some(2),
            assignee: Some("alice".into()),
            ..Default::default()
        })
        .await
        .expect("list combined");
    assert_eq!(combined.len(), 2);
}

#[tokio::test]
async fn insert_commits_atomic() {
    let Ok(base) = std::env::var("GT_DOLT_URL") else {
        eprintln!("GT_DOLT_URL unset — skipping DoltIssues.insert contract");
        return;
    };
    let base = base.trim_end_matches('/').to_string();
    seed(&base).await.expect("seed");

    let repo = DoltIssues::connect(&format!("{base}/{TEST_DB}")).expect("connect");

    let id = format!("hq-insert-{}", ulid::Ulid::new());
    let row = NewIssue {
        id: id.clone(),
        title: "insert gate".into(),
        description: "desc".into(),
        design: "design".into(),
        acceptance_criteria: "ac".into(),
        notes: "notes".into(),
        priority: 1,
        issue_type: "task".into(),
        created_by: "test".into(),
        external_ref: Some("hq-root".into()),
        assignee: Some("alice".into()),
        owner: Some("alice".into()),
    };
    repo.insert(&row).await.expect("insert");

    // Visible to a follow-up list (proves the row landed).
    let by_ref = repo
        .list(&IssueFilter {
            external_ref: Some("hq-root".into()),
            ..Default::default()
        })
        .await
        .expect("list");
    let ids: Vec<&str> = by_ref.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&id.as_str()), "{id} should be visible: ids={ids:?}");

    // Duplicate insert errors with the underlying primary-key violation.
    let err = repo.insert(&row).await.expect_err("dup must reject");
    assert!(
        err.to_string().to_lowercase().contains("duplicate")
            || err.to_string().to_lowercase().contains("primary"),
        "expected duplicate-key error, got `{err}`",
    );
}
