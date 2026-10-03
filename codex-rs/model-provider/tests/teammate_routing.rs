//! Exercise real SDK profile discovery and provider SigV4 signing without network I/O.
//! The child process isolates AWS environment variables from parallel Rust tests.

use codex_http_client::Request;
use codex_model_provider::TeammatePolicy;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use pretty_assertions::assert_eq;

#[test]
fn teammate_profiles_sign_with_distinct_accounts_despite_static_environment_keys() {
    let temp = tempfile::tempdir().expect("temporary AWS home");
    let config = temp.path().join("config");
    let credentials = temp.path().join("credentials");
    std::fs::write(&config, "[profile ammo3]\nregion = us-east-1\n").expect("config");
    std::fs::write(
        &credentials,
        "[ammo1]\naws_access_key_id = synthetic-ammo1\naws_secret_access_key = secret-one\n\n[ammo3]\naws_access_key_id = synthetic-ammo3\naws_secret_access_key = secret-three\n",
    ).expect("credentials");
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "teammate_signing_child", "--nocapture"])
        .env("CODEX_TEAMMATE_SIGNING_TEST_CHILD", "1")
        .env("AMMO_MODE", "ssh")
        .env("AMMO_LB_ENABLE", "1")
        .env("AMMO_LB_MODES", "ssh")
        .env("AMMO_LB_PROFILES", " ammo1, missing, ammo3 ")
        .env("AMMO_ARM_ENABLE", "0")
        .env("AWS_CONFIG_FILE", &config)
        .env("AWS_SHARED_CREDENTIALS_FILE", &credentials)
        .env("AWS_PROFILE", "chief")
        .env("AWS_ACCESS_KEY_ID", "synthetic-chief")
        .env("AWS_SECRET_ACCESS_KEY", "chief-secret")
        .env("AWS_SESSION_TOKEN", "chief-token")
        .env("AWS_BEARER_TOKEN_BEDROCK", "chief-bearer-token")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_REGION", "us-east-1")
        .output()
        .expect("run isolated signing test");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn source(name: &str) -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth: 1,
        agent_path: Some(AgentPath::root().join(name).expect("path")),
        agent_nickname: Some(name.to_string()),
        agent_role: Some("worker".to_string()),
    })
}

async fn authorization(info: ModelProviderInfo) -> http::HeaderMap {
    let provider = create_model_provider(info, None);
    let auth = provider
        .api_auth()
        .await
        .expect("resolve native Bedrock auth");
    let request = Request::new(
        http::Method::POST,
        "https://bedrock-mantle.us-east-1.api.aws/openai/v1/responses".to_string(),
    )
    .with_json(&serde_json::json!({"model": "test", "input": "test"}));
    auth.apply_auth(request)
        .await
        .expect("sign without sending")
        .headers
}

#[tokio::test]
async fn teammate_signing_child() {
    if std::env::var("CODEX_TEAMMATE_SIGNING_TEST_CHILD").as_deref() != Ok("1") {
        return;
    }
    let policy = TeammatePolicy::from_env();
    let chief = ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
        profile: None,
        region: Some("us-east-1".to_string()),
        credential_export: None,
        auth_refresh: None,
    }));
    let mut first = chief.clone();
    let mut second = chief.clone();
    policy
        .apply(&mut first, &mut None, Some(&source("first")))
        .await;
    policy
        .apply(&mut second, &mut None, Some(&source("second")))
        .await;
    assert_eq!(
        first.aws.as_ref().expect("AWS").profile.as_deref(),
        Some("ammo1")
    );
    assert_eq!(
        second.aws.as_ref().expect("AWS").profile.as_deref(),
        Some("ammo3")
    );
    let (first_headers, second_headers) = tokio::join!(authorization(first), authorization(second));
    for (headers, key) in [
        (first_headers, "synthetic-ammo1"),
        (second_headers, "synthetic-ammo3"),
    ] {
        assert!(
            headers[http::header::AUTHORIZATION]
                .to_str()
                .expect("header")
                .contains(&format!("Credential={key}/"))
        );
        assert!(
            !headers.contains_key("x-amz-security-token"),
            "chief token must not leak"
        );
        assert!(!headers.contains_key("chatgpt-account-id"));
    }
    assert_eq!(
        std::env::var("AWS_PROFILE").expect("chief profile"),
        "chief"
    );
    assert_eq!(
        std::env::var("AWS_ACCESS_KEY_ID").expect("chief key"),
        "synthetic-chief"
    );
    assert_eq!(
        authorization(chief).await[http::header::AUTHORIZATION],
        "Bearer chief-bearer-token"
    );
}
