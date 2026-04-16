use std::sync::Arc;

use async_trait::async_trait;
use codex_llm::AuthContext;
use codex_llm::AuthSource;
use codex_llm::Error as LlmError;
use codex_llm::LlmRuntimeConfig;
use codex_llm::UnauthorizedRecovery as LlmUnauthorizedRecovery;
use codex_llm::UnauthorizedRecoveryFailedReason;

use crate::auth::AuthManager;
use crate::auth::CodexAuth;
use crate::auth::RefreshTokenError;
use crate::config::Config;
use crate::error::RefreshTokenFailedReason;
use crate::features::FEATURES;
use crate::features::Feature;
use crate::flags::CODEX_RS_SSE_FIXTURE;
use crate::model_provider_info::ModelProviderInfo;

pub(crate) struct CoreAuthSource {
    auth_manager: Option<Arc<AuthManager>>,
    provider: ModelProviderInfo,
}

impl CoreAuthSource {
    pub(crate) fn boxed(
        auth_manager: Option<Arc<AuthManager>>,
        provider: ModelProviderInfo,
    ) -> Arc<dyn AuthSource> {
        Arc::new(Self {
            auth_manager,
            provider,
        })
    }
}

#[async_trait]
impl AuthSource for CoreAuthSource {
    async fn current_auth(&self) -> codex_llm::Result<Option<AuthContext>> {
        let auth = match self.auth_manager.as_ref() {
            Some(manager) => manager.auth().await,
            None => None,
        };
        auth_context_from_auth(auth, &self.provider)
    }

    fn unauthorized_recovery(&self) -> Option<Box<dyn LlmUnauthorizedRecovery>> {
        if self.provider.has_local_auth() {
            return None;
        }

        self.auth_manager.as_ref().map(|manager| {
            Box::new(CoreUnauthorizedRecovery {
                inner: manager.unauthorized_recovery(),
            }) as Box<dyn LlmUnauthorizedRecovery>
        })
    }
}

struct CoreUnauthorizedRecovery {
    inner: crate::auth::UnauthorizedRecovery,
}

#[async_trait]
impl LlmUnauthorizedRecovery for CoreUnauthorizedRecovery {
    fn has_next(&self) -> bool {
        self.inner.has_next()
    }

    async fn recover(&mut self) -> codex_llm::Result<()> {
        match self.inner.next().await {
            Ok(()) => Ok(()),
            Err(RefreshTokenError::Permanent(failed)) => {
                Err(LlmError::UnauthorizedRecoveryFailed {
                    reason: map_refresh_failure_reason(failed.reason),
                    message: failed.message,
                })
            }
            Err(RefreshTokenError::Transient(err)) => Err(LlmError::Io(err)),
        }
    }
}

pub(crate) fn llm_runtime_config_from_core_config(config: &Config) -> LlmRuntimeConfig {
    LlmRuntimeConfig {
        model_provider_id: config.model_provider_id.clone(),
        show_raw_agent_reasoning: config.show_raw_agent_reasoning,
        model_verbosity: config.model_verbosity,
        web_search_mode: config.web_search_mode,
        responses_websockets_enabled: config.features.enabled(Feature::ResponsesWebsockets),
        experimental_beta_feature_keys: FEATURES
            .iter()
            .filter_map(|spec| {
                if spec.stage.experimental_menu_description().is_some()
                    && config.features.enabled(spec.id)
                {
                    Some(spec.key.to_string())
                } else {
                    None
                }
            })
            .collect(),
        sse_fixture_path: (*CODEX_RS_SSE_FIXTURE).map(str::to_string),
    }
}

pub(crate) fn auth_context_from_auth(
    auth: Option<CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_llm::Result<Option<AuthContext>> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(Some(AuthContext {
            bearer_token: Some(api_key),
            account_id: None,
            use_chatgpt_base_url: false,
        }));
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(Some(AuthContext {
            bearer_token: Some(token),
            account_id: None,
            use_chatgpt_base_url: false,
        }));
    }

    if let Some(auth) = auth {
        return Ok(Some(AuthContext {
            bearer_token: Some(auth.get_token()?),
            account_id: auth.get_account_id(),
            use_chatgpt_base_url: auth.is_chatgpt_auth(),
        }));
    }

    Ok(None)
}

fn map_refresh_failure_reason(
    reason: RefreshTokenFailedReason,
) -> UnauthorizedRecoveryFailedReason {
    match reason {
        RefreshTokenFailedReason::Expired => UnauthorizedRecoveryFailedReason::Expired,
        RefreshTokenFailedReason::Exhausted => UnauthorizedRecoveryFailedReason::Exhausted,
        RefreshTokenFailedReason::Revoked => UnauthorizedRecoveryFailedReason::Revoked,
        RefreshTokenFailedReason::Other => UnauthorizedRecoveryFailedReason::Other,
    }
}
