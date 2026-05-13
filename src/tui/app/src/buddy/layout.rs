use unicode_width::UnicodeWidthStr;

use super::model::Buddy;
use super::sprites;
use super::state::BuddyState;

pub(crate) const BUDDY_MIN_FULL_WIDTH: u16 = 100;
pub(crate) const SPRITE_BODY_WIDTH: u16 = sprites::SPRITE_WIDTH as u16;
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

pub(crate) fn sprite_rendered_height(buddy: &Buddy) -> u16 {
    sprites::rendered_sprite_height(buddy.species, buddy.hat)
}

pub(crate) fn full_required_height(state: &BuddyState) -> u16 {
    let Some(buddy) = state.buddy() else {
        return 0;
    };
    let pet_height = if state.pet_active() {
        PET_EXTRA_HEIGHT
    } else {
        0
    };
    sprite_rendered_height(buddy) + NAME_HEIGHT + pet_height
}

#[cfg(test)]
mod tests {
    use adam_agent::config::types::BuddyEye;
    use adam_agent::config::types::BuddyHat;
    use adam_agent::config::types::BuddyRarity;
    use adam_agent::config::types::BuddySpecies;
    use adam_agent::config::types::TuiBuddy;
    use adam_protocol::config_types::IdentityKind;

    use super::*;
    use crate::buddy::model::BuddyStats;

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

    fn buddy_with(species: BuddySpecies, hat: BuddyHat) -> Buddy {
        Buddy {
            name: "Quill".to_string(),
            species,
            eye: BuddyEye::Degree,
            hat,
            rarity: BuddyRarity::Common,
            shiny: false,
            personality: "diff enthusiast".to_string(),
            stats: BuddyStats {
                debugging: 50,
                patience: 50,
                chaos: 50,
                wisdom: 50,
                snark: 50,
            },
            identity_kind: IdentityKind::Nobody,
        }
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
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.set_buddy_for_test(buddy_with(BuddySpecies::Duck, BuddyHat::None));

        assert_eq!(full_required_height(&state), 4 + NAME_HEIGHT);

        state.set_buddy_for_test(buddy_with(BuddySpecies::Duck, BuddyHat::TopHat));
        assert_eq!(full_required_height(&state), 5 + NAME_HEIGHT);

        state.pet();
        assert_eq!(
            full_required_height(&state),
            5 + NAME_HEIGHT + PET_EXTRA_HEIGHT
        );
    }
}
