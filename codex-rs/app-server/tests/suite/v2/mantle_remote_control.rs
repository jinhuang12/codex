//! Public-RPC regression tests. Both services are local mocks; no cloud credentials are used.

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::mount_workspace_routing;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::GetAuthStatusResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const WAIT: Duration = Duration::from_secs(60);
const CONTROL_TOKEN: &str = "chatgpt-control-token-never-for-aws";
const AWS_TOKEN: &str = "mantle-token-never-for-chatgpt";

async fn app(home: &Path, mantle: &MockServer, control: &MockServer) -> Result<TestAppServer> {
    mount_workspace_routing(control).await;
    for endpoint in [
        "/backend-api/wham/config/bundle",
        "/backend-api/wham/settings/user",
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(control)
            .await;
    }
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
remote_control_mantle = true
model_provider = "amazon-bedrock"
model = "openai.gpt-5.6-luna"
chatgpt_base_url = "{}/backend-api"
cli_auth_credentials_store = "file"
web_search = "disabled"
[features]
plugins = false
shell_snapshot = false
[model_providers.amazon-bedrock]
base_url = "{}/v1"
request_max_retries = 0
stream_max_retries = 0
[model_providers.amazon-bedrock.aws]
region = "us-east-1"
"#,
            control.uri(),
            mantle.uri()
        ),
    )?;
    write_chatgpt_auth(
        home,
        ChatGptAuthFixture::new(CONTROL_TOKEN)
            .account_id("mantle-control-account")
            .chatgpt_account_id("mantle-control-account")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    TestAppServer::builder()
        .with_codex_home(home)
        .without_auto_env()
        .with_env_overrides(&[
            ("AWS_BEARER_TOKEN_BEDROCK", Some(AWS_TOKEN)),
            ("AWS_PROFILE", None),
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
        ])
        .build_initialized_with_timeout(WAIT)
        .await
}

async fn read_account(app: &mut TestAppServer) -> Result<GetAccountResponse> {
    let id = app.send_request("account/read", Some(json!({}))).await?;
    Ok(timeout(WAIT, app.read_response(id)).await??)
}

async fn run_turn(app: &mut TestAppServer) -> Result<Value> {
    let id = app.send_request("thread/start", Some(json!({}))).await?;
    let thread: Value = timeout(WAIT, app.read_response(id)).await??;
    assert_eq!(thread["modelProvider"], "amazon-bedrock");
    let id = app
        .send_request(
            "turn/start",
            Some(json!({
                "threadId": thread["thread"]["id"],
                "input": [{"type": "text", "text": "Reply with hello.", "textElements": []}],
            })),
        )
        .await?;
    let _: Value = timeout(WAIT, app.read_response(id)).await??;
    Ok(timeout(WAIT, app.read_notification("turn/completed")).await??)
}

#[tokio::test]
async fn mantle_control_identity_and_inference_credentials_stay_separate() -> Result<()> {
    let home = TempDir::new()?;
    let mantle = MockServer::start().await;
    let control = MockServer::start().await;
    responses::mount_response_sequence(
        &mantle,
        vec![responses::sse_response(responses::sse(vec![
            responses::ev_response_created("mantle-response"),
            responses::ev_assistant_message("mantle-message", "hello from Mantle"),
            responses::ev_completed("mantle-response"),
        ]))],
    )
    .await;
    let mut app = app(home.path(), &mantle, &control).await?;
    let account = read_account(&mut app).await?;
    assert!(matches!(account.account, Some(Account::Chatgpt { .. })));
    assert!(account.requires_openai_auth);
    let inference = account.inference.expect("independent inference account");
    assert_eq!(inference.model_provider, "amazon-bedrock");
    assert!(!inference.requires_openai_auth);
    assert_eq!(
        inference.account,
        Some(Account::AmazonBedrock {
            uses_codex_managed_credentials: false
        })
    );

    let id = app
        .send_request("getAuthStatus", Some(json!({"includeToken": true})))
        .await?;
    let status: GetAuthStatusResponse = timeout(WAIT, app.read_response(id)).await??;
    assert!(status.auth_method.is_some());
    assert_eq!(status.auth_token, None);
    assert_eq!(run_turn(&mut app).await?["turn"]["status"], "completed");

    let requests = mantle.received_requests().await.expect("recorded requests");
    assert!(!requests.is_empty());
    for request in &requests {
        if request.url.path().ends_with("/responses") {
            assert_eq!(
                request.headers.get("authorization").unwrap().to_str()?,
                format!("Bearer {AWS_TOKEN}")
            );
            assert!(!request.headers.contains_key("chatgpt-account-id"));
            assert!(!format!("{:?}", request.headers).contains(CONTROL_TOKEN));
            assert!(!String::from_utf8_lossy(&request.body).contains(CONTROL_TOKEN));
        }
    }
    for request in control.received_requests().await.unwrap() {
        assert!(!request.url.path().ends_with("/responses"));
        assert!(!format!("{:?}", request.headers).contains(AWS_TOKEN));
    }
    Ok(())
}

#[tokio::test]
async fn mantle_auth_failure_does_not_fall_back_to_chatgpt_inference() -> Result<()> {
    let home = TempDir::new()?;
    let mantle = MockServer::start().await;
    let control = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "expired AWS credential"}})),
        )
        .mount(&mantle)
        .await;
    let mut app = app(home.path(), &mantle, &control).await?;
    assert_eq!(run_turn(&mut app).await?["turn"]["status"], "failed");
    let account = read_account(&mut app).await?;
    assert!(matches!(account.account, Some(Account::Chatgpt { .. })));
    assert_eq!(account.inference.unwrap().model_provider, "amazon-bedrock");
    assert!(
        control
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| !request.url.path().ends_with("/responses"))
    );
    Ok(())
}

#[tokio::test]
async fn mantle_rejects_provider_override_and_destructive_managed_login() -> Result<()> {
    let home = TempDir::new()?;
    let mantle = MockServer::start().await;
    let control = MockServer::start().await;
    let mut app = app(home.path(), &mantle, &control).await?;
    let auth_before = std::fs::read(home.path().join("auth.json"))?;
    let id = app
        .send_request(
            "thread/start",
            Some(json!({
                "modelProvider": "openai", "config": {"remote_control_mantle": false},
            })),
        )
        .await?;
    let error = timeout(
        WAIT,
        app.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    assert!(error.error.message.contains("Mantle"));
    let id = app
        .send_login_account_amazon_bedrock_request("must-not-replace-control", "us-east-1")
        .await?;
    let error = timeout(
        WAIT,
        app.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    assert!(error.error.message.contains("ChatGPT login"));
    assert_eq!(std::fs::read(home.path().join("auth.json"))?, auth_before);
    assert!(mantle.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn mantle_control_logout_keeps_aws_route_and_requests_new_control_login() -> Result<()> {
    let home = TempDir::new()?;
    let mantle = MockServer::start().await;
    let control = MockServer::start().await;
    let mut app = app(home.path(), &mantle, &control).await?;
    let config_before = std::fs::read(home.path().join("config.toml"))?;
    let id = app.send_logout_account_request().await?;
    let _: Value = timeout(WAIT, app.read_response(id)).await??;
    assert_eq!(
        std::fs::read(home.path().join("config.toml"))?,
        config_before
    );
    let account = read_account(&mut app).await?;
    assert_eq!(account.account, None);
    assert!(account.requires_openai_auth);
    assert_eq!(account.inference.unwrap().model_provider, "amazon-bedrock");
    Ok(())
}
