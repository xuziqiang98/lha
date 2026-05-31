use lha_client::Request;

use crate::provider::WireApi;

/// Provides bearer and account identity information for API requests.
///
/// Implementations should be cheap and non-blocking; any asynchronous
/// refresh or I/O should be handled by higher layers before requests
/// reach this interface.
pub trait AuthProvider: Send + Sync {
    fn bearer_token(&self) -> Option<String>;
}

pub(crate) fn add_auth_headers<A: AuthProvider>(
    auth: &A,
    wire_api: WireApi,
    mut req: Request,
) -> Request {
    if let Some(token) = auth.bearer_token() {
        match wire_api {
            WireApi::Messages => {
                if let Ok(header) = token.parse() {
                    let _ = req.headers.insert("x-api-key", header);
                }
            }
            WireApi::Responses | WireApi::Chat | WireApi::Compact => {
                if let Ok(header) = format!("Bearer {token}").parse() {
                    let _ = req.headers.insert(http::header::AUTHORIZATION, header);
                }
            }
        }
    }
    req
}
