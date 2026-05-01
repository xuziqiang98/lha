use std::time::Duration;
use std::time::Instant;

use adam_agent::config::types::TuiBuddy;

use super::model::Buddy;

const PET_BURST: Duration = Duration::from_millis(2500);
const REACTION_SHOW: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct BuddyReaction {
    pub(crate) text: String,
    pub(crate) received_at: Instant,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BuddyState {
    config: TuiBuddy,
    pet_started_at: Option<Instant>,
    reaction: Option<BuddyReaction>,
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

    pub(crate) fn buddy(&self) -> Option<Buddy> {
        Buddy::from_config(&self.config)
    }

    pub(crate) fn is_hatched(&self) -> bool {
        self.buddy().is_some()
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.config.enabled && !self.config.muted && self.is_hatched()
    }

    pub(crate) fn pet(&mut self) {
        self.pet_started_at = Some(Instant::now());
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
        self.reaction = Some(BuddyReaction {
            text,
            received_at: Instant::now(),
        });
    }

    pub(crate) fn visible_reaction(&self) -> Option<&BuddyReaction> {
        self.reaction
            .as_ref()
            .filter(|reaction| reaction.received_at.elapsed() < REACTION_SHOW)
    }

    pub(crate) fn reaction_fading(&self) -> bool {
        self.visible_reaction()
            .is_some_and(|reaction| reaction.received_at.elapsed() >= Duration::from_secs(7))
    }
}
