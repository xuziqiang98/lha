use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAuth {
    api_key: String,
}

impl CodexAuth {
    pub fn from_api_key(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
        }
    }

    pub fn get_token(&self) -> anyhow::Result<String> {
        Ok(self.api_key.clone())
    }

    pub fn from_auth_storage(
        _adam_home: &std::path::Path,
        _auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> std::io::Result<Option<Self>> {
        Ok(Some(Self::from_api_key("test")))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthCredentialsStoreMode {
    #[default]
    File,
    Keyring,
    Auto,
    Ephemeral,
}

#[derive(Debug)]
pub struct AuthManager {
    auth: RwLock<Option<CodexAuth>>,
}

impl AuthManager {
    pub fn new(
        _adam_home: PathBuf,
        _prefer_api_key: bool,
        _auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            auth: RwLock::new(None),
        }
    }

    pub fn shared(adam_home: PathBuf, prefer_api_key: bool) -> Arc<Self> {
        Arc::new(Self::new(
            adam_home,
            prefer_api_key,
            AuthCredentialsStoreMode::File,
        ))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        Arc::new(Self {
            auth: RwLock::new(Some(auth)),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, _adam_home: PathBuf) -> Arc<Self> {
        Self::from_auth_for_testing(auth)
    }

    pub fn auth_cached(&self) -> Option<CodexAuth> {
        self.auth.read().ok().and_then(|guard| guard.clone())
    }

    pub async fn auth(&self) -> Option<CodexAuth> {
        self.auth_cached()
    }

    pub fn get_auth_mode(&self) -> Option<String> {
        self.auth_cached().map(|_| "apikey".to_string())
    }
}

pub async fn load_auth(
    _adam_home: &std::path::Path,
    _prefer_api_key: bool,
) -> std::io::Result<Option<CodexAuth>> {
    Ok(None)
}

pub fn logout(_adam_home: &std::path::Path) -> std::io::Result<bool> {
    Ok(false)
}
