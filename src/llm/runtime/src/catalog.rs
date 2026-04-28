use crate::Result;
use crate::auth::auth_provider_from_endpoint;
use adam_api::ModelsClient;
use adam_api::ReqwestTransport;
use adam_client::HttpTransport;
use adam_llm_types::ModelInfo;
use http::HeaderMap;
use reqwest::Client;
use std::time::Duration;
use tokio::time::timeout;

/// Strategy for refreshing the runtime model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogRefreshStrategy {
    /// Always fetch from the network, ignoring cache.
    Online,
    /// Only use cached data, never fetch from the network.
    Offline,
    /// Use cache if available and fresh, otherwise fetch from the network.
    OnlineIfUncached,
}

pub async fn fetch_remote_models(
    http_client: Client,
    provider: &crate::provider::RuntimeEndpoint,
    client_version: &str,
    extra_headers: HeaderMap,
    request_timeout: Duration,
) -> Result<(Vec<ModelInfo>, Option<String>)> {
    let transport = ReqwestTransport::new(http_client);
    fetch_remote_models_with_transport(
        transport,
        provider.to_api_provider()?,
        auth_provider_from_endpoint(provider)?,
        client_version,
        extra_headers,
        request_timeout,
    )
    .await
}

async fn fetch_remote_models_with_transport<T, A>(
    transport: T,
    provider: adam_api::Provider,
    auth: A,
    client_version: &str,
    extra_headers: HeaderMap,
    request_timeout: Duration,
) -> Result<(Vec<ModelInfo>, Option<String>)>
where
    T: HttpTransport,
    A: adam_api::AuthProvider,
{
    let client = ModelsClient::new(transport, provider, auth);
    timeout(
        request_timeout,
        client.list_models(client_version, extra_headers),
    )
    .await
    .map_err(|_| crate::Error::RequestTimeout)?
    .map_err(Into::into)
}
