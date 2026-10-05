//! End-to-end routing through the real AWS profile loader and native signer.
//! Requests are signed locally; these tests do not contact an AWS service.

use codex_http_client::Request;
use codex_model_provider::BedrockSubagentContext;
use codex_model_provider::create_model_provider;
use codex_model_provider::route_bedrock_subagent;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::ThreadId;
use std::path::Path;

#[test]
fn bedrock_profiles_and_keys_are_balanced_and_sticky_across_processes() {
    let temp = tempfile::tempdir().expect("temporary home");
    let credentials = temp.path().join("credentials");
    let config = temp.path().join("config");
    std::fs::write(
        &config,
        "[profile a]\nregion = us-east-1\n[profile b]\nregion = us-west-2\n",
    )
    .expect("config");
    std::fs::write(&credentials, "[a]\naws_access_key_id = synthetic-a\naws_secret_access_key = secret-a\n[b]\naws_access_key_id = synthetic-b\naws_secret_access_key = secret-b\n").expect("credentials");
    std::fs::write(temp.path().join("key"), "synthetic-key-token\n").expect("key");
    let run = |phase: &str| {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--ignored",
                "--exact",
                "bedrock_routing_child",
                "--nocapture",
            ])
            .env("CODEX_LB_TEST_HOME", temp.path())
            .env("CODEX_LB_TEST_PHASE", phase)
            .env("AWS_SHARED_CREDENTIALS_FILE", &credentials)
            .env("AWS_CONFIG_FILE", &config)
            .env("AWS_PROFILE", "missing-chief")
            .env("AWS_ACCESS_KEY_ID", "synthetic-chief")
            .env("AWS_SECRET_ACCESS_KEY", "chief-secret")
            .env("AWS_SESSION_TOKEN", "chief-session")
            .env("AWS_BEARER_TOKEN_BEDROCK", "chief-bearer")
            .env("AWS_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .output()
            .expect("isolated process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("ROUTING_SCENARIO_PASSED"),
            "scenario did not execute: {stdout}"
        );
    };
    run("spawn");
    run("resume"); // New OS process; no in-memory affinity survives.
    std::fs::write(&config, "[profile b]\nregion = us-west-2\n").expect("remove a config");
    std::fs::write(
        &credentials,
        "[b]\naws_access_key_id = synthetic-b\naws_secret_access_key = secret-b\n",
    )
    .expect("remove a");
    run("missing-profile");
}

#[test]
fn bedrock_default_profile_keeps_subagents_on_the_same_account() {
    let temp = tempfile::tempdir().expect("temporary home");
    let credentials = temp.path().join("credentials");
    let config = temp.path().join("config");
    std::fs::write(&config, "[default]\nregion = us-east-1\n").expect("config");
    std::fs::write(
        &credentials,
        "[default]\naws_access_key_id = synthetic-default\naws_secret_access_key = default-secret\n",
    )
    .expect("credentials");
    for phase in ["default-no-pool", "default-missing-pool"] {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--ignored",
                "--exact",
                "bedrock_default_profile_child",
                "--nocapture",
            ])
            .env("CODEX_LB_TEST_HOME", temp.path().join(phase))
            .env("CODEX_LB_TEST_PHASE", phase)
            .env("AWS_SHARED_CREDENTIALS_FILE", &credentials)
            .env("AWS_CONFIG_FILE", &config)
            .env("AWS_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env_remove("AWS_PROFILE")
            .env_remove("AWS_DEFAULT_PROFILE")
            .env_remove("AWS_ACCESS_KEY_ID")
            .env_remove("AWS_SECRET_ACCESS_KEY")
            .env_remove("AWS_SESSION_TOKEN")
            .env_remove("AWS_BEARER_TOKEN_BEDROCK")
            .env_remove("AWS_WEB_IDENTITY_TOKEN_FILE")
            .output()
            .expect("isolated process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("DEFAULT_ACCOUNT_SCENARIO_PASSED"),
            "scenario did not execute: {stdout}"
        );
    }
}

#[tokio::test]
#[ignore = "subprocess helper; the parent supplies only a synthetic default AWS profile"]
async fn bedrock_default_profile_child() {
    let home = std::env::var("CODEX_LB_TEST_HOME").expect("test home");
    let home = Path::new(&home);
    let missing_pool =
        std::env::var("CODEX_LB_TEST_PHASE").expect("phase") == "default-missing-pool";
    let info = ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
        region: Some("us-east-1".to_string()),
        subagent_profiles: if missing_pool {
            vec!["unavailable".to_string()]
        } else {
            Vec::new()
        },
        ..Default::default()
    }));
    for id in [ThreadId::from_u128(20), ThreadId::from_u128(21)] {
        for is_new in [true, false] {
            let routed = route_bedrock_subagent(home, context(id, is_new), info.clone(), None)
                .await
                .expect("default account route");
            let signed = headers(routed).await;
            let authorization = signed[http::header::AUTHORIZATION]
                .to_str()
                .expect("authorization");
            assert!(authorization.contains("Credential=synthetic-default/"));
            assert!(authorization.contains("/us-east-1/bedrock-mantle/"));
            assert!(!signed.contains_key("x-amz-security-token"));
            assert!(!signed.contains_key("chatgpt-account-id"));
        }
    }
    assert!(
        !home.join("bedrock-subagents/cursor.json").exists(),
        "inherited accounts must not consume rotation slots"
    );
    println!("DEFAULT_ACCOUNT_SCENARIO_PASSED");
}

fn context(id: ThreadId, is_new: bool) -> BedrockSubagentContext {
    BedrockSubagentContext {
        thread_id: id,
        is_new,
        is_subagent: true,
        parent_id: Some(ThreadId::from_u128(1)),
        affinity_source: None,
        source_provider: None,
    }
}

async fn headers(info: ModelProviderInfo) -> http::HeaderMap {
    let provider = create_model_provider(info, None);
    let auth = provider.api_auth().await.expect("native auth");
    let request = Request::new(
        http::Method::POST,
        "https://bedrock-mantle.us-east-1.api.aws/openai/v1/responses".to_string(),
    )
    .with_json(&serde_json::json!({"model":"test","input":"test","store":false}));
    auth.apply_auth(request)
        .await
        .expect("sign locally")
        .headers
}

#[tokio::test]
#[ignore = "subprocess helper; the parent test runs each phase with isolated AWS settings"]
async fn bedrock_routing_child() {
    let home = std::env::var("CODEX_LB_TEST_HOME").expect("test home");
    let home = Path::new(&home);
    let mut info =
        ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
            region: Some("us-east-1".to_string()),
            subagent_profiles: vec!["a".to_string(), "missing".to_string(), "b".to_string()],
            subagent_api_key_files: vec![home.join("key")],
            ..Default::default()
        }));
    let phase = std::env::var("CODEX_LB_TEST_PHASE").expect("phase");
    let ids = [
        ThreadId::from_u128(10),
        ThreadId::from_u128(11),
        ThreadId::from_u128(12),
    ];
    if phase == "missing-profile" {
        assert!(
            route_bedrock_subagent(home, context(ids[0], false), info, None)
                .await
                .is_err()
        );
        println!("ROUTING_SCENARIO_PASSED");
        return;
    }
    if phase == "resume" {
        let aws = info.aws.as_mut().expect("AWS");
        aws.subagent_profiles.clear();
        aws.subagent_api_key_files.clear();
    }
    for (id, expected) in ids.into_iter().zip(["a", "b", "key"]) {
        let routed =
            route_bedrock_subagent(home, context(id, phase == "spawn"), info.clone(), None)
                .await
                .expect("route");
        let signed = headers(routed).await;
        let authorization = signed[http::header::AUTHORIZATION]
            .to_str()
            .expect("authorization");
        if expected == "key" {
            assert_eq!(authorization, "Bearer synthetic-key-token");
        } else {
            assert!(authorization.contains(&format!("Credential=synthetic-{expected}/")));
            assert!(authorization.contains("/us-east-1/bedrock-mantle/"));
            assert!(!signed.contains_key("x-amz-security-token"));
        }
        assert!(!signed.contains_key("chatgpt-account-id"));
    }
    assert_eq!(
        headers(info).await[http::header::AUTHORIZATION],
        "Bearer chief-bearer"
    );
    assert_eq!(
        std::env::var("AWS_PROFILE").expect("chief"),
        "missing-chief"
    );
    assert_eq!(
        std::env::var("AWS_BEARER_TOKEN_BEDROCK").expect("chief bearer"),
        "chief-bearer"
    );
    println!("ROUTING_SCENARIO_PASSED");
}
