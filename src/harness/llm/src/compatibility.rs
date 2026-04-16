use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatRoleCompatibilityKey {
    pub model_provider_id: String,
}

impl ChatRoleCompatibilityKey {
    pub fn new(model_provider_id: impl Into<String>) -> Self {
        Self {
            model_provider_id: model_provider_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRoleCompatibility {
    Unknown,
    SupportsDeveloper,
    RequiresSystemForDeveloper,
}

#[derive(Debug)]
pub struct ChatRoleCompatibilityState {
    compatibility: ChatRoleCompatibility,
}

impl ChatRoleCompatibilityState {
    pub fn new() -> Self {
        Self {
            compatibility: ChatRoleCompatibility::Unknown,
        }
    }

    pub fn current(&self) -> ChatRoleCompatibility {
        self.compatibility
    }

    pub fn record_supports_developer(&mut self) {
        self.compatibility = ChatRoleCompatibility::SupportsDeveloper;
    }

    pub fn record_requires_system(&mut self) {
        self.compatibility = ChatRoleCompatibility::RequiresSystemForDeveloper;
    }
}

impl Default for ChatRoleCompatibilityState {
    fn default() -> Self {
        Self::new()
    }
}

pub type ChatRoleCompatibilityHandle = Arc<Mutex<ChatRoleCompatibilityState>>;
