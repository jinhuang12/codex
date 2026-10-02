#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../codex-rs"
log_dir="${RUNNER_TEMP:-/tmp}/mantle-validation"
mkdir -p "$log_dir"
: > "$log_dir/results.txt"
# Tests use synthetic credentials and local services, never runner credentials.
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_PROFILE
unset AWS_BEARER_TOKEN_BEDROCK OPENAI_API_KEY CODEX_API_KEY
export AWS_EC2_METADATA_DISABLED=true AWS_REGION=us-east-1
status=0
check() {
    local name="$1" result=PASS
    shift
    echo "::group::$name"
    if "$@" 2>&1 | tee "$log_dir/$name.log"; then
        if [[ "${1:-}" == cargo && "${2:-}" == test ]] &&
            ! grep -Eq 'test result: ok\. [1-9][0-9]* passed;' "$log_dir/$name.log"; then
            echo "Validation filter did not execute any passing tests: $name" >&2
            result=FAIL
        fi
    else
        result=FAIL
    fi
    printf '%s %s\n' "$name" "$result" | tee -a "$log_dir/results.txt"
    if [[ "$result" == FAIL ]]; then
        status=1
    fi
    echo '::endgroup::'
}
check format cargo fmt --all -- --check
check compile cargo check --locked --tests -p codex-app-server -p codex-cli -p codex-model-provider
check provider cargo test --locked -p codex-model-provider --lib amazon_bedrock
check host cargo test --locked -p codex-app-server --lib mantle
check account-inference cargo test --locked -p codex-app-server --test all suite::v2::mantle_remote_control
check relay cargo test --locked -p codex-app-server --test mantle_remote_control_relay
check cli cargo test --locked -p codex-cli --bin codex remote_control_cmd
check login cargo test --locked -p codex-cli --lib login
check protocol cargo test --locked -p codex-app-server-protocol --lib
check transport cargo test --locked -p codex-app-server-transport --lib remote_control
check account-regression cargo test --locked -p codex-app-server --test all suite::v2::account
check remote-regression cargo test --locked -p codex-app-server --test all suite::v2::remote_control
exit "$status"
