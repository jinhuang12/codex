# ChatGPT Remote Control with Bedrock Mantle

This fork adds an opt-in host mode that uses ChatGPT for Remote Control identity and Amazon Bedrock Mantle for model inference. It does not replace the native Bedrock provider or introduce an inference proxy.

The setting is `remote_control_mantle = true`. The provider must be `amazon-bedrock`, which selects **Mantle**, not `amazon-bedrock-runtime`.

## Build and start

Build the fork, not an upstream release:

```sh
git clone --branch feat/mantle-remote-control https://github.com/jinhuang12/codex.git
cd codex/codex-rs
cargo build --locked --release -p codex-cli
./target/release/codex --version
```

The workspace may set a custom `CARGO_TARGET_DIR`. If so, use that directory for the version check. Use the resulting `codex` binary throughout these steps. Installing this CLI does not replace a binary bundled with the desktop app.

The build must report `0.162.0-alpha.7+mantle.1`, not `0.0.0`. The shipping phone client rejects a host that reports the development placeholder. The version comes from this fork's upstream base: `ca466061d64f0b44f416135c7fd06aa7af850bbc` is the parent of release commit `0f02e325f8fccbaa71a965b8efa15c224d10a8ba` (`rust-v0.162.0-alpha.7`), whose only change is the workspace version. The `+mantle.1` suffix identifies this fork build. Do not change the reported version of an older, incompatible host to bypass a client check.

Use a separate configuration directory for the first test:

```sh
export CODEX_HOME="$HOME/.codex-mantle-remote-control"
install -d -m 700 "$CODEX_HOME"
```

Save the following in `$CODEX_HOME/config.toml`. Put the top-level fields before TOML table headers. Replace the profile, region, and model with values your AWS account supports.

```toml
remote_control_mantle = true
model_provider = "amazon-bedrock"
model = "openai.gpt-6.1-sol"
model_reasoning_effort = "xhigh"
model_reasoning_summary = "none"
web_search = "disabled"
sandbox_mode = "workspace-write"
approval_policy = "on-request"

[model_providers.amazon-bedrock.aws]
profile = "your-bedrock-profile"
region = "us-east-1"
```

This model and region passed the live test described below. Model access still depends on the AWS account. An explicit AWS profile takes precedence over environment credentials. Authenticate the profile through your usual AWS process before starting the host.

The native Mantle catalog disables `reasoning.summary` for GPT-6.1 Sol. A phone can request `summary = "detailed"` for a turn, which overrides the configuration default. The model capability prevents that unsupported field from reaching Mantle. Setting `model_reasoning_summary = "none"` alone did not fix the phone test. No custom `model_catalog_json` file is required by this revision.

### Linux sandbox prerequisites

A bare Cargo build needs a system `bwrap` executable for sandboxed shell tools. On Ubuntu 24.04, AppArmor can also deny its user namespace. Install `bubblewrap` and the distribution's `apparmor-profiles` package through the host's package-management process. Check the package changes before installation.

If AppArmor denies `bwrap`, install its vendor profile without replacing an existing local profile:

```sh
if [ ! -e /etc/apparmor.d/bwrap-userns-restrict ]; then
  sudo install -m 0644 /usr/share/apparmor/extra-profiles/bwrap-userns-restrict \
    /etc/apparmor.d/bwrap-userns-restrict
fi
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
bwrap --unshare-user --ro-bind / / -- /bin/true
```

The final command must exit successfully before testing a sandboxed shell tool. Keep `kernel.apparmor_restrict_unprivileged_userns` enabled. Do not disable the sandbox to work around this error. The live host used the vendor profile and kept this restriction set to `1`.

### Sign in and pair

Sign into ChatGPT with the **fork binary**, using the same `CODEX_HOME`:

```sh
/path/to/fork/codex login --device-auth
/path/to/fork/codex remote-control --pair
```

Complete device sign-in with the ChatGPT account used by the phone. If that account does not allow device sign-in, use the browser flow with `codex login` on a host where its callback is reachable.

The `--pair` flag requests and prints a short-lived manual pairing code from this foreground host. The command stays attached to the terminal. With `--json --pair`, pairing data appears in the same JSON output object. Stop it with Ctrl-C. This is the simplest way to verify that the running host is the fork.

Daemon operation requires a complete packaged fork, not only a bare `cargo build` executable. A managed daemon may retain a previously installed upstream package or update it independently. Install and pin the fork package through the daemon package workflow, confirm its reported path and version, and save all settings in `config.toml` before using:

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
    "account": {"type": "amazonBedrock", "usesCodexManagedCredentials": false},
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

The relay test also sends GPT-6.1 Sol a turn with `effort = "xhigh"` and `summary = "detailed"`. It checks that both model requests retain the effort and omit the summary field. The provider tests cover both the built-in catalog and a configured catalog that advertises summary support.

### Live test on 2026-10-02

The live test used an Ubuntu 24.04 x86-64 host, the shipping ChatGPT iOS app `1.2026.266`, a real ChatGPT account, and an AWS profile with Mantle access in `us-east-1`.

| Check | Observed result |
| --- | --- |
| Build provenance | PR source `c6e026bd9ca9b1465bbeb8e63e78524d351d411f`, stamped `0.162.0-alpha.7+mantle.c6e026bd9ca9` |
| Separate accounts | `account/read` reported ChatGPT control identity and `amazon-bedrock` inference with `requiresOpenaiAuth = false` |
| Pairing | The shipping phone client connected to the foreground fork through the hosted relay |
| Phone tool turn | GPT-6.1 Sol with `xhigh` ran `pwd` successfully and returned `MANTLE_PHONE_OK` |
| Effective turn summary | The turn used `detailed`; a one-field model-catalog override disabled the unsupported summary capability. The incoming client request was not captured. |
| Connection trace | During a later phone test, the same host process opened a TLS connection whose server name was `bedrock-mantle.us-east-1.api.aws` |

For the traced turn, the request started at 4:15:33 PM EDT, the AWS TLS handshake was captured at 4:15:34 PM, and the reply containing `MANTLE_TRACE_20261002` completed at 4:15:40 PM. The capture also saw `chatgpt.com` connections. It retained socket metadata and handshake hostnames; it did not decrypt request bodies or inspect billing records.

This revision includes the tested catalog override in the native provider and uses a stable fork version suffix. This fork also always omits `reasoning.summary` from model requests, regardless of model capability, config, or client turn overrides. Reasoning effort is preserved. The live results above describe the earlier binary plus the catalog override. They do not validate this later source revision.

This is a successful basic phone/tool test, not full production qualification. Live approval prompts, interruption, automatic restart, account revocation, and AWS request-log auditing remain unverified. Local relay tests cover reconnect and logout, but those tests do not replace live checks of those behaviors.

## Troubleshooting

- **The account still reports Bedrock as the top-level login:** check that the host runs the fork, reads the intended config, and has the opt-in setting enabled.
- **Mantle binding error:** a client/profile/config reload tried to change the captured provider definition. Restore it or restart with the intended configuration.
- **AWS access or region error:** fix the selected AWS credential source or model access. ChatGPT login does not grant AWS access.
- **The phone requires a newer host:** check the running fork's version. A `0.0.0` build is rejected even when its source is current; rebuild this branch and restart only that host.
- **Unsupported `reasoning.summary`:** rebuild with the request-level summary override and restart the host. The rebuilt fork omits this parameter even if a client or saved thread requests it.
- **`bwrap` missing or user namespace denied:** complete the Linux sandbox prerequisites above and rerun the sandbox check.
- **Remote Control remains unavailable in the shipping client:** distinguish host logs from client UI and hosted-service restrictions. This fork cannot change a closed client or grant server-side enrollment privileges.
