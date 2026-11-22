use agent_runtime::config::{Config, ProviderAuth};
use std::fs;

#[test]
fn codex_credentials_are_explicit_and_scoped_to_the_subscription_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("project");
    let home = dir.path().join("engine");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&home).unwrap();
    let path = home.join("account.json");
    fs::write(&path, "{}").unwrap();
    let mut c = Config::initial(workspace.clone(), "example".into(), "gpt-6.2-sol".into());
    c.provider.allow_unverified_model = true;
    c.provider.auth = ProviderAuth::Codex;
    c.provider.codex_account_id = Some("fixture".into());
    c.provider.api_key_env.clear();
    c.provider.codex_credentials_file = Some(path);
    c.provider.endpoint = "https://chatgpt.com/backend-api/codex/responses".into();
    c.provider.native.tools = vec![serde_json::json!({"type":"image_generation"})];
    c.validate(&home).unwrap();
    let mut wrong = c.clone();
    wrong.provider.codex_account_id = None;
    assert!(wrong.validate(&home).is_err());
    let old_path_only_key = wrong.provider.fingerprint();
    let store = agent_runtime::store::Store::open(home.clone()).unwrap();
    let session = store
        .create(workspace.clone(), agent_runtime::config::Mode::ReadOnly)
        .unwrap();
    store
        .bind_provider(&session.id, &old_path_only_key, false)
        .unwrap();
    assert!(
        store
            .bind_configured_provider(&session.id, &c.provider)
            .is_err()
    );
    let mut wrong = c.clone();
    wrong.provider.endpoint = "https://api.openai.com/v1/responses".into();
    assert!(wrong.validate(&home).is_err());
    wrong = c.clone();
    wrong.provider.auth = ProviderAuth::Bearer;
    assert!(wrong.validate(&home).is_err());
    let exposed = workspace.join("account.json");
    fs::write(&exposed, "{}").unwrap();
    c.provider.codex_credentials_file = Some(exposed);
    assert!(c.validate(&home).is_err());
}
