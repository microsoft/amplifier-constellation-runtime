use agent_runtime::config::Config;
use serde_json::Value;
use std::process::Command;

#[test]
fn inspect_can_repair_retired_models_but_execution_stays_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("state");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config::initial(workspace, "Fixture".into(), "gpt-5.6-sol".into());
    let path = home.join("runtime.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
            .arg("--home")
            .arg(&home)
            .args(args)
            .output()
            .unwrap()
    };
    let inspection = run(&["config-inspect"]);
    assert!(inspection.status.success());
    let value: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(value["validation"]["valid"], false);
    for command in ["check", "acp", "capabilities"] {
        let rejected = run(&[command]);
        assert!(
            !rejected.status.success(),
            "{command} must not bypass validation"
        );
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("supported classes"));
        assert!(rejected.stdout.is_empty());
    }
    config.provider.model = "gpt-6.1-sol".into();
    let candidate = temp.path().join("candidate.json");
    std::fs::write(&candidate, serde_json::to_vec(&config).unwrap()).unwrap();
    let updated = run(&[
        "configure",
        candidate.to_str().unwrap(),
        "--expected-revision",
        value["revision"].as_str().unwrap(),
    ]);
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    assert!(run(&["check"]).status.success());
    // Other invalid security settings also stay inspectable without executing.
    config.workspace = temp.path().join("missing-workspace");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(run(&["config-inspect"]).status.success());
    for command in ["check", "acp", "capabilities"] {
        assert!(!run(&[command]).status.success());
    }
}
