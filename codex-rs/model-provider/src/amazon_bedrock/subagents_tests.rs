use super::*;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use tempfile::TempDir;

#[test]
fn request_scope_is_stable_across_reloaded_hash_maps() {
    use std::collections::HashMap;
    let mut info = ModelProviderInfo::create_amazon_bedrock_provider(None);
    let entries: Vec<_> = (0..20)
        .map(|i| (format!("key-{i}"), format!("value-{i}").into()))
        .collect();
    info.http_headers = Some(entries.clone().into_iter().collect::<HashMap<_, _>>());
    info.query_params = Some(entries.clone().into_iter().collect::<HashMap<_, _>>());
    let expected = request_scope(&info);
    for _ in 0..30 {
        info.http_headers = Some(entries.clone().into_iter().rev().collect());
        info.query_params = Some(entries.clone().into_iter().rev().collect());
        assert_eq!(request_scope(&info), expected);
    }
}

fn context(id: ThreadId, parent: Option<ThreadId>, is_new: bool) -> BedrockSubagentContext {
    BedrockSubagentContext {
        thread_id: id,
        is_new,
        is_subagent: parent.is_some(),
        parent_id: parent,
        affinity_source: None,
        source_provider: None,
    }
}

fn key(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("synthetic-{name}-token\n")).expect("write token");
    path
}

fn provider(chief: PathBuf, pool: Vec<PathBuf>) -> ModelProviderInfo {
    ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
        api_key_file: Some(chief),
        subagent_api_key_files: pool,
        region: Some("us-east-1".to_string()),
        ..Default::default()
    }))
}

fn bound_key(info: &ModelProviderInfo) -> &Path {
    info.aws
        .as_ref()
        .expect("AWS settings")
        .api_key_file
        .as_deref()
        .expect("key file")
}

fn binding() -> Binding {
    Binding {
        version: 1,
        account: Account::Profile {
            name: "chief".to_string(),
        },
        rotated: false,
        runtime_endpoint: false,
        base_url: "https://bedrock-mantle.us-east-1.api.aws/openai/v1".to_string(),
        region: "us-east-1".to_string(),
        request_scope: "test-scope".to_string(),
    }
}

#[tokio::test]
async fn root_does_not_rotate_and_nested_independent_agents_do() {
    let tmp = TempDir::new().expect("home");
    let chief = key(tmp.path(), "chief");
    let a = key(tmp.path(), "a");
    let b = key(tmp.path(), "b");
    let info = provider(chief.clone(), vec![a.clone(), b.clone()]);
    let root = ThreadId::new();
    let root_info =
        route_bedrock_subagent(tmp.path(), context(root, None, true), info.clone(), None)
            .await
            .expect("root");
    assert_eq!(bound_key(&root_info), chief);
    assert_eq!(
        root_info, info,
        "root provider must retain its host-bound shape"
    );
    assert!(!state_dir(tmp.path()).join("cursor.json").exists());
    let first = ThreadId::new();
    let first_info = route_bedrock_subagent(
        tmp.path(),
        context(first, Some(root), true),
        info.clone(),
        None,
    )
    .await
    .expect("first");
    let nested = route_bedrock_subagent(
        tmp.path(),
        context(ThreadId::new(), Some(first), true),
        first_info.clone(),
        None,
    )
    .await
    .expect("nested");
    let other_session = route_bedrock_subagent(
        tmp.path(),
        context(ThreadId::new(), Some(ThreadId::new()), true),
        info,
        None,
    )
    .await
    .expect("other session");
    assert_eq!(bound_key(&first_info), a);
    assert_eq!(bound_key(&nested), b);
    assert_eq!(bound_key(&other_session), a);
}

#[tokio::test]
async fn resume_ignores_pool_reordering_removal_and_region_edits() {
    let tmp = TempDir::new().expect("home");
    let chief = key(tmp.path(), "chief");
    let a = key(tmp.path(), "a");
    let b = key(tmp.path(), "b");
    let info = provider(chief, vec![a.clone(), b.clone()]);
    let id = ThreadId::new();
    let parent = Some(ThreadId::new());
    route_bedrock_subagent(tmp.path(), context(id, parent, true), info.clone(), None)
        .await
        .expect("spawn");
    let mut changed = info;
    changed.aws.as_mut().expect("AWS").subagent_api_key_files = vec![b];
    changed.aws.as_mut().expect("AWS").region = Some("us-west-2".to_string());
    for disabled in [false, true] {
        if disabled {
            changed
                .aws
                .as_mut()
                .expect("AWS")
                .subagent_api_key_files
                .clear();
        }
        let restored = route_bedrock_subagent(
            tmp.path(),
            context(id, parent, false),
            changed.clone(),
            None,
        )
        .await
        .expect("resume");
        assert_eq!(bound_key(&restored), a);
        assert_eq!(
            restored.aws.as_ref().expect("AWS").region.as_deref(),
            Some("us-east-1")
        );
        assert!(restored.base_url.expect("endpoint").contains("us-east-1"));
    }
    assert_eq!(
        fs::read_to_string(state_dir(tmp.path()).join("cursor.json")).expect("cursor"),
        "1"
    );
}

#[tokio::test]
async fn missing_pinned_key_never_falls_back_to_available_account() {
    let tmp = TempDir::new().expect("home");
    let chief = key(tmp.path(), "chief");
    let a = key(tmp.path(), "a");
    let b = key(tmp.path(), "b");
    let info = provider(chief, vec![a.clone(), b]);
    let id = ThreadId::new();
    let parent = Some(ThreadId::new());
    route_bedrock_subagent(tmp.path(), context(id, parent, true), info.clone(), None)
        .await
        .expect("spawn");
    fs::remove_file(a).expect("remove assigned key");
    assert!(
        route_bedrock_subagent(tmp.path(), context(id, parent, false), info, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn opaque_fork_reuses_source_pin_without_consuming_a_slot() {
    let tmp = TempDir::new().expect("home");
    let chief = key(tmp.path(), "chief");
    let a = key(tmp.path(), "a");
    let info = provider(chief.clone(), vec![a.clone()]);
    let source = ThreadId::new();
    route_bedrock_subagent(tmp.path(), context(source, None, true), info.clone(), None)
        .await
        .expect("source");
    let mut fork = context(ThreadId::new(), Some(source), true);
    fork.affinity_source = Some(source); // No live provider: simulate cold/offline fork.
    let forked = route_bedrock_subagent(tmp.path(), fork, info.clone(), None)
        .await
        .expect("fork");
    assert_eq!(bound_key(&forked), chief);
    assert!(!state_dir(tmp.path()).join("cursor.json").exists());
    let fresh = route_bedrock_subagent(
        tmp.path(),
        context(ThreadId::new(), Some(source), true),
        info,
        None,
    )
    .await
    .expect("independent");
    assert_eq!(bound_key(&fresh), a);
}

#[tokio::test]
async fn unknown_opaque_source_is_an_error_not_a_new_account() {
    let tmp = TempDir::new().expect("home");
    let info = provider(key(tmp.path(), "chief"), vec![key(tmp.path(), "a")]);
    let mut fork = context(ThreadId::new(), Some(ThreadId::new()), true);
    fork.affinity_source = fork.parent_id;
    assert!(
        route_bedrock_subagent(tmp.path(), fork, info, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn legacy_subagent_is_not_silently_assigned_a_new_account() {
    let tmp = TempDir::new().expect("home");
    let info = provider(key(tmp.path(), "chief"), vec![key(tmp.path(), "a")]);
    assert!(
        route_bedrock_subagent(
            tmp.path(),
            context(ThreadId::new(), Some(ThreadId::new()), false),
            info.clone(),
            None
        )
        .await
        .is_err()
    );
    assert!(
        route_bedrock_subagent(
            tmp.path(),
            context(ThreadId::new(), None, false),
            info,
            None
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn disabled_pool_and_non_bedrock_leave_new_sessions_alone() {
    let tmp = TempDir::new().expect("home");
    let info = provider(key(tmp.path(), "chief"), vec![]);
    let before = serde_json::to_value(&info).expect("serialize");
    let out = route_bedrock_subagent(
        tmp.path(),
        context(ThreadId::new(), Some(ThreadId::new()), true),
        info,
        None,
    )
    .await
    .expect("disabled");
    assert_eq!(serde_json::to_value(out).expect("serialize"), before);
    let info = ModelProviderInfo::create_openai_provider(None);
    let before = serde_json::to_value(&info).expect("serialize");
    let out = route_bedrock_subagent(
        tmp.path(),
        context(ThreadId::new(), Some(ThreadId::new()), true),
        info,
        None,
    )
    .await
    .expect("OpenAI");
    assert_eq!(serde_json::to_value(out).expect("serialize"), before);
    assert!(!state_dir(tmp.path()).exists());
}

#[tokio::test]
async fn invalid_slots_are_skipped_and_empty_pool_pins_inherited_identity() {
    let tmp = TempDir::new().expect("home");
    let chief = key(tmp.path(), "chief");
    let a = key(tmp.path(), "a");
    let empty = tmp.path().join("empty");
    fs::write(&empty, " \n").expect("empty key");
    let mut info = provider(
        chief.clone(),
        vec![tmp.path().join("missing"), empty, a.clone(), a.clone()],
    );
    let id = ThreadId::new();
    let out = route_bedrock_subagent(
        tmp.path(),
        context(id, Some(ThreadId::new()), true),
        info.clone(),
        None,
    )
    .await
    .expect("valid slot");
    assert_eq!(bound_key(&out), a);
    info.aws.as_mut().expect("AWS").subagent_api_key_files = vec![tmp.path().join("missing")];
    let out = route_bedrock_subagent(
        tmp.path(),
        context(ThreadId::new(), Some(ThreadId::new()), true),
        info,
        None,
    )
    .await
    .expect("inherited fallback");
    assert_eq!(bound_key(&out), chief);
}

#[tokio::test]
async fn restored_pin_rejects_project_changes_or_another_backend() {
    let tmp = TempDir::new().expect("home");
    let info = provider(key(tmp.path(), "chief"), vec![key(tmp.path(), "a")]);
    let id = ThreadId::new();
    let parent = Some(ThreadId::new());
    route_bedrock_subagent(tmp.path(), context(id, parent, true), info.clone(), None)
        .await
        .expect("spawn");
    let mut changed = info;
    changed.http_headers = Some(std::collections::HashMap::from([(
        "x-project-id".to_string(),
        "different".to_string().into(),
    )]));
    assert!(
        route_bedrock_subagent(tmp.path(), context(id, parent, false), changed, None)
            .await
            .is_err()
    );
    assert!(
        route_bedrock_subagent(
            tmp.path(),
            context(id, parent, false),
            ModelProviderInfo::create_openai_provider(None),
            None
        )
        .await
        .is_err()
    );
}

#[test]
fn persisted_bindings_contain_references_not_tokens() {
    let tmp = TempDir::new().expect("home");
    let path = key(tmp.path(), "key");
    let mut pin = binding();
    pin.account = Account::ApiKeyFile { path };
    let id = ThreadId::new();
    allocate(tmp.path(), id, pin.clone(), &[]).expect("allocate");
    let bytes = fs::read_to_string(pin_path(tmp.path(), id)).expect("pin");
    assert!(!bytes.contains("synthetic-key-token"));
    assert_eq!(read_pin(tmp.path(), id).expect("read"), Some(pin));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(pin_path(tmp.path(), id))
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn simultaneous_reloads_are_idempotent_and_new_assignments_are_balanced() {
    let tmp = TempDir::new().expect("home");
    let pool = vec![
        Account::Profile {
            name: "a".to_string(),
        },
        Account::Profile {
            name: "b".to_string(),
        },
    ];
    let same_id = ThreadId::new();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let pool = &pool;
            let dir = tmp.path();
            scope.spawn(move || {
                assert_eq!(
                    allocate(dir, same_id, binding(), pool)
                        .expect("allocate")
                        .account,
                    pool[0]
                );
            });
        }
    });
    assert_eq!(
        fs::read_to_string(tmp.path().join("cursor.json")).expect("cursor"),
        "1"
    );
    let results = std::thread::scope(|scope| {
        (0..20)
            .map(|_| {
                let pool = &pool;
                let dir = tmp.path();
                scope.spawn(move || {
                    allocate(dir, ThreadId::new(), binding(), pool)
                        .expect("allocate")
                        .account
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results
            .iter()
            .filter(|account| **account == pool[0])
            .count(),
        10
    );
    assert_eq!(
        results
            .iter()
            .filter(|account| **account == pool[1])
            .count(),
        10
    );
}

#[test]
fn corrupt_pin_or_counter_never_resets_identity() {
    let tmp = TempDir::new().expect("home");
    let id = ThreadId::new();
    fs::write(pin_path(tmp.path(), id), "not-json").expect("corrupt pin");
    assert!(allocate(tmp.path(), id, binding(), &[]).is_err());
    fs::write(tmp.path().join("cursor.json"), "not-json").expect("corrupt cursor");
    assert!(allocate(tmp.path(), ThreadId::new(), binding(), &[binding().account]).is_err());
}

#[test]
fn key_validation_is_bounded_and_errors_do_not_echo_tokens() {
    let tmp = TempDir::new().expect("home");
    let path = tmp.path().join("key");
    for bad in [
        "".to_string(),
        " \n".to_string(),
        "secret\ninjection".to_string(),
        "x".repeat(16_385),
    ] {
        fs::write(&path, &bad).expect("key");
        let error = auth::read_api_key_file(&path)
            .expect_err("invalid key")
            .to_string();
        assert!(!error.contains("injection"));
    }
    assert!(auth::read_api_key_file(Path::new("relative-key")).is_err());
    assert!(auth::read_api_key_file(tmp.path()).is_err());
}
