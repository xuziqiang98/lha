mod auth_status;
mod logging_client_handler;
mod oauth;
mod perform_oauth_login;
mod program_resolver;
mod rmcp_client;
#[cfg(any(test, debug_assertions))]
#[path = "bin/test_stdio_server.rs"]
pub(crate) mod test_stdio_server;
#[cfg(any(test, debug_assertions))]
#[path = "bin/test_streamable_http_server.rs"]
pub(crate) mod test_streamable_http_server;
mod utils;

pub use crate::product::protocol::protocol::McpAuthStatus;
pub use auth_status::determine_streamable_http_auth_status;
pub use auth_status::supports_oauth_login;
pub use oauth::OAuthCredentialsStoreMode;
pub use oauth::StoredOAuthTokens;
pub use oauth::WrappedOAuthTokenResponse;
pub use oauth::delete_oauth_tokens;
pub(crate) use oauth::load_oauth_tokens;
pub use oauth::save_oauth_tokens;
pub use perform_oauth_login::OauthLoginHandle;
pub use perform_oauth_login::perform_oauth_login;
pub use perform_oauth_login::perform_oauth_login_return_url;
pub use rmcp::model::ElicitationAction;
pub use rmcp_client::Elicitation;
pub use rmcp_client::ElicitationResponse;
pub use rmcp_client::ListToolsWithConnectorIdResult;
pub use rmcp_client::RmcpClient;
pub use rmcp_client::SendElicitation;
pub use rmcp_client::ToolWithConnectorId;
