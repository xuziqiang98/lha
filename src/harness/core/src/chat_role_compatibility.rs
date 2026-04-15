#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ChatRoleCompatibilityKey {
    pub(crate) model_provider_id: String,
}

impl ChatRoleCompatibilityKey {
    pub(crate) fn new(model_provider_id: impl Into<String>) -> Self {
        Self {
            model_provider_id: model_provider_id.into(),
        }
    }
}

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
    pub(crate) fn new() -> Self {
        Self {
            compatibility: ChatRoleCompatibility::Unknown,
        }
    }

    pub(crate) fn current(&self) -> ChatRoleCompatibility {
        self.compatibility
    }

    pub(crate) fn record_supports_developer(&mut self) {
        self.compatibility = ChatRoleCompatibility::SupportsDeveloper;
    }

    pub(crate) fn record_requires_system(&mut self) {
        self.compatibility = ChatRoleCompatibility::RequiresSystemForDeveloper;
    }
}
