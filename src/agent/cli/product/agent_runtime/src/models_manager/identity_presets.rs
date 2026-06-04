use crate::product::protocol::config_types::IdentityMask;

pub(super) fn builtin_identity_presets() -> Vec<IdentityMask> {
    crate::product::identity::builtin_identity_presets()
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_builtin_identity_presets() -> Vec<IdentityMask> {
    builtin_identity_presets()
}
