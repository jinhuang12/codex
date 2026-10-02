//! The application account authorizes remote control; the provider account authorizes inference.
//! No ChatGPT credential is returned to the Bedrock provider or exported through legacy RPCs.

use super::AccountRequestProcessor;
use crate::auth_mode::auth_mode_to_api;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::GetAuthStatusResponse;
use codex_app_server_protocol::InferenceAccount;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::ProviderAccount;
use codex_model_provider::ProviderAccountError;
use codex_model_provider::ProviderAccountState;

fn control_auth(manager: &AuthManager) -> Option<CodexAuth> {
    manager.auth_cached().filter(|auth| {
        auth.is_chatgpt_auth()
            && auth.get_account_id().is_some_and(|id| !id.is_empty())
            && manager.refresh_failure_for_auth(auth).is_none()
    })
}

pub(super) fn account_states(
    config: &Config,
    manager: &AuthManager,
    provider: ProviderAccountState,
) -> Result<(ProviderAccountState, Option<InferenceAccount>), ProviderAccountError> {
    if !config.remote_control_mantle {
        return Ok((provider, None));
    }
    let account = control_auth(manager)
        .map(|auth| {
            let plan_type = auth
                .account_plan_type()
                .ok_or(ProviderAccountError::MissingChatgptAccountDetails)?;
            Ok::<_, ProviderAccountError>(ProviderAccount::Chatgpt {
                email: auth.get_account_email(),
                plan_type,
            })
        })
        .transpose()?;
    Ok((
        ProviderAccountState {
            account,
            // This is an application login requirement, not an inference credential.
            requires_openai_auth: true,
        },
        Some(InferenceAccount {
            model_provider: config.model_provider_id.clone(),
            account: provider.account.map(Account::from),
            requires_openai_auth: provider.requires_openai_auth,
        }),
    ))
}

pub(super) fn auth_status(manager: &AuthManager) -> GetAuthStatusResponse {
    GetAuthStatusResponse {
        auth_method: control_auth(manager).map(|auth| auth_mode_to_api(auth.api_auth_mode())),
        // includeToken must never turn a control-plane token into an inference credential.
        auth_token: None,
        requires_openai_auth: Some(true),
    }
}

impl AccountRequestProcessor {
    pub(super) async fn ensure_mantle_control_login_preserved(
        &self,
    ) -> Result<(), JSONRPCErrorError> {
        // Do not use the permissive config fallback before a credential write.
        let config = self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
            .map_err(|_| internal_error("cannot verify the current login configuration"))?;
        if config.remote_control_mantle {
            return Err(invalid_request(
                "Mantle remote control requires ChatGPT login. Configure AWS credentials with a profile, AWS_BEARER_TOKEN_BEDROCK, or a credential command instead of replacing the control login with a managed API key.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::config::ConfigBuilder;
    use codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn config(enabled: bool) -> Config {
        let home = TempDir::new().expect("temporary home");
        ConfigBuilder::default()
            .loader_overrides(codex_config::LoaderOverrides::without_managed_config_for_tests())
            .codex_home(home.path().to_path_buf())
            .cli_overrides(vec![
                ("model_provider".into(), AMAZON_BEDROCK_PROVIDER_ID.into()),
                ("remote_control_mantle".into(), enabled.into()),
            ])
            .build()
            .await
            .expect("Mantle config")
    }

    fn provider_account() -> ProviderAccountState {
        ProviderAccountState {
            account: Some(ProviderAccount::AmazonBedrock {
                uses_codex_managed_credentials: false,
            }),
            requires_openai_auth: false,
        }
    }

    #[tokio::test]
    async fn mantle_reports_real_control_identity_and_separate_inference_identity() {
        let config = config(true).await;
        let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
        let expected_email = auth.get_account_email();
        let expected_plan = auth.account_plan_type().expect("plan");
        let manager = AuthManager::from_auth_for_testing(auth);
        let (control, inference) =
            account_states(&config, &manager, provider_account()).expect("separate accounts");
        assert_eq!(
            control,
            ProviderAccountState {
                account: Some(ProviderAccount::Chatgpt {
                    email: expected_email,
                    plan_type: expected_plan,
                }),
                requires_openai_auth: true,
            }
        );
        assert_eq!(
            inference,
            Some(InferenceAccount {
                model_provider: AMAZON_BEDROCK_PROVIDER_ID.into(),
                account: Some(Account::AmazonBedrock {
                    uses_codex_managed_credentials: false
                }),
                requires_openai_auth: false,
            })
        );
        assert_eq!(auth_status(&manager).auth_token, None);
        assert!(auth_status(&manager).auth_method.is_some());
    }

    #[tokio::test]
    async fn mantle_never_promotes_api_key_to_control_identity() {
        let config = config(true).await;
        let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("not-chatgpt"));
        let (control, inference) = account_states(&config, &manager, provider_account())
            .expect("account status without control login");
        assert_eq!(control.account, None);
        assert!(control.requires_openai_auth);
        assert!(inference.is_some());
        assert_eq!(auth_status(&manager).auth_method, None);
        assert_eq!(auth_status(&manager).auth_token, None);
    }

    #[tokio::test]
    async fn disabled_mode_preserves_provider_account_contract() {
        let config = config(false).await;
        let manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        assert_eq!(
            account_states(&config, &manager, provider_account()),
            Ok((provider_account(), None))
        );
    }
}
