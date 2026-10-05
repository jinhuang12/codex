#!/usr/bin/env bash
# Run from any directory. No live AWS account or API key is required.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../codex-rs"
log_dir=$(mktemp -d)
trap 'rm -rf "$log_dir"' EXIT
# Isolate tests from a developer's live AWS credentials.
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_PROFILE
unset AWS_BEARER_TOKEN_BEDROCK OPENAI_API_KEY CODEX_API_KEY
export AWS_EC2_METADATA_DISABLED=true AWS_REGION=us-east-1
export AWS_CONFIG_FILE="$log_dir/aws-config"
export AWS_SHARED_CREDENTIALS_FILE="$log_dir/aws-credentials"
: > "$AWS_CONFIG_FILE"
: > "$AWS_SHARED_CREDENTIALS_FILE"
run_tests() {
    cargo test --locked "$@" 2>&1 | tee "$log_dir/test.log"
    grep -Eq 'test result: ok\. [1-9][0-9]* passed;' "$log_dir/test.log" || {
        echo 'The test command did not execute any passing tests.' >&2
        return 1
    }
}
cargo fmt --all -- --check
run_tests -p codex-model-provider-info
run_tests -p codex-model-provider
run_tests -p codex-core --lib session::bedrock_subagents::tests
run_tests -p codex-core --lib bedrock_subagent_startup_and_cold_resume_keep_account_binding
run_tests -p codex-core --lib bedrock_plaintext_messages
run_tests -p codex-core --lib multi_agent_v2_message_schemas_are_encrypted
run_tests -p codex-app-server --lib mantle_pool_accepts_host_pins_but_rejects_client_route_overrides
run_tests -p codex-app-server --test mantle_remote_control_relay
cargo check --locked -p codex-core --tests
cp core/config.schema.json "$log_dir/config.schema.json"
cargo run --locked -p codex-config-schema --bin codex-write-config-schema
cmp "$log_dir/config.schema.json" core/config.schema.json
