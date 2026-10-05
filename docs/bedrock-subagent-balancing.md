# Bedrock subagent account balancing

This feature allocates one AWS credential slot when an independent subagent starts.
It does not rotate an existing conversation between accounts. It works for native
Bedrock subagents across all Codex sessions, including nested agents. The
`amazon-bedrock` provider uses AWS Mantle; `amazon-bedrock-runtime` is a separate
provider. It has no AMMO mode, role-name condition, or reasoning-effort override.

## Configure once

Add the pool to the native provider's AWS table in `~/.codex/config.toml`. Merge it
with an existing table rather than adding a duplicate table.

```toml
# Native Bedrock reserves the default collaboration schemas for encrypted messages.
[features.multi_agent_v2]
enabled = true
tool_namespace = "agents"

[model_providers.amazon-bedrock.aws]
region = "us-east-1"
profile = "default" # Main agent's AWS profile; no special "chief" profile is needed.
subagent_profiles = ["dev", "bis", "ironfist"]
# Optional. Use absolute paths; one Bedrock API key per file.
# subagent_api_key_files = ["/absolute/path/to/bedrock-key"]
```

Only if you choose Bedrock Runtime, set `model_provider = "amazon-bedrock-runtime"`
and use `[model_providers.amazon-bedrock-runtime.aws]` instead. For Mantle, keep
`model_provider = "amazon-bedrock"` and the table shown above. The selected model,
endpoint, permissions, and region still need to be configured normally. ChatGPT
login, tool permissions, approvals, and sandbox policy stay unchanged.

## One account and the default profile

The pool is optional. With only `[default]` in `~/.aws/credentials`, set
`profile = "default"` and omit both pool lists. New subagents use the same AWS
account as the main agent. Without an explicit profile, the normal AWS credential
chain still applies; environment credentials can take precedence over `[default]`.
Profiles are not discovered and rotated unless they are listed in the pool.

If a configured pool has no available entries, a new subagent keeps its inherited
credentials. Existing agents with saved assignments keep those assignments: a
missing saved profile or key fails rather than switching to `default`. The
`agents` tool namespace above is still required for V2 messaging in this build,
even when no account pool is configured.

Profiles must map to distinct AWS accounts to add account-level quota capacity.
Every slot must authorize the selected model in the selected region/project.
The allocator discovers profile names, not account IDs or credential health.
Expired credentials and model access errors still produce normal auth errors.
Do not repoint a profile name or key-file path to another account after using it.
Credential renewal under the **same account** is allowed for these stable references.

Key files should have mode `600`. Do not put bearer tokens in the configuration.
A selected key file overrides ambient bearer credentials and profiles. A selected
profile uses the explicit SDK profile loader, not the chief's ambient bearer token
or static keys. Environment variables are not changed. This controls model API
requests, not AWS CLI commands an agent runs in its shell.

## Allocation and lifetime

Profile slots come first, then key-file slots. The allocator skips missing profiles
and invalid key files for new assignments. It deduplicates identical references,
not different aliases for the same account. A shared counter under
`$CODEX_HOME/bedrock-subagents` rotates across separate Codex processes that share
that home. Independent homes have independent counters.

The main session retains its account and consumes no slot. Each independent child,
including a nested child, consumes one slot. A thread UUID identifies its assignment.
The assignment is written before the model provider starts. Reloads, resumes, pool
reordering, and pool removal do not replace an existing assignment. Disabling the
pool stops new allocations; existing pins still apply. Unpooled explicit command
authentication and credential exporters remain authoritative.

The implementation uses an OS file lock and atomic file replacement. A process
crash releases the lock. A crash between the cursor and pin writes can skip one
slot, but cannot acknowledge an unpinned routed agent. Corrupt state or an unavailable
state directory fails startup rather than guessing another account. If all configured
slots are absent, a **new** agent retains and pins its inherited credentials.

Pin files store credential references, endpoint, region, and hashes of request scope.
They do not store bearer tokens or AWS secret keys. Preserve this directory when
moving a Codex home or restoring its history. Do not delete pins for conversations
you plan to resume. Ambient/managed credentials without a stable profile or key-file
reference use a fingerprint; changes can require restoring those credentials or
starting a fresh thread. Prefer stable profile/key-file references for long-lived work.

## Account affinity and forked history

AWS documents that stored Responses state is scoped to an AWS account's Project.
A `previous_response_id` cannot be reused across Projects. This fork of Codex also
requests encrypted reasoning and sends `store=false`; public storage documentation
alone does not establish the scope of every encrypted item. The implementation
therefore treats opaque history as account-bound conservatively.

Use the custom `agents` namespace shown above for native Bedrock. Its reserved
`collaboration` namespace requires encrypted message schemas and rejects these
portable messages. Merge this setting into an existing feature table if present.

Native Bedrock collaboration tools request plain-text task messages for spawning,
sending messages, and follow-up tasks. Protected task arguments are account-bound
and cannot be delivered to another account, even when `fork_turns="none"`. Explicitly
encrypted task arguments are rejected and the model must retry with plain text.
This does not remove encrypted reasoning from existing conversation history.
Task messages are readable to the local process and can appear in saved histories;
HTTPS still protects their transport. Each selected account receives its assigned
task data. This message format applies to native AWS V2 tools in this build even
when the pool is disabled. Custom tool schemas may need compatibility checks after
model updates; routing tests do not establish equivalent model quality or latency.

A fork containing reasoning, compaction, provider file references, unknown model
items, or reference-backed history keeps the source account. It does not consume
another slot. Known portable message-only forks may allocate a new account. The
allocator never drops encrypted state to make a fork portable. An offline fork can
read its source pin. If the owning account cannot be established, startup fails.

Once bound, requests and retries stay on that account. A missing pinned key/profile,
changed project configuration, or incompatible provider fails instead of failing
over. Profile/key-file references must retain the same actual account. Account IDs
are not checked through STS, so repointing references violates the affinity contract.

This is **spawn-time load distribution**, not a health-aware request proxy. It does
not promise higher availability, measure account load, or move a throttled existing
session. Fresh independent agents can use other slots. Existing agents must recover
on their original account or restart without account-bound state.

Legacy resumed subagents have no trustworthy allocation pin. With the pool enabled,
they fail rather than receive an arbitrary account. Resume them under their original
credentials with the pool disabled, or start fresh agents.

## Validation

```sh
cd codex-rs
cargo fmt --all
cargo run --locked -p codex-config-schema --bin codex-write-config-schema
cd ..
# Review formatting and schema changes before the check-only validation script.
bash scripts/test-bedrock-subagents.sh
```

Tests cover allocation, concurrent startup, nested agents, pool edits, persisted
resume, opaque-fork affinity, invalid slots, corrupt state, and secret-free pins.
A subprocess integration scenario uses real SDK profile loading and native request
signing with synthetic credentials. A core test exercises actual thread startup and
cold resume. The default-profile regression signs requests with only a synthetic
`[default]` profile, both with no pool and with an unavailable pool. These tests do
not send live inference requests or measure availability.

Sources:
- https://docs.aws.amazon.com/bedrock/latest/userguide/inference-responses-api.html
- `codex-rs/core/src/client.rs`: encrypted reasoning and `store=false` request setup.
- `cc-lb-wrapper.sh` supplied for this change: profile/key-file round-robin semantics.
