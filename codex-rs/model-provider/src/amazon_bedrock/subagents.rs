//! Allocate an account once per independent thread, never once per request.
//!
//! The user-wide cursor coordinates separate Codex processes. Immutable thread pins
//! outlive runtimes and pool edits. Neither file contains credential material.

use super::AmazonBedrockModelProvider;
use super::auth;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

/// Startup facts supplied after role resolution and history preparation.
pub struct BedrockSubagentContext {
    pub thread_id: ThreadId,
    pub is_new: bool,
    pub is_subagent: bool,
    pub parent_id: Option<ThreadId>,
    /// Set only when inherited history can contain account-bound provider state.
    pub affinity_source: Option<ThreadId>,
    /// Effective source settings, when the source is still live.
    pub source_provider: Option<ModelProviderInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Account {
    Profile {
        name: String,
    },
    ApiKeyFile {
        path: PathBuf,
    },
    /// Native auth not representable as a profile or key file. Never rotate it.
    Inherited {
        fingerprint: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u8,
    account: Account,
    /// Pool assignments must not inherit an account-specific refresh command.
    rotated: bool,
    runtime_endpoint: bool,
    base_url: String,
    region: String,
    /// Detect project/header changes without writing header secrets to disk.
    request_scope: String,
}

fn state_dir(home: &Path) -> PathBuf {
    home.join("bedrock-subagents")
}

fn pin_path(dir: &Path, thread: ThreadId) -> PathBuf {
    dir.join(format!("{thread}.json"))
}

fn read_pin(dir: &Path, thread: ThreadId) -> io::Result<Option<Binding>> {
    match fs::read(pin_path(dir, thread)) {
        Ok(bytes) => {
            let binding: Binding = serde_json::from_slice(&bytes).map_err(|_| {
                io::Error::other("invalid Bedrock account binding; refusing to reassign")
            })?;
            if binding.version != 1 {
                return Err(io::Error::other(
                    "unsupported Bedrock account binding version",
                ));
            }
            Ok(Some(binding))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn pool_enabled(info: &ModelProviderInfo) -> bool {
    info.aws.as_ref().is_some_and(|aws| {
        !aws.subagent_profiles.is_empty() || !aws.subagent_api_key_files.is_empty()
    })
}

fn native_bedrock(info: &ModelProviderInfo) -> bool {
    info.is_amazon_bedrock()
}

fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

fn request_scope(info: &ModelProviderInfo) -> String {
    // Sort environment-backed headers so HashMap iteration order cannot alter a pin.
    let headers: std::collections::BTreeMap<_, _> = info
        .env_http_headers
        .iter()
        .flat_map(|headers| headers.iter())
        .map(|(name, variable)| (name, std::env::var(variable).ok()))
        .collect();
    let static_headers: std::collections::BTreeMap<_, _> = info
        .http_headers
        .iter()
        .flat_map(|values| values.iter())
        .collect();
    let query: std::collections::BTreeMap<_, _> = info
        .query_params
        .iter()
        .flat_map(|values| values.iter())
        .collect();
    digest(
        serde_json::to_vec(&serde_json::json!({
            "headers": static_headers,
            "environment_headers": headers,
            "query": query,
        }))
        .expect("provider request scope is serializable"),
    )
}

fn inherited_account(provider: &AmazonBedrockModelProvider) -> Account {
    use auth::BedrockAuthSource;
    match provider.auth_source() {
        BedrockAuthSource::ConfiguredAwsProfile => Account::Profile {
            name: provider.aws.profile.clone().expect("selected profile"),
        },
        BedrockAuthSource::ConfiguredApiKeyFile => Account::ApiKeyFile {
            path: provider
                .aws
                .api_key_file
                .clone()
                .expect("selected key file"),
        },
        source => {
            let credential = match (source, provider.managed_auth()) {
                (BedrockAuthSource::ManagedBearerToken, Some(CodexAuth::BedrockApiKey(auth))) => {
                    serde_json::json!([auth.api_key, auth.region])
                }
                (
                    BedrockAuthSource::ManagedAccessKeys,
                    Some(CodexAuth::BedrockAccessKeys(auth)),
                ) => {
                    serde_json::json!([
                        auth.access_key_id,
                        auth.secret_access_key,
                        auth.session_token
                    ])
                }
                (BedrockAuthSource::EnvBearerToken, _) => {
                    serde_json::json!(std::env::var("AWS_BEARER_TOKEN_BEDROCK").ok())
                }
                (BedrockAuthSource::EnvAwsCredentials, _) => serde_json::json!([
                    std::env::var("AWS_ACCESS_KEY_ID").ok(),
                    std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
                    std::env::var("AWS_SESSION_TOKEN").ok(),
                ]),
                _ => serde_json::json!({
                    "command": provider.info.auth,
                    "export": provider.aws.credential_export,
                    "profile": std::env::var("AWS_PROFILE").ok(),
                    "default_profile": std::env::var("AWS_DEFAULT_PROFILE").ok(),
                    "config_file": std::env::var("AWS_CONFIG_FILE").ok(),
                    "credentials_file": std::env::var("AWS_SHARED_CREDENTIALS_FILE").ok(),
                }),
            };
            Account::Inherited {
                fingerprint: digest(
                    serde_json::to_vec(&(format!("{source:?}"), credential))
                        .expect("auth reference is serializable"),
                ),
            }
        }
    }
}

async fn capture_binding(provider: &AmazonBedrockModelProvider) -> Result<Binding> {
    let factory = provider.http_client_factory.clone().with_network_policy(
        provider
            .http_client_factory
            .network_policy()
            .clone()
            .for_current_account(),
    );
    let region = auth::resolve_region(
        provider.auth_source(),
        provider.managed_auth().as_ref(),
        &provider.aws,
        provider.endpoint,
        &factory,
    )
    .await?;
    let base_url = provider
        .runtime_base_url()
        .await?
        .ok_or_else(|| CodexErr::Fatal("Amazon Bedrock did not resolve an endpoint".to_string()))?;
    // Native endpoints carry no URL credentials. Do not persist a secret-bearing URL.
    let parsed = url::Url::parse(&base_url).map_err(|_| {
        CodexErr::InvalidRequest("invalid Bedrock endpoint for subagent routing".to_string())
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.query().is_some() {
        return Err(CodexErr::InvalidRequest("Bedrock subagent routing requires an endpoint without URL credentials or query parameters".to_string()));
    }
    Ok(Binding {
        version: 1,
        account: inherited_account(provider),
        rotated: false,
        runtime_endpoint: provider.info.is_amazon_bedrock_runtime(),
        base_url,
        region,
        request_scope: request_scope(&provider.info),
    })
}

async fn apply_binding(
    binding: &Binding,
    mut info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> Result<ModelProviderInfo> {
    if !native_bedrock(&info)
        || binding.runtime_endpoint != info.is_amazon_bedrock_runtime()
        || binding.request_scope != request_scope(&info)
    {
        return Err(CodexErr::InvalidRequest(
            "this thread is bound to its original Bedrock endpoint and project; restore its provider settings or start a new thread".to_string(),
        ));
    }
    let aws = info.aws.get_or_insert_default();
    match &binding.account {
        Account::Profile { name } => {
            if !codex_aws_auth::discover_aws_profiles()
                .await
                .map_err(|_| CodexErr::Fatal("cannot discover the pinned AWS profile".to_string()))?
                .iter()
                .any(|profile| &profile.name == name)
            {
                return Err(CodexErr::Fatal(format!(
                    "pinned AWS profile {name:?} is unavailable; restore it (no account failover)"
                )));
            }
            let same_source = aws.profile.as_ref() == Some(name) && aws.api_key_file.is_none();
            aws.profile = Some(name.clone());
            aws.api_key_file = None;
            aws.credential_export = None;
            if binding.rotated || !same_source {
                aws.auth_refresh = None;
            }
            info.auth = None;
        }
        Account::ApiKeyFile { path } => {
            auth::read_api_key_file(path)?;
            let same_source = aws.api_key_file.as_ref() == Some(path);
            aws.profile = None;
            aws.api_key_file = Some(path.clone());
            aws.credential_export = None;
            if binding.rotated || !same_source {
                aws.auth_refresh = None;
            }
            info.auth = None;
        }
        Account::Inherited { .. } => {}
    }
    // Keep an equivalent host definition unchanged: Remote Control binds its
    // exact shape. Only restore explicit values when the resolved route differs.
    let resolved = capture_binding(&AmazonBedrockModelProvider::new(
        info.clone(),
        auth_manager.clone(),
    ))
    .await?;
    if resolved.base_url != binding.base_url {
        info.base_url = Some(binding.base_url.clone());
    }
    if resolved.region != binding.region {
        info.aws.as_mut().expect("AWS settings").region = Some(binding.region.clone());
    }
    if matches!(binding.account, Account::Inherited { .. }) {
        let current = AmazonBedrockModelProvider::new(info.clone(), auth_manager);
        if inherited_account(&current) != binding.account {
            return Err(CodexErr::InvalidRequest("the inherited Bedrock credential source changed; restore the original account before resuming".to_string()));
        }
    }
    Ok(info)
}

/// Restore only an existing host-written pin; never allocate from client input.
pub async fn restore_bedrock_subagent_binding(
    home: &Path,
    thread_id: ThreadId,
    info: ModelProviderInfo,
) -> Result<Option<ModelProviderInfo>> {
    let dir = state_dir(home);
    let binding = tokio::task::spawn_blocking(move || read_pin(&dir, thread_id))
        .await
        .map_err(|err| CodexErr::Fatal(format!("Bedrock binding read failed: {err}")))??;
    match binding {
        Some(binding) => apply_binding(&binding, info, None).await.map(Some),
        None => Ok(None),
    }
}

/// Route a native Bedrock thread before its provider or first model request is created.
/// Reopening a thread always reads its old binding, even after the pool is disabled.
pub async fn route_bedrock_subagent(
    home: &Path,
    context: BedrockSubagentContext,
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> Result<ModelProviderInfo> {
    let dir = state_dir(home);
    let read_dir = dir.clone();
    let thread_id = context.thread_id;
    let source_id = context.affinity_source;
    let parent_id = context.parent_id;
    let (saved, source, parent_pinned) = tokio::task::spawn_blocking(move || -> io::Result<_> {
        Ok((
            read_pin(&read_dir, thread_id)?,
            source_id
                .map(|id| read_pin(&read_dir, id))
                .transpose()?
                .flatten(),
            parent_id
                .map(|id| read_pin(&read_dir, id))
                .transpose()?
                .flatten()
                .is_some(),
        ))
    })
    .await
    .map_err(|err| CodexErr::Fatal(format!("Bedrock binding read failed: {err}")))??;
    if let Some(saved) = saved {
        return apply_binding(&saved, info, auth_manager).await;
    }
    let enabled = pool_enabled(&info);
    if !native_bedrock(&info) {
        if source.is_some()
            || (source_id.is_some() && context.source_provider.as_ref().is_some_and(native_bedrock))
        {
            return Err(CodexErr::InvalidRequest(
                "cannot replay an account-bound Bedrock history with another provider".to_string(),
            ));
        }
        return Ok(info);
    }
    if !context.is_new {
        if enabled && context.is_subagent {
            return Err(CodexErr::InvalidRequest("this existing Bedrock subagent has no account binding; resume with its original credentials and the subagent pool disabled, or start a new agent".to_string()));
        }
        // Legacy sessions have no trustworthy assignment. Never give them a new slot.
        return Ok(info);
    }
    if !enabled && source.is_none() && !parent_pinned {
        return Ok(info);
    }
    let native = AmazonBedrockModelProvider::new(info.clone(), auth_manager.clone());
    let mut binding = if let Some(source) = source {
        source
    } else if context.affinity_source.is_some() {
        let source_info = context.source_provider.ok_or_else(|| CodexErr::InvalidRequest(
            "cannot determine the account that owns inherited Bedrock history; open its source thread first".to_string()
        ))?;
        if !native_bedrock(&source_info) {
            return Err(CodexErr::InvalidRequest(
                "cannot replay another provider's opaque history on Bedrock".to_string(),
            ));
        }
        capture_binding(&AmazonBedrockModelProvider::new(
            source_info,
            auth_manager.clone(),
        ))
        .await?
    } else {
        capture_binding(&native).await?
    };
    let may_rotate = enabled
        && context.is_subagent
        && context.affinity_source.is_none()
        && !info.has_command_auth()
        && native.aws.credential_export.is_none();
    let mut accounts = Vec::new();
    if may_rotate {
        let profiles = codex_aws_auth::discover_aws_profiles().await.unwrap_or_else(|_| {
            tracing::warn!("AWS profile discovery failed; only readable Bedrock key files can join the pool");
            Vec::new()
        });
        for name in &native.aws.subagent_profiles {
            let account = Account::Profile { name: name.clone() };
            if profiles.iter().any(|profile| &profile.name == name) && !accounts.contains(&account)
            {
                accounts.push(account);
            }
        }
        for path in &native.aws.subagent_api_key_files {
            let account = Account::ApiKeyFile { path: path.clone() };
            if auth::read_api_key_file(path).is_ok() && !accounts.contains(&account) {
                accounts.push(account);
            }
        }
        if accounts.is_empty() {
            tracing::warn!(
                "no usable Bedrock subagent account slots; pinning the inherited credential source"
            );
        }
    }
    binding = tokio::task::spawn_blocking(move || allocate(&dir, thread_id, binding, &accounts))
        .await
        .map_err(|err| CodexErr::Fatal(format!("Bedrock account allocation failed: {err}")))??;
    tracing::info!(thread_id = %thread_id, account = ?binding.account, "pinned Bedrock subagent account");
    apply_binding(&binding, info, auth_manager).await
}

/// All participants lock one stable inode; a crashed process releases the OS lock.
fn allocate(
    dir: &Path,
    thread_id: ThreadId,
    mut binding: Binding,
    accounts: &[Account],
) -> io::Result<Binding> {
    fs::create_dir_all(dir)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("routing.lock"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => {
                return Err(io::Error::other(format!(
                    "cannot lock Bedrock account allocator: {err}"
                )));
            }
        }
    }
    // A concurrent startup of the same UUID must not consume another slot.
    if let Some(existing) = read_pin(dir, thread_id)? {
        return Ok(existing);
    }
    if !accounts.is_empty() {
        let cursor_path = dir.join("cursor.json");
        let cursor: u64 = match fs::read(&cursor_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| io::Error::other("invalid Bedrock account cursor"))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => return Err(err),
        };
        binding.account = accounts[(cursor % accounts.len() as u64) as usize].clone();
        binding.rotated = true;
        // A crash may skip a slot, but cannot publish an agent without its binding.
        atomic_write(
            dir,
            &cursor_path,
            &serde_json::to_vec(&cursor.wrapping_add(1))?,
        )?;
    }
    atomic_write(
        dir,
        &pin_path(dir, thread_id),
        &serde_json::to_vec(&binding)?,
    )?;
    Ok(binding)
}

fn atomic_write(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = dir.join(format!(".{}.tmp", ThreadId::default()));
    let result = (|| {
        let mut options = File::options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
#[path = "subagents_tests.rs"]
mod tests;
