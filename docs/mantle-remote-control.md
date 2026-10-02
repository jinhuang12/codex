# ChatGPT Remote Control with Bedrock Mantle

This fork adds an opt-in host mode that uses ChatGPT for Remote Control identity and Amazon Bedrock Mantle for model inference. It does not replace the native Bedrock provider or introduce an inference proxy.

The setting is `remote_control_mantle = true`. The provider must be `amazon-bedrock`, which selects **Mantle**, not `amazon-bedrock-runtime`.

## Build and start

Build the fork, not an upstream release:

```sh
git clone --branch feat/mantle-remote-control https://github.com/jinhuang12/codex.git
cd codex/codex-rs
cargo build --locked --release -p codex-cli
```

The workspace may set a custom `CARGO_TARGET_DIR`. Use the resulting `codex` binary throughout these steps. Do not assume that installing this CLI replaces a binary bundled with the desktop app.

Save the following in the host's `$CODEX_HOME/config.toml` (normally `~/.codex/config.toml`). Put the top-level fields before TOML table headers. Replace the profile, region, and model with values your AWS account supports.

```toml
remote_control_mantle = true
model_provider = "amazon-bedrock"
model = "openai.gpt-5.6-luna"
web_search = "disabled"

[model_providers.amazon-bedrock.aws]
profile = "your-bedrock-profile"
region = "us-east-1"
```

The model above is an example from this source revision's Mantle catalog. Model access and regional availability still depend on AWS. An explicit AWS profile takes precedence over environment credentials. Authenticate the profile through your usual AWS process before starting the host.

Sign into ChatGPT with the **fork binary**, using the same `CODEX_HOME`:

```sh
/path/to/fork/codex login
/path/to/fork/codex remote-control
```

The foreground command prints the pairing information and stays attached to the terminal. Stop it with Ctrl-C. This is the simplest way to verify that the running host is the fork.

For daemon operation, save all settings in `config.toml`, then use:

```sh
/path/to/fork/codex remote-control start
/path/to/fork/codex remote-control pair
/path/to/fork/codex remote-control stop
```

Stop any old daemon before starting a different build. Daemon subcommands reject `-c` overrides because their lifecycle path does not pass those overrides to the daemon. Foreground operation supports overrides and rejects invalid configuration instead of silently loading provider defaults.

A desktop client must launch or connect to this fork's app server. A separately installed upstream desktop binary may keep its own provider/UI restriction. This repository contains the open-source host, not the hosted relay or the distributed ChatGPT mobile/desktop clients.

## Credentials

There are two independent credential purposes:

| Purpose | Credential | Destination |
| --- | --- | --- |
| Account, workspace policy, enrollment, pairing | Real ChatGPT login | Existing OpenAI account and Remote Control services |
| Inference | Native AWS profile, SDK credentials, bearer-token environment, or configured credential command | Bedrock Mantle |

This mode reserves Codex's managed login store for ChatGPT. It rejects managed Bedrock-key and OpenAI API-key login operations that would replace that identity. It does not store two managed logins in `auth.json`. For a Bedrock bearer token, provide `AWS_BEARER_TOKEN_BEDROCK` in the host process environment and omit an explicit AWS profile. Never put secret tokens into tracked configuration, issue comments, or command-line arguments.

An AWS credential error fails the model operation. It does not switch inference to OpenAI. The native provider fixes its credential-source selection for its lifetime. Refresh can obtain new temporary credentials from that same source; editing an external AWS profile can still change the principal that source resolves to.

ChatGPT logout closes the old Remote Control authority through the existing transport ownership checks. It leaves the Mantle provider configuration in place. Reconnect or restart after signing into a different account. Signing out of ChatGPT is not an AWS logout and does not cancel every local model operation that was already running.

## Account protocol

With the mode disabled, the existing account response is unchanged and the new optional field is omitted.

With the mode enabled, `account/read` describes the control account separately from inference:

```json
{
  "account": {"type": "chatgpt", "email": "user@example.com", "planType": "pro"},
  "requiresOpenaiAuth": true,
  "inference": {
    "modelProvider": "amazon-bedrock",
    "account": {"type": "amazonBedrock", "authMode": "awsSdk"},
    "requiresOpenaiAuth": false
  }
}
```

The exact inference account fields depend on the native provider's account representation. Read the generated `InferenceAccount` and `Account` schemas for authoritative types. The legacy auth-status RPC never exports a ChatGPT access token in this mode, even when its caller requests a token. OpenAI inference rate-limit reporting is unavailable because it would not describe Bedrock usage.

## Routing and policy

At startup, the host captures the Mantle provider definition. New, resumed, and forked thread configuration must preserve the provider, endpoint, region, credential selector, and enabled mode. Model changes within that provider remain possible. A client cannot disable this mode and select OpenAI in the same request. Restart the host after an intentional provider-definition change.

The native provider supplies inference authentication, model catalogs, and background-model selection. Existing tool approvals, sandbox restrictions, account-owner checks, workspace routing, and managed policy remain in force. An incompatible managed requirement fails rather than being bypassed.

This is **not** an AWS-only data-residency mode. OpenAI still handles account services and Remote Control transport, including commands and session events. External tools, MCP servers, and commands can make their own network requests; this setting does not turn those tools into Bedrock-only services.

## Tests and validation

No real credentials are included in tests. The regression suite uses separate local control and inference endpoints:

```sh
cd codex-rs
cargo test --locked -p codex-model-provider --lib amazon_bedrock
cargo test --locked -p codex-app-server --lib mantle
cargo test --locked -p codex-app-server --test all suite::v2::mantle_remote_control
cargo test --locked -p codex-app-server --test mantle_remote_control_relay
```

The relay test uses the actual host WebSocket transport. It exercises pairing, account reads, a model/tool/model exchange, reconnect, thread resume, an attempted provider escape, and logout. Other tests cover credential separation, failed AWS authentication, and configuration binding.

These tests are not a live acceptance test of the hosted relay, an AWS account, or the shipping ChatGPT client. Before production use, verify enrollment, pairing, a tool approval, interruption, reconnect, account revocation, and AWS request logs with the actual client and fork binary. Do not remove hosted authorization checks or claim compatibility from a local unit-test result alone.

## Troubleshooting

- **The account still reports Bedrock as the top-level login:** check that the host runs the fork, reads the intended config, and has the opt-in setting enabled.
- **Mantle binding error:** a client/profile/config reload tried to change the captured provider definition. Restore it or restart with the intended configuration.
- **AWS access or region error:** fix the selected AWS credential source or model access. ChatGPT login does not grant AWS access.
- **Remote Control remains unavailable in the shipping client:** distinguish host logs from client UI and hosted-service restrictions. This fork cannot change a closed client or grant server-side enrollment privileges.
