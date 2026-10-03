use super::*;
use codex_model_provider_info::AwsCredentialExportConfig;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;

fn settings(values: &[(&str, &str)]) -> Settings {
    Settings::from_env(|key| {
        values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.to_string())
    })
}

fn make_policy(values: &[(&str, &str)], available: &[&str]) -> TeammatePolicy {
    TeammatePolicy {
        settings: settings(values),
        available: OnceCell::from(
            available
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        ),
        assignments: Mutex::default(),
    }
}

fn child(path: &str, depth: i32, role: &str) -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth,
        agent_path: Some(AgentPath::try_from(path).expect("valid test path")),
        agent_nickname: Some("nickname".to_string()),
        agent_role: Some(role.to_string()),
    })
}

fn bedrock() -> ModelProviderInfo {
    ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
        profile: Some("chief".to_string()),
        region: Some("us-west-2".to_string()),
        credential_export: None,
        auth_refresh: None,
    }))
}

#[test]
fn mode_and_enable_defaults_match_wrapper() {
    let no_mode = settings(&[]);
    assert!(no_mode.profiles.is_empty());
    assert!(no_mode.champions.is_empty());
    let ssh = settings(&[("AMMO_MODE", "ssh")]);
    assert_eq!(ssh.profiles, csv("ammo1,ammo2,ammo3,ammo4"));
    assert_eq!(ssh.champions, csv("red-champ,blue-champ"));
    assert_eq!(ssh.effort, Some(ReasoningEffort::XHigh));
    for mode in ["docker", "local"] {
        let local = settings(&[("AMMO_MODE", mode)]);
        assert!(local.profiles.is_empty());
        assert_eq!(local.champions, ssh.champions);
    }
    let disabled = settings(&[
        ("AMMO_MODE", "ssh"),
        ("AMMO_LB_ENABLE", "0"),
        ("AMMO_ARM_ENABLE", "0"),
    ]);
    assert!(disabled.profiles.is_empty());
    assert!(disabled.champions.is_empty());
}

#[test]
fn custom_lists_and_claude_effort_alias() {
    let custom = settings(&[
        ("AMMO_MODE", "local"),
        ("AMMO_LB_MODES", " ssh, local "),
        ("AMMO_LB_PROFILES", " a, b, ,a "),
        ("AMMO_ARM_AGENT_TYPES", " leader "),
        ("AMMO_ARM_EFFORT", "ultrahigh"),
    ]);
    assert_eq!(custom.profiles, csv("a,b"));
    assert_eq!(custom.champions, csv("leader"));
    assert_eq!(custom.effort, Some(ReasoningEffort::XHigh));
}

#[tokio::test]
async fn root_review_non_bedrock_and_missing_profiles_are_not_routed() {
    let policy = make_policy(&[("AMMO_MODE", "ssh")], &["a", "b"]);
    let original = bedrock();
    for source in [
        None,
        Some(SessionSource::Cli),
        Some(SessionSource::SubAgent(SubAgentSource::Review)),
    ] {
        let mut provider = original.clone();
        let mut effort = Some(ReasoningEffort::Low);
        policy
            .apply(&mut provider, &mut effort, source.as_ref())
            .await;
        assert_eq!(provider, original);
        assert_eq!(effort, Some(ReasoningEffort::Low));
    }
    let source = child("/root/worker", 1, "worker");
    let mut openai = ModelProviderInfo::create_openai_provider(None);
    let expected = openai.clone();
    policy.apply(&mut openai, &mut None, Some(&source)).await;
    assert_eq!(openai, expected);
    assert!(policy.assignments.lock().expect("lock").profiles.is_empty());
    let empty = make_policy(&[("AMMO_MODE", "ssh")], &[]);
    let mut provider = original.clone();
    empty.apply(&mut provider, &mut None, Some(&source)).await;
    assert_eq!(provider, original);
}

#[tokio::test]
async fn rotation_reload_and_nested_inheritance_preserve_region_and_parent() {
    let policy = make_policy(&[("AMMO_MODE", "ssh")], &["ammo1", "ammo3"]);
    let parent = bedrock();
    for (path, depth, expected) in [
        ("/root/red_champ", 1, "ammo1"),
        ("/root/blue_champ", 1, "ammo3"),
        ("/root/red_champ/worker", 2, "ammo1"),
        ("/root/red_champ", 1, "ammo1"),
        ("/root/third", 1, "ammo1"),
    ] {
        let mut provider = parent.clone();
        let source = child(path, depth, "worker");
        policy.apply(&mut provider, &mut None, Some(&source)).await;
        let aws = provider.aws.as_ref().expect("AWS config");
        assert_eq!(aws.profile.as_deref(), Some(expected));
        assert_eq!(aws.region.as_deref(), Some("us-west-2"));
        provider.aws.as_mut().expect("AWS config").profile = Some("chief".to_string());
        assert_eq!(provider, parent, "only the profile may change");
    }
    assert_eq!(
        parent.aws.expect("AWS config").profile.as_deref(),
        Some("chief")
    );
}

#[tokio::test]
async fn champion_matching_is_exact_and_only_direct_teammates_are_armed() {
    let policy = make_policy(&[("AMMO_MODE", "local")], &[]);
    for (path, depth, role, armed) in [
        ("/root/one", 1, "red-champ", true),
        ("/root/blue_champ", 1, "blue-champ", true),
        ("/root/red_champ_extra", 1, "worker", false),
        ("/root/red_champ/blue_champ", 2, "blue-champ", false),
        ("/root/ordinary", 1, "worker", false),
    ] {
        let mut provider = bedrock();
        let original = provider.clone();
        let mut effort = Some(ReasoningEffort::Low);
        policy
            .apply(&mut provider, &mut effort, Some(&child(path, depth, role)))
            .await;
        assert_eq!(
            effort,
            Some(if armed {
                ReasoningEffort::XHigh
            } else {
                ReasoningEffort::Low
            })
        );
        assert_eq!(provider, original, "local mode does not route by default");
    }
}

#[tokio::test]
async fn explicit_credential_export_is_not_overridden() {
    let policy = make_policy(&[("AMMO_MODE", "ssh")], &["ammo1"]);
    let mut provider = bedrock();
    let aws = provider.aws.as_mut().expect("AWS config");
    aws.profile = None;
    aws.credential_export = Some(AwsCredentialExportConfig {
        command: "export-credentials".to_string(),
        args: Vec::new(),
        timeout_ms: NonZeroU64::new(1000).expect("non-zero"),
    });
    let original = provider.clone();
    policy
        .apply(
            &mut provider,
            &mut None,
            Some(&child("/root/worker", 1, "worker")),
        )
        .await;
    assert_eq!(provider, original);
    assert!(policy.assignments.lock().expect("lock").profiles.is_empty());
}

#[test]
fn concurrent_teammates_are_balanced_and_independent_trees_start_fresh() {
    let policy = TeammatePolicy::default();
    let profiles = csv("a,b,c,d");
    let selected = std::thread::scope(|scope| {
        let handles = (0..128)
            .map(|index| {
                let policy = &policy;
                let profiles = &profiles;
                scope.spawn(move || {
                    policy
                        .select_profile(&index.to_string(), true, profiles, None)
                        .expect("profile")
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect::<Vec<_>>()
    });
    for profile in &profiles {
        assert_eq!(
            selected
                .iter()
                .filter(|selected| *selected == profile)
                .count(),
            32
        );
    }
    assert_eq!(
        TeammatePolicy::default().select_profile("new", true, &profiles, None),
        Some("a".to_string())
    );
    assert_eq!(
        policy.select_profile("unknown descendant", false, &profiles, None),
        None
    );
}

#[test]
fn poisoned_assignment_lock_fails_open() {
    let policy = TeammatePolicy::default();
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let _guard = policy.assignments.lock().expect("lock");
            panic!("simulate failed assignment");
        });
        assert!(handle.join().is_err());
    });
    assert_eq!(
        policy.select_profile("worker", true, &csv("a,b"), None),
        None
    );
}

#[test]
fn legacy_worker_remembers_inherited_profile_on_cold_reload_without_rotating() {
    let policy = TeammatePolicy::default();
    let profiles = csv("ammo1,ammo3");
    assert_eq!(
        policy.select_profile("name:champion", true, &profiles, None),
        Some("ammo1".to_string())
    );
    assert_eq!(
        policy.select_profile("name:worker", false, &profiles, Some("ammo1")),
        Some("ammo1".to_string())
    );
    assert_eq!(
        policy.select_profile("name:worker", false, &profiles, Some("chief")),
        Some("ammo1".to_string())
    );
    assert_eq!(
        policy.select_profile("name:other", true, &profiles, None),
        Some("ammo3".to_string())
    );
    assert_eq!(
        policy.select_profile("name:unknown", false, &profiles, Some("chief")),
        None
    );
}

#[tokio::test]
async fn native_agent_names_match_only_when_explicitly_configured() {
    let policy = make_policy(
        &[
            ("AMMO_MODE", "local"),
            ("AMMO_ARM_AGENT_TYPES", "red_champ,blue_champ"),
        ],
        &[],
    );
    for (path, armed) in [
        ("/root/red_champ", true),
        ("/root/blue_champ", true),
        ("/root/red_champ_extra", false),
    ] {
        let mut provider = bedrock();
        let mut effort = Some(ReasoningEffort::Low);
        policy
            .apply(&mut provider, &mut effort, Some(&child(path, 1, "worker")))
            .await;
        assert_eq!(
            effort,
            Some(if armed {
                ReasoningEffort::XHigh
            } else {
                ReasoningEffort::Low
            })
        );
    }
}
