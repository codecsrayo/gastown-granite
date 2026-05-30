//! `GET /api/worktrees` gate (hq-fe-api-r.8). Builds a real on-disk repo + worktree, wires
//! the router with `town_root: Some(repo)`, and asserts the DTO mirrors `git worktree list`
//! plus `git status --porcelain=v2`. Keeps the shell-out behaviour honest end-to-end — the
//! parsers in `routes.rs` are private, so this is the contract test for the endpoint.

use std::process::Command;
use std::sync::Arc;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, SystemClock};
use gt_web::dto::WorktreeDto;
use gt_web::{router, AppState, AuthConfig, InMemoryWebAudit, ReadinessGate, WebAuditSink};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

fn tempdir(label: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gt-web-worktrees-{label}-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Run `git ARGS` inside `cwd`. Panics on non-zero exit so a broken test fixture surfaces
/// immediately instead of pretending the test passed.
fn git(cwd: &std::path::Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: stderr={} stdout={}",
        args,
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
}

fn init_repo(root: &std::path::Path) {
    // `-b main` so the default branch matches what the handler's ahead/behind query expects.
    git(root, &["init", "-b", "main", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "test"]);
    git(root, &["commit", "--allow-empty", "-q", "-m", "init"]);
}

async fn boot(town_root: std::path::PathBuf) -> String {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-worktrees-log-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        log,
        RootConfig::default(),
    );
    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: Some(Arc::new(town_root)),
        issues: None,
        bus: None,
        worktrees_stream: None,
        control: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn empty_when_town_root_unset() {
    // `boot` would set Some(...), so build the AppState by hand with `town_root: None`.
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-worktrees-log-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        log,
        RootConfig::default(),
    );
    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: None,
        worktrees_stream: None,
        control: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let rows: Vec<WorktreeDto> = reqwest::Client::new()
        .get(format!("http://{addr}/api/worktrees"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rows.is_empty(), "expected empty list, got {rows:?}");
}

#[tokio::test]
async fn lists_main_and_worktree_with_dirty_and_divergence() {
    let repo = tempdir("main");
    init_repo(&repo);

    // Branch off `main` into a sibling worktree, add one commit (ahead=1) and leave a dirty
    // tracked + untracked file so the porcelain v2 parser exercises both shapes.
    let wt = tempdir("branch");
    // `worktree add` creates the destination, so drop the empty pre-created dir first.
    std::fs::remove_dir_all(&wt).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feat/sample",
            wt.to_str().unwrap(),
        ],
    );
    std::fs::write(wt.join("tracked.txt"), "v1\n").unwrap();
    git(&wt, &["add", "tracked.txt"]);
    git(&wt, &["commit", "-q", "-m", "add tracked"]);
    std::fs::write(wt.join("tracked.txt"), "v2\n").unwrap(); // unstaged modify -> ".M"
    std::fs::write(wt.join("loose.txt"), "x\n").unwrap(); // untracked -> "??"

    let base = boot(repo.clone()).await;
    let rows: Vec<WorktreeDto> = reqwest::Client::new()
        .get(format!("{base}/api/worktrees"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(rows.len(), 2, "expected main + branch worktrees: {rows:?}");

    let main_row = &rows[0];
    assert!(main_row.is_main, "main worktree must be flagged: {main_row:?}");
    assert_eq!(main_row.branch.as_deref(), Some("main"));
    assert_eq!(main_row.ahead, 0);
    assert_eq!(main_row.behind, 0);
    assert!(main_row.dirty.is_empty(), "main should be clean: {main_row:?}");

    let branch_row = rows
        .iter()
        .find(|r| r.branch.as_deref() == Some("feat/sample"))
        .expect("feat/sample row missing");
    assert!(!branch_row.is_main);
    assert_eq!(branch_row.ahead, 1, "one commit ahead of main");
    assert_eq!(branch_row.behind, 0);

    let dirty_paths: Vec<&str> = branch_row.dirty.iter().map(|d| d.path.as_str()).collect();
    assert!(
        dirty_paths.contains(&"tracked.txt"),
        "tracked modify missing: {:?}",
        branch_row.dirty
    );
    assert!(
        dirty_paths.contains(&"loose.txt"),
        "untracked file missing: {:?}",
        branch_row.dirty
    );
    let untracked = branch_row
        .dirty
        .iter()
        .find(|d| d.path == "loose.txt")
        .unwrap();
    assert_eq!(untracked.xy, "??");

    // hq-fe-api-r.10: HEAD commit subject + author surfaced per row. The fixture commit on
    // the branch worktree is "add tracked"; the test git config sets author name to "test".
    assert_eq!(branch_row.head_subject.as_deref(), Some("add tracked"));
    assert_eq!(branch_row.head_author.as_deref(), Some("test"));
    assert_eq!(main_row.head_subject.as_deref(), Some("init"));
    assert_eq!(main_row.head_author.as_deref(), Some("test"));

    // hq-fe-api-r.11: head_time (Unix seconds) present on both rows, and the branch commit
    // (created after init) is at least as new as main's. `>=` not `>` because both commits
    // can land within the same second on a fast test run.
    let main_time = main_row.head_time.expect("main head_time missing");
    let branch_time = branch_row.head_time.expect("branch head_time missing");
    assert!(
        branch_time >= main_time,
        "branch commit must not pre-date init: branch={branch_time} main={main_time}"
    );
}

/// hq-fe-api-r.12 gate: when `AppState.worktrees_stream` is unset, the SSE endpoint must
/// fail-fast with 503 instead of hanging — clients can fall back to the snapshot endpoint
/// or surface the disabled state.
#[tokio::test]
async fn worktrees_stream_503_when_unwired() {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-worktrees-log-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        log,
        RootConfig::default(),
    );
    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: None,
        worktrees_stream: None,
        control: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/worktrees/stream"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

/// hq-fe-api-r.12 happy path: when the broadcast channel is wired, a connected SSE client
/// receives the next snapshot the test pushes into it. Uses a hand-built channel rather
/// than the real polling task so the test doesn't sleep waiting for a 2s tick.
#[tokio::test]
async fn worktrees_stream_delivers_snapshot() {
    use futures::StreamExt;
    let repo = tempdir("stream");
    init_repo(&repo);

    // Build AppState by hand with a fresh broadcast channel; bypass the production poller
    // and push the snapshot ourselves so the test is deterministic.
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::default());
    let log = {
        let mut p = std::env::temp_dir();
        p.push(format!("gt-web-worktrees-log-{}.jsonl", ulid::Ulid::new()));
        p
    };
    let root = spawn(
        beads.clone(),
        Arc::new(gt_merge::InMemoryMergeRepo::default()),
        Arc::new(gt_patrol::InMemoryPatrolRepo::default()),
        Arc::new(gt_orchestration::InMemoryOrchRepo::default()),
        NoopEffects,
        SystemClock,
        log,
        RootConfig::default(),
    );
    let (tx, _rx) = tokio::sync::broadcast::channel::<Vec<WorktreeDto>>(8);
    let state = AppState {
        beads,
        sessions,
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: Some(Arc::new(repo.clone())),
        issues: None,
        bus: None,
        worktrees_stream: Some(tx.clone()),
        control: None,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, AuthConfig::open(), sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/worktrees/stream"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();

    // Wait for the broadcast to register a receiver before pushing, otherwise `send`
    // returns Err(NoSubscribers) and the event is dropped.
    for _ in 0..50 {
        if tx.receiver_count() >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let snap = gt_web::collect_worktrees(&repo).await.unwrap();
    tx.send(snap).expect("broadcast send");

    let mut buf = Vec::<u8>::new();
    let saw_main = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.unwrap());
            if let Ok(s) = std::str::from_utf8(&buf) {
                if s.contains("\"is_main\":true") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(
        saw_main,
        "no main-worktree SSE frame received; got: {}",
        String::from_utf8_lossy(&buf)
    );
}
