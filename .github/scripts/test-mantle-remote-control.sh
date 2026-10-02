#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../codex-rs"
log_dir="${RUNNER_TEMP:-/tmp}/mantle-validation"
mkdir -p "$log_dir"
status=0
check() {
    local name="$1"
    shift
    echo "::group::$name"
    if "$@" 2>&1 | tee "$log_dir/$name.log"; then
        printf '%s PASS\n' "$name" | tee -a "$log_dir/results.txt"
    else
        printf '%s FAIL\n' "$name" | tee -a "$log_dir/results.txt"
        status=1
    fi
    echo '::endgroup::'
}
check compile cargo check --locked --tests -p codex-app-server -p codex-cli -p codex-model-provider
check provider cargo test --locked -p codex-model-provider --lib amazon_bedrock
check host cargo test --locked -p codex-app-server --lib mantle
check account-inference cargo test --locked -p codex-app-server --test all suite::v2::mantle_remote_control
check relay cargo test --locked -p codex-app-server --test mantle_remote_control_relay
check cli cargo test --locked -p codex-cli --lib remote_control_cmd
check account-regression cargo test --locked -p codex-app-server --test all suite::v2::account
check remote-regression cargo test --locked -p codex-app-server --test all suite::v2::remote_control
check format cargo fmt --all -- --check
exit "$status"
