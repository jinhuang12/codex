# Use this fork's AWS features

This guide sets up ChatGPT Remote Control with AWS Mantle and optional AWS account balancing for subagents. Both features are on this fork's `main` branch.

## 1. Build the fork

Install Git. Install Rust with `rustup`. The repository selects the required Rust toolchain.

```sh
git clone --branch main https://github.com/jinhuang12/codex.git
cd codex/codex-rs
cargo build --locked --release -p codex-cli --target-dir target
export PATH="$PWD/target/release:$PATH"
codex --version
```

Use this terminal for the remaining commands. The explicit target directory makes the executable path above independent of `CARGO_TARGET_DIR`.

This builds the CLI and foreground Remote Control host. Desktop apps and IDE extensions can use their own bundled executable; installing this CLI does not replace those copies. For sandboxed shell tools on Linux, complete the [Linux sandbox prerequisites](mantle-remote-control.md#linux-sandbox-prerequisites).

## 2. Configure AWS Mantle

Use an AWS profile with access to the selected model and region. Authenticate it through your normal AWS process before starting Codex.

Use a separate Codex configuration directory:

```sh
export CODEX_HOME="$HOME/.codex-mantle"
install -d -m 700 "$CODEX_HOME"
```

Save this as `$CODEX_HOME/config.toml`. Change the model, region, and profile if needed. Keep the top-level settings before the table headers.

```toml
remote_control_mantle = true
model_provider = "amazon-bedrock"
model = "openai.gpt-6-astra"
model_reasoning_effort = "xhigh"
model_reasoning_summary = "none"
web_search = "disabled"
sandbox_mode = "workspace-write"
approval_policy = "on-request"

[features.multi_agent_v2]
enabled = true
tool_namespace = "agents"

[model_providers.amazon-bedrock.aws]
profile = "default"
region = "us-east-1"
```

`amazon-bedrock` selects **AWS Mantle**. `profile = "default"` selects your normal AWS default profile; no special main-agent profile is required. Keep `tool_namespace = "agents"` for this build's AWS subagent messages, even with only one account.

## 3. Connect through ChatGPT

Run these commands on the host, in the same terminal with the same `CODEX_HOME`:

```sh
codex login --device-auth
codex remote-control --pair
```

Complete sign-in with the ChatGPT account you use on your phone or desktop. Enter the displayed pairing code in that client's Remote Control connection flow. Keep the host command running; Ctrl-C stops it. If device sign-in is unavailable, use `codex login` where the browser callback can reach the host.

```text
ChatGPT client -> OpenAI Remote Control relay -> your Codex host -> AWS Mantle
```

ChatGPT handles login, pairing, commands, and session events. Model requests use AWS credentials and go to Mantle. This is not an AWS-only data path. An AWS authentication failure does not switch inference to OpenAI.

The fork omits `reasoning.summary` even if a client requests `detailed`. Setting it to `none` makes the configuration clear; reasoning effort still applies.

## 4. Add an optional subagent account pool

For local use without Remote Control, omit `remote_control_mantle` and run `codex`. Account balancing does not require a ChatGPT Remote Control login.

Add this field to the **existing** `[model_providers.amazon-bedrock.aws]` table above. Replace the example names with profiles that you have configured. Use distinct AWS accounts if you want separate account quotas.

```toml
subagent_profiles = ["account-a", "account-b"]
```

You can use API-key files instead, or add them to the profile pool. Use an absolute path to each file, with one raw Bedrock API key per file and file permissions set to `600`.

```toml
subagent_api_key_files = ["/absolute/path/account-a.key", "/absolute/path/account-b.key"]
```

Restart the host after changing its provider configuration. To use separate accounts, ask Codex:

> Start two independent subagents with `fork_turns="none"`. Give each a different small task and have each report back.

With a new pool of two profiles, the assignments follow this pattern:

```text
Main agent     -> default
New subagent 1 -> account-a -> resume on account-a
New subagent 2 -> account-b -> resume on account-b
New subagent 3 -> account-a -> resume on account-a
```

The pool rotates across independent new agents, including nested agents, that share the same `CODEX_HOME`. A resume or retry keeps the saved assignment. The default `fork_turns="all"` copies history; if that history contains encrypted reasoning or other account-bound state, the child stays on the parent's account. Use `none` for independent work that can use another account.

With only `[default]` in `~/.aws/credentials`, keep `profile = "default"` and omit both pool lists. New subagents use that same account. A missing saved profile or key causes an error instead of switching an existing agent to another account.

Keep `$CODEX_HOME/bedrock-subagents` with the session history. Keep each profile name and key-file path tied to its original AWS account; renewing credentials for that same account is allowed. The pool controls model requests, not AWS commands run by tools. It does not move an existing agent to another account when a request is throttled.

AWS subagent task messages use plain text inside the local process and saved history so they can cross accounts. HTTPS protects transport; each selected AWS account receives its assigned task data. Existing encrypted reasoning stays tied to its source account.

## More detail

- [Remote Control setup, credentials, and troubleshooting](mantle-remote-control.md)
- [Account balancing, saved assignments, and fork restrictions](bedrock-subagent-balancing.md)

Account balancing also supports the separate `amazon-bedrock-runtime` provider. That requires its matching AWS configuration table and a supported Runtime model. The Remote Control mode above requires Mantle.
