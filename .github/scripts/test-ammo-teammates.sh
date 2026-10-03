#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../codex-rs"
# No inference is sent. The integration test uses synthetic AWS files in an isolated child.
cargo fmt --all -- --check
cargo test --locked -p codex-model-provider --lib teammate::tests
cargo test --locked -p codex-model-provider --test teammate_routing
cargo test --locked -p codex-model-provider --lib amazon_bedrock
cargo check --locked -p codex-core --tests
