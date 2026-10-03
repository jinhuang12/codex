//! Per-tree translation of AMMO's Claude teammate launcher policy.
//!
//! Codex teammates share a process: select credentials on the child provider, never
//! by changing AWS_* in the process environment. Nested workers stay on their
//! direct teammate's account. This policy does not grant tools or permissions.

use codex_aws_auth::discover_aws_profiles;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::OnceCell;

#[derive(Default)]
struct Assignments {
    next: usize,
    profiles: HashMap<String, String>,
}

/// One instance per agent tree; clones of the tree runtime share it through Arc.
/// Settings are captured when the tree starts; profile discovery is lazy and local.
#[derive(Default)]
pub struct TeammatePolicy {
    settings: Settings,
    available: OnceCell<Vec<String>>,
    assignments: Mutex<Assignments>,
}

#[derive(Default)]
struct Settings {
    profiles: Vec<String>,
    champions: Vec<String>,
    effort: Option<ReasoningEffort>,
}

fn csv(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    for value in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_string());
        }
    }
    values
}

impl Settings {
    fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let get = |key, default: &str| {
            env(key)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let mode = get("AMMO_MODE", "");
        let enabled = |switch, modes, default_modes| {
            get(switch, "1") == "1" && csv(&get(modes, default_modes)).contains(&mode)
        };
        let profiles = if enabled("AMMO_LB_ENABLE", "AMMO_LB_MODES", "ssh") {
            csv(&get("AMMO_LB_PROFILES", "ammo1,ammo2,ammo3,ammo4"))
        } else {
            Vec::new()
        };
        let champions = if enabled("AMMO_ARM_ENABLE", "AMMO_ARM_MODES", "ssh,docker,local") {
            csv(&get("AMMO_ARM_AGENT_TYPES", "red-champ,blue-champ"))
        } else {
            Vec::new()
        };
        let effort = match get("AMMO_ARM_EFFORT", "xhigh").as_str() {
            // Keep existing AMMO deployments usable without forwarding a Claude-only value.
            "ultrahigh" => Some(ReasoningEffort::XHigh),
            value => value.parse().ok(),
        };
        Self {
            profiles,
            champions,
            effort,
        }
    }
}

impl TeammatePolicy {
    pub fn from_env() -> Self {
        Self {
            settings: Settings::from_env(|name| std::env::var(name).ok()),
            ..Self::default()
        }
    }

    /// Apply after role overrides and before constructing a child model provider.
    /// A named teammate keeps its assignment across eviction/reload in this tree.
    pub async fn apply(
        &self,
        provider: &mut ModelProviderInfo,
        effort: &mut Option<ReasoningEffort>,
        source: Option<&SessionSource>,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            depth,
            agent_path,
            agent_nickname,
            agent_role,
            ..
        })) = source
        else {
            return;
        };
        if *depth < 1 {
            return;
        }
        if *depth == 1
            && [
                agent_role.as_deref(),
                agent_path.as_ref().map(|path| path.name()),
                agent_nickname.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|name| {
                self.settings
                    .champions
                    .iter()
                    .any(|champion| champion == name)
            })
            && let Some(champion_effort) = &self.settings.effort
        {
            *effort = Some(champion_effort.clone());
            tracing::info!(agent_role, agent_nickname, effort = %champion_effort, "AMMO champion effort");
        }
        // Explicit credential commands take precedence over profiles in the provider.
        // Do not silently override them or create an invalid profile/export combination.
        if self.settings.profiles.is_empty()
            || !provider.is_amazon_bedrock()
            || provider.has_command_auth()
            || provider
                .aws
                .as_ref()
                .is_some_and(|aws| aws.credential_export.is_some())
        {
            return;
        }
        let key = agent_path
            .as_ref()
            .and_then(|path| path.as_str().strip_prefix("/root/"))
            .and_then(|path| path.split('/').next())
            .map(|name| format!("path:{name}"))
            .or_else(|| agent_nickname.as_ref().map(|name| format!("name:{name}")));
        let Some(key) = key else {
            // Legacy unnamed descendants already inherit their parent's provider config.
            return;
        };
        let profiles = self
            .available
            .get_or_init(|| async {
                match discover_aws_profiles().await {
                    Ok(available) => self
                        .settings
                        .profiles
                        .iter()
                        .filter(|name| available.iter().any(|profile| &profile.name == *name))
                        .cloned()
                        .collect(),
                    Err(error) => {
                        tracing::warn!(%error, "AMMO AWS profile discovery failed; keeping inherited credentials");
                        Vec::new()
                    }
                }
            })
            .await;
        let inherited = provider.aws.as_ref().and_then(|aws| aws.profile.as_deref());
        let Some(profile) = self.select_profile(&key, *depth == 1, profiles, inherited) else {
            return;
        };
        tracing::info!(agent = %key, %profile, "AMMO teammate AWS profile");
        provider
            .aws
            .get_or_insert_with(|| ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
                credential_export: None,
                auth_refresh: None,
            })
            .profile = Some(profile);
    }

    fn select_profile(
        &self,
        key: &str,
        direct_teammate: bool,
        profiles: &[String],
        inherited: Option<&str>,
    ) -> Option<String> {
        if profiles.is_empty() {
            return None;
        }
        // No I/O under the lock. Poisoning is fail-open, like a failed flock in the wrapper.
        let mut state = self.assignments.lock().ok()?;
        if let Some(profile) = state.profiles.get(key) {
            return Some(profile.clone());
        }
        let profile = if direct_teammate {
            let profile = profiles[state.next % profiles.len()].clone();
            state.next = (state.next + 1) % profiles.len();
            profile
        } else {
            // Remember a legacy worker's inherited account so a sender-driven cold
            // reload cannot replace it with the chief's provider configuration.
            inherited
                .filter(|profile| profiles.iter().any(|available| available == profile))?
                .to_string()
        };
        state.profiles.insert(key.to_string(), profile.clone());
        Some(profile)
    }
}

#[cfg(test)]
#[path = "teammate_tests.rs"]
mod tests;
