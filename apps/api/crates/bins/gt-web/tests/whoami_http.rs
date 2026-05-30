//! `GET /api/whoami` end-to-end.
//!
//! - hq-fe-rbac.4 — open mode → `actor=web:open`, `mode=open`; bearer mode → `actor=
//!   web:<sha-prefix>`, `mode=bearer`. Empty roles/scopes on the wire.
//! - hq-fe-rbac.1 — JWT mode → `actor=<sub>`, `mode=jwt`, `roles`/`scopes` mirror the
//!   verified claims; unsigned/expired/tampered tokens 401 ahead of the handler.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use gt_agent::InMemorySessions;
use gt_beads::InMemoryBeads;
use gt_root::{root::Effects, spawn, RootConfig, RootHandle, SystemClock};
use gt_web::{
    auth::actor_tag, router, AppState, AuthConfig, InMemoryWebAudit, JwtIssuer, ReadinessGate,
    WebAuditSink,
};

struct NoopEffects;
impl Effects for NoopEffects {
    fn sling(&self, _convoy: &str, _member: &str) {}
    fn rotate(&self, _account: &str) {}
}

async fn boot(auth: AuthConfig) -> (String, RootHandle<Arc<InMemoryBeads>>) {
    let beads = Arc::new(InMemoryBeads::default());
    let sessions = Arc::new(InMemorySessions::new(vec![]));
    let merges = Arc::new(gt_merge::InMemoryMergeRepo::default());
    let log = std::env::temp_dir().join(format!("gt-web-whoami-{}.jsonl", ulid::Ulid::new()));
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
async fn whoami_open_mode_surfaces_dev_actor() {
    let (base, root) = boot(AuthConfig::open()).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::WhoamiDto = client
        .get(format!("{base}/api/whoami"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.actor, "web:open");
    assert_eq!(body.mode, "open");
    assert!(body.roles.is_empty());
    assert!(body.scopes.is_empty());

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn whoami_bearer_mode_returns_actor_tag() {
    let secret = "topsecret-bearer-token";
    let (base, root) = boot(AuthConfig::bearer(secret)).await;
    let client = reqwest::Client::new();

    let body: gt_web::dto::WhoamiDto = client
        .get(format!("{base}/api/whoami"))
        .header("authorization", format!("Bearer {secret}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.actor, actor_tag(secret));
    assert_eq!(body.mode, "bearer");
    assert!(body.roles.is_empty());

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn whoami_bearer_mode_rejects_missing_header() {
    let (base, root) = boot(AuthConfig::bearer("topsecret")).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/whoami"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn whoami_jwt_mode_returns_sub_and_claims() {
    let issuer = JwtIssuer::from_secret("jwt-secret").shared();
    let (base, root) = boot(AuthConfig::jwt(issuer.clone())).await;
    let token = issuer
        .sign(
            "claude-host",
            vec!["sheriff".into(), "deacon".into()],
            vec!["beads.write".into(), "merge.read".into()],
        )
        .unwrap();
    let client = reqwest::Client::new();

    let body: gt_web::dto::WhoamiDto = client
        .get(format!("{base}/api/whoami"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.actor, "claude-host");
    assert_eq!(body.mode, "jwt");
    assert_eq!(body.roles, vec!["sheriff".to_string(), "deacon".to_string()]);
    assert_eq!(
        body.scopes,
        vec!["beads.write".to_string(), "merge.read".to_string()]
    );

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}

#[tokio::test]
async fn whoami_jwt_mode_rejects_token_from_other_signer() {
    let server_issuer = JwtIssuer::from_secret("server-secret").shared();
    let attacker = JwtIssuer::from_secret("not-the-server-secret");
    let bad = attacker.sign("evil-actor", vec![], vec![]).unwrap();
    let (base, root) = boot(AuthConfig::jwt(server_issuer)).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/whoami"))
        .header("authorization", format!("Bearer {bad}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    tokio::time::sleep(Duration::from_millis(10)).await;
    root.shutdown();
}
