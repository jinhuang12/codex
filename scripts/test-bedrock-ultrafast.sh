#!/usr/bin/env bash
# Use local HTTP fixtures. No live AWS account or API key is required.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../codex-rs"
if [[ -n "${CODEX_SANDBOX_NETWORK_DISABLED+x}" ]]; then
    echo 'Local HTTP fixtures must be available; do not skip the request tests.' >&2
    exit 1
fi
log_dir=$(mktemp -d)
trap 'rm -rf "$log_dir"' EXIT
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
run_tests -p codex-model-provider
run_tests -p codex-core --test all bedrock_service_tier
run_tests -p codex-app-server --test all bedrock_service_tier
run_tests -p codex-tui --lib bedrock_catalog_tests
