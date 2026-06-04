use crate::product::agent::ThreadManager;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::IdentityMask;

fn is_tui_identity(kind: IdentityKind) -> bool {
    matches!(
        kind,
        IdentityKind::Nobody
            | IdentityKind::Planner
            | IdentityKind::Programmer
            | IdentityKind::Explorer
            | IdentityKind::Reviewer
    )
}

pub(crate) fn normalize_mask(mut mask: IdentityMask) -> IdentityMask {
    if mask.kind.is_some_and(is_tui_identity) {
        mask.model = None;
        mask.reasoning_effort = None;
    }
    mask
}

fn filtered_presets(thread_manager: &ThreadManager) -> Vec<IdentityMask> {
    thread_manager
        .list_identities()
        .into_iter()
        .filter(|mask| mask.kind.is_some_and(is_tui_identity))
        .map(normalize_mask)
        .collect()
}

pub(crate) fn presets_for_tui(thread_manager: &ThreadManager) -> Vec<IdentityMask> {
    filtered_presets(thread_manager)
}

pub(crate) fn default_mask(thread_manager: &ThreadManager) -> Option<IdentityMask> {
    let presets = filtered_presets(thread_manager);
    presets
        .iter()
        .find(|mask| mask.kind == Some(IdentityKind::Nobody))
        .cloned()
        .or_else(|| presets.into_iter().next())
}

pub(crate) fn mask_for_kind(
    thread_manager: &ThreadManager,
    kind: IdentityKind,
) -> Option<IdentityMask> {
    if !is_tui_identity(kind) {
        return None;
    }
    filtered_presets(thread_manager)
        .into_iter()
        .find(|mask| mask.kind == Some(kind))
}

pub(crate) fn programmer_mask(thread_manager: &ThreadManager) -> Option<IdentityMask> {
    mask_for_kind(thread_manager, IdentityKind::Programmer)
}
