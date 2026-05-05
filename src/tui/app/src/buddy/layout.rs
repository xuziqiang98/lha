use unicode_width::UnicodeWidthStr;

use super::model::Buddy;
use super::sprites;
use super::state::BuddyState;

pub(crate) const BUDDY_MIN_FULL_WIDTH: u16 = 100;
pub(crate) const SPRITE_BODY_WIDTH: u16 = sprites::SPRITE_WIDTH as u16;
pub(crate) const SPRITE_HEIGHT: u16 = 5;
pub(crate) const NAME_HEIGHT: u16 = 1;
pub(crate) const PET_EXTRA_HEIGHT: u16 = 1;
pub(crate) const NAME_ROW_PAD: u16 = 2;
pub(crate) const SPRITE_PADDING_X: u16 = 2;
pub(crate) const BUBBLE_WIDTH: u16 = 24;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuddyLayoutMode {
    Hidden,
    FullSidebar,
}

pub(crate) fn layout_mode(state: &BuddyState, terminal_width: u16) -> BuddyLayoutMode {
    if !state.is_visible() {
        BuddyLayoutMode::Hidden
    } else if terminal_width >= BUDDY_MIN_FULL_WIDTH {
        BuddyLayoutMode::FullSidebar
    } else {
        BuddyLayoutMode::Hidden
    }
}

pub(crate) fn reserved_width(state: &BuddyState, terminal_width: u16) -> u16 {
    match layout_mode(state, terminal_width) {
        BuddyLayoutMode::FullSidebar => {
            full_reserved_width(state.buddy(), state.visible_reaction().is_some())
        }
        BuddyLayoutMode::Hidden => 0,
    }
}

pub(crate) fn sprite_column_width(name_width: u16) -> u16 {
    SPRITE_BODY_WIDTH.max(name_width.saturating_add(NAME_ROW_PAD))
}

pub(crate) fn full_reserved_width(buddy: Option<&Buddy>, has_reaction: bool) -> u16 {
    let Some(buddy) = buddy else {
        return 0;
    };
    let name_width = UnicodeWidthStr::width(buddy.name.as_str()) as u16;
    let bubble_width = if has_reaction { BUBBLE_WIDTH } else { 0 };
    bubble_width + sprite_column_width(name_width) + SPRITE_PADDING_X
}

pub(crate) fn full_required_height(state: &BuddyState) -> u16 {
    let pet_height = if state.pet_active() {
        PET_EXTRA_HEIGHT
    } else {
        0
    };
    SPRITE_HEIGHT + NAME_HEIGHT + pet_height
}

#[cfg(test)]
mod tests {
    use adam_agent::config::types::TuiBuddy;

    use super::*;

    fn visible_state() -> BuddyState {
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.ensure_active_buddy();
        state
    }

    #[test]
    fn hides_buddy_below_full_width() {
        let state = visible_state();
        assert_eq!(
            layout_mode(&state, BUDDY_MIN_FULL_WIDTH - 1),
            BuddyLayoutMode::Hidden
        );
    }

    #[test]
    fn shows_full_buddy_at_full_width() {
        let state = visible_state();
        assert_eq!(
            layout_mode(&state, BUDDY_MIN_FULL_WIDTH),
            BuddyLayoutMode::FullSidebar
        );
    }

    #[test]
    fn full_required_height_includes_pet_row() {
        let mut state = visible_state();
        assert_eq!(full_required_height(&state), SPRITE_HEIGHT + NAME_HEIGHT);
        state.pet();
        assert_eq!(
            full_required_height(&state),
            SPRITE_HEIGHT + NAME_HEIGHT + PET_EXTRA_HEIGHT
        );
    }
}
