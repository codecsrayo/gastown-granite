//! hq-fe-skills.2: `GET /api/skills` + `GET /api/roles` read-side wiring.
//!
//! Three fixtures cover the contract surface:
//!
//! 1. **None handle → empty arrays**: when `AppState.skills` is unwired the routes
//!    return `[]` (200, never 404) so the dashboard renders a stable shell without
//!    conditional logic on the wire shape — same posture as `/api/issues` /
//!    `/api/worktrees`.
//! 2. **Wired actor → catalog + bindings projection**: register two skills and
//!    enable one on a role; the responses must surface the skills sorted by id
//!    (BTreeMap iteration) and the role's `skills` field sorted alphabetically
//!    (BTreeSet iteration).
//! 3. **Scope guard enforcement**: in JWT mode, a token without `skills.read`
//!    must be rejected `403` on both routes; a token with the scope passes.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_merge::InMemoryMergeRepo;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_skills::{EnableSkillForRole, RegisterSkill, SkillCommand, SkillHandle};
use gt_web::{
    router, AppState, AuthConfig, InMemoryWebAudit, JwtIssuer, ReadinessGate, WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

/// Build the gateway with the supplied skills handle and auth posture. `skills=None`
/// exercises the empty-array short-circuit branch; `Some(handle)` exercises the
/// actor-snapshot projection branch.
async fn boot(
    skills: Option<SkillHandle>,
    auth: AuthConfig,
) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let merges = Arc::new(InMemoryMergeRepo::default());
    let log = std::env::temp_dir().join(format!("gt-web-skills-{}.jsonl", ulid::Ulid::new()));
    let root = spawn(
        beads.clone(),
        merges.clone(),
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
        merges: merges.clone(),
        agent_events: root.agent_events.clone(),
        events: root.events_sender(),
        town_root: None,
        issues: None,
        bus: None,
        worktrees_stream: None,
        control: None,
        respawner: None,
        commenter: None,
        event_log: None,
        login_registry: Arc::new(gt_web::LoginRegistry::new()),
        login_pty: None,
        login_config: Arc::new(gt_web::LoginConfig::default()),
        terminal_attach: None,
        skills,
    };
    let sink: Arc<dyn WebAuditSink> = Arc::new(InMemoryWebAudit::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state, auth, sink, ReadinessGate::ready());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), root)
}

#[tokio::test]
async fn skills_and_roles_empty_when_handle_unwired() {
    let (base, root) = boot(None, AuthConfig::open()).await;
    let client = reqwest::Client::new();

    let skills: Vec<gt_web::dto::SkillDto> = client
        .get(format!("{base}/api/skills"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(skills.is_empty(), "no actor → empty catalog");

    let roles: Vec<gt_web::dto::RoleSkillsDto> = client
        .get(format!("{base}/api/roles"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(roles.is_empty(), "no actor → empty bindings");

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn skills_and_roles_project_actor_snapshot() {
    // Spawn the catalog actor with a discard-only relay (the route is read-only;
    // emitted SkillEvents are not surfaced over HTTP in `.2`).
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let handle = gt_skills::spawn(tx);

    // Two registered skills + one role binding ⇒ catalog has both, the role lists
    // only the enabled one. Use deliberately out-of-alphabetical-order inserts so
    // the assertion proves the BTreeMap/BTreeSet sort lands on the wire.
    handle
        .exec(SkillCommand::Register(RegisterSkill {
            skill: "merge_admin".into(),
            label: "Merge admin".into(),
            description: "Can drive merge slots".into(),
            default_scopes: vec!["merge.write".into()],
            now_secs: 1,
        }))
        .await
        .unwrap();
    handle
        .exec(SkillCommand::Register(RegisterSkill {
            skill: "audit_reader".into(),
            label: "Audit reader".into(),
            description: "Can read audit log".into(),
            default_scopes: vec!["feed.read".into()],
            now_secs: 2,
        }))
        .await
        .unwrap();
    handle
        .exec(SkillCommand::Enable(EnableSkillForRole {
            role: "deacon".into(),
            skill: "merge_admin".into(),
            now_secs: 3,
        }))
        .await
        .unwrap();

    let (base, root) = boot(Some(handle), AuthConfig::open()).await;
    let client = reqwest::Client::new();

    let skills: Vec<gt_web::dto::SkillDto> = client
        .get(format!("{base}/api/skills"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["audit_reader", "merge_admin"],
        "BTreeMap iteration ⇒ sorted by id"
    );
    let admin = skills.iter().find(|s| s.id == "merge_admin").unwrap();
    assert_eq!(admin.label, "Merge admin");
    assert_eq!(admin.default_scopes, vec!["merge.write".to_string()]);

    let roles: Vec<gt_web::dto::RoleSkillsDto> = client
        .get(format!("{base}/api/roles"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].role, "deacon");
    assert_eq!(roles[0].skills, vec!["merge_admin".to_string()]);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn toggle_enables_then_idempotently_re_enables_then_disables() {
    use std::sync::{Arc, Mutex};
    // Real actor + dispatch through `POST /api/roles/:role/skills`. Capture every
    // emitted `SkillEvent` into a shared Vec so the assertion proves the idempotent
    // replay did not produce an extra wire event (the route short-circuited).
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<gt_events::Envelope<gt_skills::SkillEvent>>(16);
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let log_writer = log.clone();
    tokio::spawn(async move {
        while let Some(env) = rx.recv().await {
            let kind = match env.payload {
                gt_skills::SkillEvent::Registered { .. } => "registered",
                gt_skills::SkillEvent::Retired { .. } => "retired",
                gt_skills::SkillEvent::EnabledForRole { .. } => "enabled",
                gt_skills::SkillEvent::DisabledForRole { .. } => "disabled",
            };
            log_writer.lock().unwrap().push(kind);
        }
    });
    let handle = gt_skills::spawn(tx);
    handle
        .exec(SkillCommand::Register(RegisterSkill {
            skill: "merge_admin".into(),
            label: "Merge admin".into(),
            description: "".into(),
            default_scopes: vec!["merge.write".into()],
            now_secs: 1,
        }))
        .await
        .unwrap();

    let (base, root) = boot(Some(handle), AuthConfig::open()).await;
    let client = reqwest::Client::new();

    // First enable: 200 + body echoes desired state.
    let resp = client
        .post(format!("{base}/api/roles/deacon/skills"))
        .json(&serde_json::json!({"skill": "merge_admin", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: gt_web::dto::SkillToggleResponse = resp.json().await.unwrap();
    assert_eq!(body.role, "deacon");
    assert!(body.enabled);

    // Idempotent replay: same body again → 200, no extra SkillEvent fan-out.
    let resp = client
        .post(format!("{base}/api/roles/deacon/skills"))
        .json(&serde_json::json!({"skill": "merge_admin", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Disable flips back. Cross-check the role snapshot has no skills left.
    let resp = client
        .post(format!("{base}/api/roles/deacon/skills"))
        .json(&serde_json::json!({"skill": "merge_admin", "enabled": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let roles: Vec<gt_web::dto::RoleSkillsDto> = client
        .get(format!("{base}/api/roles"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Binding row sticks around (apply_disable keeps empty bindings — a distinct,
    // valid state from "never bound").
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].role, "deacon");
    assert!(roles[0].skills.is_empty());

    // Let the actor flush its mpsc into the capture task before snapshotting the log.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let kinds = log.lock().unwrap().clone();
    // Exactly register + enable + disable — the idempotent replay must NOT emit.
    assert_eq!(kinds, vec!["registered", "enabled", "disabled"]);

    root.shutdown();
}

#[tokio::test]
async fn toggle_with_unknown_skill_returns_400() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let handle = gt_skills::spawn(tx);

    let (base, root) = boot(Some(handle), AuthConfig::open()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/roles/deacon/skills"))
        .json(&serde_json::json!({"skill": "ghost", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "unknown skill must surface as 400");

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn toggle_route_enforces_skills_write_scope_in_jwt_mode() {
    let issuer = JwtIssuer::from_secret("test-secret").shared();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let handle = gt_skills::spawn(tx);
    handle
        .exec(SkillCommand::Register(RegisterSkill {
            skill: "audit_reader".into(),
            label: "Audit reader".into(),
            description: "".into(),
            default_scopes: vec![],
            now_secs: 1,
        }))
        .await
        .unwrap();
    let (base, root) = boot(Some(handle), AuthConfig::jwt(issuer.clone())).await;
    let client = reqwest::Client::new();

    // `skills.read` alone is not enough — write scope required.
    let reader = issuer
        .sign("reader", vec!["reader".into()], vec!["skills.read".into()])
        .unwrap();
    let resp = client
        .post(format!("{base}/api/roles/deacon/skills"))
        .bearer_auth(&reader)
        .json(&serde_json::json!({"skill": "audit_reader", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // With `skills.write` the call succeeds.
    let writer = issuer
        .sign(
            "operator",
            vec!["mayor".into()],
            vec!["skills.write".into()],
        )
        .unwrap();
    let resp = client
        .post(format!("{base}/api/roles/deacon/skills"))
        .bearer_auth(&writer)
        .json(&serde_json::json!({"skill": "audit_reader", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn skills_routes_enforce_skills_read_scope_in_jwt_mode() {
    let issuer = JwtIssuer::from_secret("test-secret").shared();
    let (base, root) = boot(None, AuthConfig::jwt(issuer.clone())).await;
    let client = reqwest::Client::new();

    // Token without `skills.read` ⇒ 403 on both routes.
    let bad = issuer
        .sign(
            "stranger",
            vec!["reader".into()],
            vec!["beads.read".into()],
        )
        .unwrap();
    for path in ["/api/skills", "/api/roles"] {
        let resp = client
            .get(format!("{base}{path}"))
            .bearer_auth(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "{path} must require skills.read");
    }

    // Token with `skills.read` ⇒ both routes return 200 + empty array (no actor wired).
    let good = issuer
        .sign(
            "operator",
            vec!["mayor".into()],
            vec!["skills.read".into()],
        )
        .unwrap();
    for path in ["/api/skills", "/api/roles"] {
        let resp = client
            .get(format!("{base}{path}"))
            .bearer_auth(&good)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path} must accept skills.read");
    }

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}
