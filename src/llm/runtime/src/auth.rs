use lha_api::AuthProvider as ApiAuthProvider;

use crate::Result;
use crate::provider::RuntimeEndpoint;

#[derive(Clone, Debug, Default)]
pub(crate) struct LlmAuthProvider {
    token: Option<String>,
}

impl ApiAuthProvider for LlmAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }
}

pub(crate) fn auth_provider_from_endpoint(endpoint: &RuntimeEndpoint) -> Result<LlmAuthProvider> {
    if let Some(api_key) = endpoint.api_key()? {
        return Ok(LlmAuthProvider {
            token: Some(api_key),
        });
    }

    Ok(LlmAuthProvider {
        token: endpoint.bearer_token.clone(),
    })
}
