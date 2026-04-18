use async_trait::async_trait;
use codex_api::AuthProvider as ApiAuthProvider;

use crate::Result;

#[derive(Clone, Debug, Default)]
pub struct AuthContext {
    pub bearer_token: Option<String>,
    pub account_id: Option<String>,
    pub use_chatgpt_base_url: bool,
}

#[async_trait]
pub trait AuthSource: Send + Sync {
    async fn current_auth(&self) -> Result<Option<AuthContext>>;

    fn unauthorized_recovery(&self) -> Option<Box<dyn UnauthorizedRecovery>> {
        None
    }
}

#[async_trait]
pub trait UnauthorizedRecovery: Send {
    fn has_next(&self) -> bool;

    async fn recover(&mut self) -> Result<()>;
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LlmAuthProvider {
    token: Option<String>,
    account_id: Option<String>,
}

impl ApiAuthProvider for LlmAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }
}

pub(crate) fn auth_provider_from_context(auth: Option<AuthContext>) -> LlmAuthProvider {
    match auth {
        Some(auth) => LlmAuthProvider {
            token: auth.bearer_token,
            account_id: auth.account_id,
        },
        None => LlmAuthProvider::default(),
    }
}
