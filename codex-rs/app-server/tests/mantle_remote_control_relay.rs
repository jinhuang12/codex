//! End-to-end host test with a real relay WebSocket and a separate mock Mantle endpoint.
//! All credentials are synthetic. This does not contact the hosted relay or AWS.
#![allow(clippy::expect_used)]

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use axum::Json;
use axum::Router;
use axum::extract::OriginalUri;
use axum::extract::State;
use axum::extract::ws::Message;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::HeaderMap;
use axum::routing::get;
use codex_app_server_protocol::RemoteControlPairingStartParams;
use codex_config::types::AuthCredentialsStoreMode;
use core_test_support::responses;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::task::AbortOnDropHandle;
use wiremock::MockServer;

const WAIT: Duration = Duration::from_secs(60);
const CONTROL_TOKEN: &str = "synthetic-chatgpt-control-token";
const AWS_TOKEN: &str = "synthetic-mantle-inference-token";
const RELAY_TOKEN: &str = "synthetic-relay-token";

#[derive(Clone)]
struct Control {
    sockets: mpsc::Sender<WebSocket>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl Control {
    fn record(&self, path: &str, headers: &HeaderMap) {
        self.requests.lock().expect("request log").push((
            path.to_string(),
            headers.get("authorization").and_then(|h| h.to_str().ok())
                .unwrap_or_default().to_string(),
        ));
    }
}

async fn control_http(
    State(control): State<Control>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Json<Value> {
    control.record(uri.path(), &headers);
    let body = if uri.path().ends_with("/accounts/check") {
        json!({"accounts": [{"id": "mantle-account", "workspace_backend_origin": "https://chatgpt.com",
            "account_routing_override": "NO_CONSTRAINT"}]})
    } else if uri.path().ends_with("/server/enroll") || uri.path().ends_with("/server/refresh") {
        json!({"server_id": "mantle-server", "environment_id": "mantle-environment",
            "remote_control_token": RELAY_TOKEN, "expires_at": "3026-05-22T12:34:56Z"})
    } else if uri.path().ends_with("/server/pair") {
        json!({"server_id": "mantle-server", "environment_id": "mantle-environment",
            "pairing_code": "synthetic-pairing-code", "manual_pairing_code": "123456",
            "expires_at": "3026-05-22T12:34:56Z"})
    } else if uri.path().ends_with("/server/pair/status") {
        json!({"claimed": true})
    } else {
        json!({})
    };
    Json(body)
}

async fn control_socket(
    State(control): State<Control>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> axum::response::Response {
    control.record("/websocket", &headers);
    upgrade.on_upgrade(move |socket| async move {
        let _ = control.sockets.send(socket).await;
    })
}

struct Relay {
    socket: WebSocket,
    next_seq: u64,
    pending: Vec<Value>,
}

impl Relay {
    async fn send(&mut self, message: Value) -> Result<()> {
        self.socket.send(Message::Text(json!({
            "type": "client_message", "client_id": "mantle-client", "stream_id": "mantle-stream",
            "seq_id": self.next_seq, "cursor": format!("cursor-{}", self.next_seq), "message": message,
        }).to_string().into())).await?;
        self.next_seq += 1;
        Ok(())
    }

    async fn matching(&mut self, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        timeout(WAIT, async {
            loop {
                if let Some(index) = self.pending.iter().position(&predicate) {
                    return Ok(self.pending.remove(index));
                }
                let message = self.socket.next().await.context("relay disconnected")??;
                if let Message::Text(text) = message {
                    let envelope: Value = serde_json::from_str(&text)?;
                    if envelope["type"] == "server_message" {
                        self.socket.send(Message::Text(json!({
                            "type": "ack", "client_id": "mantle-client", "stream_id": "mantle-stream",
                            "seq_id": envelope["seq_id"],
                        }).to_string().into())).await?;
                        self.pending.push(envelope["message"].clone());
                    }
                }
            }
        }).await?
    }

    async fn response(&mut self, id: i64) -> Result<Value> {
        self.matching(|message| message["id"] == id && message.get("method").is_none()).await
    }

    async fn closed(&mut self) -> Result<()> {
        timeout(WAIT, async {
            loop {
                match self.socket.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        }).await?;
        Ok(())
    }
}

#[tokio::test]
async fn mantle_remote_pair_tool_turn_reconnect_resume_and_logout() -> Result<()> {
    let home = TempDir::new()?;
    let mantle = MockServer::start().await;
    let model_requests = responses::mount_response_sequence(&mantle, vec![
        responses::sse_response(responses::sse(vec![
            responses::ev_response_created("mantle-1"),
            responses::ev_function_call("echo-call", "echo", "{}"),
            responses::ev_completed("mantle-1"),
        ])),
        responses::sse_response(responses::sse(vec![
            responses::ev_response_created("mantle-2"),
            responses::ev_assistant_message("answer", "tool completed on Mantle"),
            responses::ev_completed("mantle-2"),
        ])),
    ]).await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let control_url = format!("http://{}", listener.local_addr()?);
    let (sockets, mut socket_rx) = mpsc::channel(4);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let control = Control { sockets, requests: requests.clone() };
    let router = Router::new()
        .route("/backend-api/wham/remote/control/server", get(control_socket))
        .fallback(control_http)
        .with_state(control);
    let _server = AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, router).await
    }));
    std::fs::write(home.path().join("config.toml"), format!(r#"
remote_control_mantle = true
model_provider = "amazon-bedrock"
model = "openai.gpt-5.6-luna"
chatgpt_base_url = "{control_url}/backend-api"
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
"#, mantle.uri()))?;
    write_chatgpt_auth(home.path(), ChatGptAuthFixture::new(CONTROL_TOKEN)
        .account_id("mantle-account").chatgpt_account_id("mantle-account")
        .chatgpt_user_id("mantle-user").plan_type("pro"), AuthCredentialsStoreMode::File)?;
    let mut app = TestAppServer::builder().with_codex_home(home.path()).without_auto_env()
        .with_env_overrides(&[("AWS_BEARER_TOKEN_BEDROCK", Some(AWS_TOKEN)),
            ("AWS_PROFILE", None), ("OPENAI_API_KEY", None), ("CODEX_API_KEY", None)])
        .build_initialized_with_timeout(WAIT).await?;
    let id = app.send_remote_control_ephemeral_enable_request().await?;
    let _: Value = timeout(WAIT, app.read_response(id)).await??;
    let socket = timeout(WAIT, socket_rx.recv()).await?.context("relay did not connect")?;
    let id = app.send_remote_control_pairing_start_request(RemoteControlPairingStartParams {
        manual_code: true,
    }).await?;
    let pairing: Value = timeout(WAIT, app.read_response(id)).await??;
    assert_eq!(pairing["manualPairingCode"], "123456");

    let mut relay = Relay { socket, next_seq: 0, pending: Vec::new() };
    relay.send(json!({"id": 1, "method": "initialize", "params": {
        "clientInfo": {"name": "mantle-remote-test", "version": "0.1.0"},
        "capabilities": {"experimentalApi": true},
    }})).await?;
    assert!(relay.response(1).await?.get("result").is_some());
    relay.send(json!({"method": "initialized"})).await?;
    relay.send(json!({"id": 2, "method": "account/read", "params": {}})).await?;
    let account = relay.response(2).await?;
    assert_eq!(account["result"]["account"]["type"], "chatgpt");
    assert_eq!(account["result"]["inference"]["modelProvider"], "amazon-bedrock");
    assert_eq!(account["result"]["inference"]["requiresOpenaiAuth"], false);
    relay.send(json!({"id": 3, "method": "thread/start", "params": {
        "dynamicTools": [{"name": "echo", "description": "Echo a test result",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}],
    }})).await?;
    let thread = relay.response(3).await?;
    assert_eq!(thread["result"]["modelProvider"], "amazon-bedrock");
    let thread_id = thread["result"]["thread"]["id"].clone();
    relay.send(json!({"id": 4, "method": "turn/start", "params": {
        "threadId": thread_id, "input": [{"type": "text", "text": "Use echo and report the result.", "textElements": []}],
    }})).await?;
    assert!(relay.response(4).await?.get("result").is_some());
    let tool = relay.matching(|message| message["method"] == "item/tool/call").await?;
    relay.send(json!({"id": tool["id"], "result": {
        "contentItems": [{"type": "inputText", "text": "echoed-over-remote-control"}], "success": true,
    }})).await?;
    let completed = relay.matching(|message| message["method"] == "turn/completed").await?;
    assert_eq!(completed["params"]["turn"]["status"], "completed");

    // Reconnect the physical transport without replacing its authenticated owner or thread route.
    let next_seq = relay.next_seq;
    relay.socket.send(Message::Close(None)).await?;
    drop(relay);
    let socket = timeout(WAIT, socket_rx.recv()).await?.context("relay did not reconnect")?;
    let mut relay = Relay { socket, next_seq, pending: Vec::new() };
    relay.send(json!({"id": 5, "method": "thread/resume", "params": {"threadId": thread_id}})).await?;
    assert_eq!(relay.response(5).await?["result"]["modelProvider"], "amazon-bedrock");
    relay.send(json!({"id": 6, "method": "thread/start", "params": {
        "modelProvider": "openai", "config": {"remote_control_mantle": false},
    }})).await?;
    assert!(relay.response(6).await?["error"]["message"].as_str().context("expected route error")?.contains("Mantle"));

    let id = app.send_logout_account_request().await?;
    let _: Value = timeout(WAIT, app.read_response(id)).await??;
    relay.closed().await?;
    let observed = mantle.received_requests().await.context("model request log")?;
    let inference: Vec<_> = observed.iter().filter(|r| r.url.path().ends_with("/responses")).collect();
    assert_eq!(inference.len(), 2);
    for request in &inference {
        assert_eq!(request.headers.get("authorization").context("AWS auth")?.to_str()?, format!("Bearer {AWS_TOKEN}"));
        assert!(!request.headers.contains_key("chatgpt-account-id"));
        assert!(!format!("{:?}", request.headers).contains(CONTROL_TOKEN));
    }
    assert!(String::from_utf8_lossy(&inference[1].body).contains("echoed-over-remote-control"));
    drop(model_requests);
    let control_requests = requests.lock().expect("control request log");
    assert!(control_requests.iter().any(|(path, token)| path.ends_with("/server/enroll") && token == &format!("Bearer {CONTROL_TOKEN}")));
    assert!(control_requests.iter().any(|(path, token)| path == "/websocket" && token == &format!("Bearer {RELAY_TOKEN}")));
    assert!(control_requests.iter().all(|(path, token)| !path.ends_with("/responses") && !token.contains(AWS_TOKEN)));
    Ok(())
}
