use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

async fn mantle_config(home: &Path) -> Config {
    ConfigBuilder::default()
        .loader_overrides(codex_config::LoaderOverrides::without_managed_config_for_tests())
        .codex_home(home.to_path_buf())
        .cli_overrides(vec![
            ("model_provider".into(), AMAZON_BEDROCK_PROVIDER_ID.into()),
            ("remote_control_mantle".into(), true.into()),
            (
                "model_providers.amazon-bedrock.aws.profile".into(),
                "test-profile".into(),
            ),
            (
                "model_providers.amazon-bedrock.aws.region".into(),
                "us-east-1".into(),
            ),
        ])
        .build()
        .await
        .expect("Mantle configuration")
}

#[tokio::test]
async fn mantle_binding_rejects_route_changes_but_not_model_selection() {
    let home = TempDir::new().expect("temporary home");
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let config = mantle_config(home.path()).await;
    manager
        .bind_mantle_remote_control(&config)
        .expect("bind route");
    manager
        .check_mantle_remote_control(&config)
        .expect("same route");

    let mut different_model = config.clone();
    different_model.model = Some("openai.gpt-5.6-luna".into());
    manager
        .check_mantle_remote_control(&different_model)
        .expect("model change on same route");

    let mut disabled = config.clone();
    disabled.remote_control_mantle = false;
    let mut other_provider = config.clone();
    other_provider.model_provider_id = "openai".into();
    let mut other_profile = config.clone();
    other_profile
        .model_provider
        .aws
        .as_mut()
        .expect("AWS config")
        .profile = Some("other".into());
    let mut other_region = config.clone();
    other_region
        .model_provider
        .aws
        .as_mut()
        .expect("AWS config")
        .region = Some("us-west-2".into());
    let mut other_endpoint = config.clone();
    other_endpoint.model_provider.base_url = Some("https://api.openai.com/v1".into());
    for changed in [
        disabled,
        other_provider,
        other_profile,
        other_region,
        other_endpoint,
    ] {
        assert_eq!(
            manager
                .check_mantle_remote_control(&changed)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}

#[tokio::test]
async fn mantle_config_rejects_runtime_and_openai_without_permissive_fallback() {
    for provider in ["openai", "amazon-bedrock-runtime"] {
        let home = TempDir::new().expect("temporary home");
        std::fs::write(
            home.path().join("config.toml"),
            format!("remote_control_mantle = true\nmodel_provider = {provider:?}\n"),
        )
        .expect("write config");
        let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
        assert!(manager.load_startup_config(None).await.is_err());
    }
}

#[tokio::test]
async fn mantle_binding_rejects_client_disabling_mode_and_selecting_openai_together() {
    let home = TempDir::new().expect("temporary home");
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let config = mantle_config(home.path()).await;
    manager
        .bind_mantle_remote_control(&config)
        .expect("bind route");
    let overrides = HashMap::from([
        ("model_provider".to_string(), serde_json::json!("openai")),
        (
            "remote_control_mantle".to_string(),
            serde_json::json!(false),
        ),
    ]);
    assert!(
        manager
            .load_with_overrides(Some(overrides), ConfigOverrides::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn mantle_invalid_provider_definition_must_not_fall_back_to_openai() {
    let home = TempDir::new().expect("temporary home");
    std::fs::write(
        home.path().join("config.toml"),
        r#"
remote_control_mantle = true
model_provider = "amazon-bedrock"
[model_providers.amazon-bedrock]
request_max_retries = 0
"#,
    )
    .expect("invalid native provider config");
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let error = manager
        .load_startup_config(None)
        .await
        .err()
        .expect("must not select OpenAI defaults");
    assert!(error.to_string().contains("Mantle"));
}
