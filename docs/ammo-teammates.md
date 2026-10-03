# AMMO teammates

This is the native Codex equivalent of
[`ammo-server/scripts/teammate-cmd-wrapper.sh`](https://github.com/jinhuang12/ammo-server/blob/main/scripts/teammate-cmd-wrapper.sh).
There is no replacement CLI, subprocess wrapper, proxy, or shared counter file.
The local agent-tree runtime owns one small policy, applied after role settings
and before child provider construction, including fork and reload paths.

## Start

Configure native `amazon-bedrock` (including Mantle Remote Control) normally and
make the selected named AWS profiles available in the usual shared AWS files.
For a local terminal:

```sh
AMMO_MODE=local AMMO_LB_MODES=local \
AMMO_LB_PROFILES=ammo1,ammo2,ammo3,ammo4 codex
```

For an AMMO SSH deployment, `AMMO_MODE=ssh` is enough with the default profile
names. Use Codex's existing custom agent definitions for `red-champ` and
`blue-champ`; this policy does not create roles or replace their instructions. Codex agent path
names use underscores, not hyphens: an agent named `red_champ` can use the
`red-champ` role. To match native names instead of roles, set
`AMMO_ARM_AGENT_TYPES=red_champ,blue_champ`; matching remains exact.

## Semantics

Direct teammates (depth 1) receive available profiles in configured round-robin
order. Missing profiles are skipped. Named teammates reuse their assignment on
reload within the same tree. Named nested workers use their top-level teammate's
assignment rather than consuming another slot; legacy unnamed workers retain the
parent-derived provider configuration. The chief is never modified. Each tree
starts its own counter: state is not shared across independent Codex processes or
persisted across a full application restart.

Selection sets only the child provider's `aws.profile`. Native Bedrock's explicit
profile credential provider takes precedence over ambient static AWS keys and
bearer tokens. It does not mutate `AWS_PROFILE` or unset process credentials, so
concurrent children and the chief remain isolated. Existing region, endpoint,
model, provider headers, and Remote Control/ChatGPT identity are preserved.
Explicit provider auth commands and credential exporters are not rerouted.

Only direct teammates with an exactly matching role, agent path name, or nickname
receive champion effort. The default is Codex `xhigh`; legacy AMMO `ultrahigh`
is translated to `xhigh`. An explicit native effort value can be supplied instead;
as with normal Codex settings, the selected model must support the requested effort.
Ordinary agents keep their inherited or role-specific effort.

Claude's `ULTRACODE`, `ULTRAREVIEW`, and `AUTO_MODE` switches are product-specific,
not portable flags. Codex uses its existing agent/review tools and configured
execution policy. This implementation **does not enable hidden features, bypass
approvals, increase agent limits, widen sandbox access, or grant new tools**.
Configure permitted delegation and review through existing Codex settings/roles.

## Controls

The existing AMMO environment interface is retained:

| Variable | Default | Effect |
| --- | --- | --- |
| `AMMO_MODE` | unset | Deployment mode; unset or unmatched means no policy. |
| `AMMO_LB_ENABLE` | `1` | Set `0` to disable profile routing. |
| `AMMO_LB_MODES` | `ssh` | Comma-separated routing modes. |
| `AMMO_LB_PROFILES` | `ammo1,ammo2,ammo3,ammo4` | Named profile pool in rotation order. |
| `AMMO_ARM_ENABLE` | `1` | Set `0` to disable champion effort override. |
| `AMMO_ARM_MODES` | `ssh,docker,local` | Comma-separated champion modes. |
| `AMMO_ARM_AGENT_TYPES` | `red-champ,blue-champ` | Exact role/name matches. |
| `AMMO_ARM_EFFORT` | `xhigh` | Native effort, with `ultrahigh` compatibility alias. |

Settings are captured when the agent tree starts. Profiles are discovered lazily
once per tree, using the AWS SDK's shared config/credentials loader, including
`AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE`. No credential validation call
or inference request is made during selection. Discovery errors, no matching
profiles, and unavailable assignment state leave inherited credentials intact.
An existing but expired/unusable profile still produces the normal AWS auth error;
this is routing, not health checking, failover, or a throttle-retry mechanism.
`CLAUDE_CODE_TEAMMATE_COMMAND`, `CLAUDE_CMD`, and `AMMO_LB_RR_DIR` are unnecessary
and are not read.

## Validation

```sh
bash .github/scripts/test-ammo-teammates.sh
```

The tests cover mode/enable controls, champion boundaries, concurrent rotation,
missing profiles, tree isolation, reload/nested behavior, fail-open locking, and
credential-export protection. An isolated subprocess loads real shared AWS files
containing synthetic credentials, routes two teammates, and concurrently signs
requests through Codex's actual Bedrock provider. It verifies distinct SigV4 keys,
no chief token leakage, and unchanged chief authentication. Requests are signed
locally, not sent to AWS. The script also runs the existing Bedrock regression
tests and compiles the core integration and its test targets.
