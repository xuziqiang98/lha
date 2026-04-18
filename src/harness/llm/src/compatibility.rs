use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatRoleCompatibility {
    Unknown,
    SupportsDeveloper,
    RequiresSystemForDeveloper,
}

#[derive(Debug)]
pub(crate) struct ChatRoleCompatibilityState {
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

pub(crate) type ChatRoleCompatibilityHandle = Arc<Mutex<ChatRoleCompatibilityState>>;
