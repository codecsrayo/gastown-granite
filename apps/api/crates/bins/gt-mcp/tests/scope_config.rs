//! Gate test for Paso 6.f.10: `ScopeConfig::load` reads a per-actor scope file (TOML or
//! JSON, chosen by extension) and resolves each connection's scope. There is no hardcoded
//! admin — an actor absent from the file, or a missing file, denies by default.

use std::io::Write;

use gt_mcp::ScopeConfig;

fn temp_file(name: &str, body: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("gt-mcp-scope-{}-{}-{}", std::process::id(), nanos, name));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p
}

const TOML_BODY: &str = r#"
[actors.max]
allow = ["*"]

[actors.scheduler-bot]
allow = ["scheduling.*", "patrol.tick.execute"]

[actors.watcher]
allow = ["agent.*"]
validate_only = true
"#;

const JSON_BODY: &str = r#"
{ "actors": {
    "max": { "allow": ["*"] },
    "scheduler-bot": { "allow": ["scheduling.*", "patrol.tick.execute"] },
    "watcher": { "allow": ["agent.*"], "validate_only": true }
} }
"#;

fn check_resolution(cfg: &ScopeConfig) {
    // Full admin.
    cfg.resolve("max").check("orch.launch_convoy.execute").unwrap();

    // Scoped bot: scheduling.* + one explicit patrol tool, nothing else.
    let bot = cfg.resolve("scheduler-bot");
    bot.check("scheduling.enqueue.execute").unwrap();
    bot.check("patrol.tick.execute").unwrap();
    assert!(bot.check("merge.submit.execute").is_err(), "merge not granted to bot");
    assert!(bot.check("patrol.close.execute").is_err(), "only patrol.tick granted");

    // Watcher: validate-only over the agent domain.
    let watcher = cfg.resolve("watcher");
    watcher.check("agent.transition.validate").unwrap();
    assert!(watcher.check("agent.transition.execute").is_err(), "validate_only blocks execute");

    // Unknown actor: denied, never admin.
    assert!(cfg.resolve("intruder").check("agent.add.validate").is_err());
}

#[test]
fn loads_toml_by_extension() {
    let path = temp_file("scope.toml", TOML_BODY);
    let cfg = ScopeConfig::load(&path).expect("load toml");
    check_resolution(&cfg);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn loads_json_by_extension() {
    let path = temp_file("scope.json", JSON_BODY);
    let cfg = ScopeConfig::load(&path).expect("load json");
    check_resolution(&cfg);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn missing_file_is_an_error() {
    let path = std::env::temp_dir().join("gt-mcp-scope-does-not-exist.toml");
    let _ = std::fs::remove_file(&path);
    assert!(ScopeConfig::load(&path).is_err(), "missing config must error, not silently allow");
}
