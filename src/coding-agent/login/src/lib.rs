mod device_code_auth;
mod pkce;
mod server;

pub use device_code_auth::DeviceCode;
pub use device_code_auth::complete_device_code_login;
pub use device_code_auth::request_device_code;
pub use device_code_auth::run_device_code_login;
pub use server::LoginServer;
pub use server::ServerOptions;
pub use server::ShutdownHandle;
pub use server::run_login_server;

// Re-export commonly used auth types and helpers from adam-coding-agent for compatibility.
pub use adam_agent::AuthManager;
pub use adam_agent::CodexAuth;
pub use adam_agent::auth::AuthDotJson;
pub use adam_agent::auth::CLIENT_ID;
pub use adam_agent::auth::CODEX_API_KEY_ENV_VAR;
pub use adam_agent::auth::OPENAI_API_KEY_ENV_VAR;
pub use adam_agent::auth::login_with_api_key;
pub use adam_agent::auth::logout;
pub use adam_agent::auth::save_auth;
pub use adam_agent::token_data::TokenData;
pub use adam_app_server_protocol::AuthMode;
