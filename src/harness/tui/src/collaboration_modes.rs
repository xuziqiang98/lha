use codex_agent::ThreadManager;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;

fn is_tui_mode(kind: ModeKind) -> bool {
    matches!(kind, ModeKind::Plan | ModeKind::Code)
}

pub(crate) fn normalize_mask(mut mask: CollaborationModeMask) -> CollaborationModeMask {
    if mask.mode.is_some_and(is_tui_mode) {
        mask.model = None;
        mask.reasoning_effort = None;
    }
    mask
}

fn filtered_presets(thread_manager: &ThreadManager) -> Vec<CollaborationModeMask> {
    thread_manager
        .list_collaboration_modes()
        .into_iter()
        .filter(|mask| mask.mode.is_some_and(is_tui_mode))
        .map(normalize_mask)
        .collect()
}

pub(crate) fn presets_for_tui(thread_manager: &ThreadManager) -> Vec<CollaborationModeMask> {
    filtered_presets(thread_manager)
}

pub(crate) fn default_mask(thread_manager: &ThreadManager) -> Option<CollaborationModeMask> {
    let presets = filtered_presets(thread_manager);
    presets
        .iter()
        .find(|mask| mask.mode == Some(ModeKind::Code))
        .cloned()
        .or_else(|| presets.into_iter().next())
}

pub(crate) fn mask_for_kind(
    thread_manager: &ThreadManager,
    kind: ModeKind,
) -> Option<CollaborationModeMask> {
    if !is_tui_mode(kind) {
        return None;
    }
    filtered_presets(thread_manager)
        .into_iter()
        .find(|mask| mask.mode == Some(kind))
}

/// Cycle to the next collaboration mode preset in list order.
pub(crate) fn next_mask(
    thread_manager: &ThreadManager,
    current: Option<&CollaborationModeMask>,
) -> Option<CollaborationModeMask> {
    let presets = filtered_presets(thread_manager);
    if presets.is_empty() {
        return None;
    }
    let current_kind = current.and_then(|mask| mask.mode);
    let next_index = presets
        .iter()
        .position(|mask| mask.mode == current_kind)
        .map_or(0, |idx| (idx + 1) % presets.len());
    presets.get(next_index).cloned()
}

pub(crate) fn code_mask(thread_manager: &ThreadManager) -> Option<CollaborationModeMask> {
    mask_for_kind(thread_manager, ModeKind::Code)
}

pub(crate) fn plan_mask(thread_manager: &ThreadManager) -> Option<CollaborationModeMask> {
    mask_for_kind(thread_manager, ModeKind::Plan)
}
