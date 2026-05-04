use std::time::Duration;
use std::time::Instant;

use adam_agent::config::types::TuiBuddy;
use adam_protocol::config_types::IdentityKind;
use rand::SeedableRng;
use rand::rngs::StdRng;

use super::model;
use super::model::Buddy;

const PET_BURST: Duration = Duration::from_millis(2500);
const REACTION_SHOW: Duration = Duration::from_secs(10);
const REACTION_FADE_START: Duration = Duration::from_secs(7);
const IDLE_SEQUENCE: [Option<usize>; 15] = [
    Some(0),
    Some(0),
    Some(0),
    Some(0),
    Some(1),
    Some(0),
    Some(0),
    Some(0),
    None,
    Some(0),
    Some(0),
    Some(2),
    Some(0),
    Some(0),
    Some(0),
];
const HEART_FRAMES: [&str; 5] = [
    "   ♥    ♥   ",
    "  ♥  ♥   ♥  ",
    " ♥   ♥  ♥   ",
    "♥  ♥      ♥ ",
    "·    ·   ·  ",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuddyAnimationMode {
    Idle,
    Speaking,
    Petting,
}

#[derive(Debug, Clone)]
pub(crate) struct BuddyReaction {
    pub(crate) text: String,
    pub(crate) received_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct BuddyState {
    idle_animation_started_at: Instant,
    reaction_animation_started_at: Option<Instant>,
    pet_animation_started_at: Option<Instant>,
    config: TuiBuddy,
    active_identity_kind: IdentityKind,
    buddies: std::collections::HashMap<IdentityKind, Buddy>,
    rng: StdRng,
    pet_started_at: Option<Instant>,
    reaction: Option<BuddyReaction>,
}

impl Default for BuddyState {
    fn default() -> Self {
        Self {
            idle_animation_started_at: Instant::now(),
            reaction_animation_started_at: None,
            pet_animation_started_at: None,
            config: TuiBuddy {
                enabled: true,
                ..TuiBuddy::default()
            },
            active_identity_kind: IdentityKind::Nobody,
            buddies: std::collections::HashMap::new(),
            rng: default_rng(),
            pet_started_at: None,
            reaction: None,
        }
    }
}

#[cfg(test)]
fn default_rng() -> StdRng {
    StdRng::seed_from_u64(0xAD_A0_BD_D1)
}

#[cfg(not(test))]
fn default_rng() -> StdRng {
    StdRng::from_os_rng()
}

impl BuddyState {
    pub(crate) fn config(&self) -> &TuiBuddy {
        &self.config
    }

    pub(crate) fn set_config(&mut self, config: TuiBuddy) {
        self.config = config;
        if !self.is_visible() {
            self.reaction = None;
        }
    }

    pub(crate) fn set_identity_kind(&mut self, identity_kind: IdentityKind) {
        if self.active_identity_kind == identity_kind {
            self.ensure_active_buddy();
            return;
        }
        self.active_identity_kind = identity_kind;
        self.reaction = None;
        self.pet_started_at = None;
        self.pet_animation_started_at = None;
        self.reaction_animation_started_at = None;
        self.ensure_active_buddy();
    }

    pub(crate) fn ensure_active_buddy(&mut self) {
        self.buddies
            .entry(self.active_identity_kind)
            .or_insert_with(|| model::generate_buddy(self.active_identity_kind, &mut self.rng));
    }

    pub(crate) fn buddy(&self) -> Option<&Buddy> {
        self.buddies.get(&self.active_identity_kind)
    }

    pub(crate) fn is_hatched(&self) -> bool {
        self.buddy().is_some()
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.config.enabled && !self.config.muted && self.is_hatched()
    }

    pub(crate) fn pet(&mut self) {
        let now = Instant::now();
        self.pet_started_at = Some(now);
        self.pet_animation_started_at = Some(now);
    }

    pub(crate) fn pet_active(&self) -> bool {
        self.pet_started_at
            .is_some_and(|started_at| started_at.elapsed() < PET_BURST)
    }

    pub(crate) fn set_reaction(&mut self, text: String) {
        let text = text.trim().replace(['\n', '\r'], " ");
        if text.is_empty() || !self.is_visible() {
            return;
        }
        let now = Instant::now();
        self.reaction = Some(BuddyReaction {
            text,
            received_at: now,
        });
        self.reaction_animation_started_at = Some(now);
    }

    pub(crate) fn visible_reaction(&self) -> Option<&BuddyReaction> {
        self.reaction
            .as_ref()
            .filter(|reaction| reaction.received_at.elapsed() < REACTION_SHOW)
    }

    pub(crate) fn reaction_fading(&self) -> bool {
        self.visible_reaction()
            .is_some_and(|reaction| reaction.received_at.elapsed() >= REACTION_FADE_START)
    }

    pub(crate) fn animation_mode(&self) -> BuddyAnimationMode {
        if self.pet_active() {
            BuddyAnimationMode::Petting
        } else if self.visible_reaction().is_some() {
            BuddyAnimationMode::Speaking
        } else {
            BuddyAnimationMode::Idle
        }
    }

    pub(crate) fn sprite_frame(
        &self,
        frame_count: usize,
        animations_enabled: bool,
    ) -> (usize, bool) {
        if !animations_enabled || frame_count <= 1 {
            return (0, false);
        }
        match self.animation_mode() {
            BuddyAnimationMode::Speaking => {
                let started_at = self
                    .reaction_animation_started_at
                    .unwrap_or(self.idle_animation_started_at);
                let tick = (started_at.elapsed().as_millis() / 500) as usize;
                (tick % frame_count, false)
            }
            BuddyAnimationMode::Petting => {
                let started_at = self
                    .pet_animation_started_at
                    .unwrap_or(self.idle_animation_started_at);
                let tick = (started_at.elapsed().as_millis() / 500) as usize;
                (tick % frame_count, false)
            }
            BuddyAnimationMode::Idle => {
                let tick = (self.idle_animation_started_at.elapsed().as_millis() / 500) as usize;
                match IDLE_SEQUENCE[tick % IDLE_SEQUENCE.len()] {
                    Some(frame) => (frame % frame_count, false),
                    None => (0, true),
                }
            }
        }
    }

    pub(crate) fn pet_heart_frame(&self) -> Option<&'static str> {
        let started_at = self.pet_started_at?;
        if started_at.elapsed() >= PET_BURST {
            return None;
        }
        let tick = (started_at.elapsed().as_millis() / 500) as usize;
        Some(HEART_FRAMES[tick % HEART_FRAMES.len()])
    }
}

#[cfg(test)]
mod tests {
    use adam_agent::config::types::TuiBuddy;
    use adam_protocol::config_types::IdentityKind;

    use super::*;

    fn visible_buddy_state() -> BuddyState {
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            name: None,
            species: None,
            eye: None,
            hat: None,
            rarity: None,
            shiny: None,
            observer: Default::default(),
        });
        state.ensure_active_buddy();
        state
    }

    #[test]
    fn sprite_frame_stays_static_when_animations_disabled() {
        let state = visible_buddy_state();
        assert_eq!(state.sprite_frame(3, false), (0, false));
    }

    #[test]
    fn sprite_frame_stays_static_for_single_frame_sprites() {
        let state = visible_buddy_state();
        assert_eq!(state.sprite_frame(1, true), (0, false));
    }

    #[test]
    fn speaking_animation_advances_frames_with_tick() {
        let mut state = visible_buddy_state();
        state.set_reaction("hi".to_string());

        assert_eq!(state.animation_mode(), BuddyAnimationMode::Speaking);
        assert!(state.reaction_animation_started_at.is_some());
    }

    #[test]
    fn idle_animation_can_blink() {
        let state = visible_buddy_state();
        assert_eq!(state.animation_mode(), BuddyAnimationMode::Idle);
        let tick = 8;
        let frame = match IDLE_SEQUENCE[tick % IDLE_SEQUENCE.len()] {
            Some(frame) => (frame % 3, false),
            None => (0, true),
        };
        assert_eq!(frame, (0, true));
    }

    #[test]
    fn pet_resets_pet_animation_start() {
        let mut state = visible_buddy_state();
        state.pet();

        assert_eq!(state.animation_mode(), BuddyAnimationMode::Petting);
        assert!(state.pet_animation_started_at.is_some());
    }

    #[test]
    fn switching_identity_caches_buddies_for_session() {
        let mut state = visible_buddy_state();
        let nobody = state.buddy().expect("nobody buddy").clone();

        state.set_identity_kind(IdentityKind::Planner);
        let planner = state.buddy().expect("planner buddy").clone();
        assert_eq!(planner.identity_kind, IdentityKind::Planner);

        state.set_identity_kind(IdentityKind::Nobody);
        assert_eq!(state.buddy(), Some(&nobody));
        assert_ne!(state.buddy(), Some(&planner));
    }
}
