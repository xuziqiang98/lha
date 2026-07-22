//! Limits how frequently frame draw notifications may be emitted.
//!
//! Widgets sometimes call `FrameRequester::schedule_frame()` more frequently than a user can
//! perceive. This limiter clamps draw notifications to the terminal-specific maximum frame rate.
//!
//! This is intentionally a small, pure helper so it can be unit-tested in isolation and used by
//! the async frame scheduler without adding complexity to the app/event loop.

use std::time::Duration;
use std::time::Instant;

/// The default 60 FPS minimum frame interval (about 16.67ms).
pub(super) const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// Remembers the most recent emitted draw, allowing deadlines to be clamped forward.
#[derive(Debug)]
pub(super) struct FrameRateLimiter {
    last_emitted_at: Option<Instant>,
    min_frame_interval: Duration,
}

impl FrameRateLimiter {
    pub(super) const fn new(min_frame_interval: Duration) -> Self {
        Self {
            last_emitted_at: None,
            min_frame_interval,
        }
    }

    /// Returns `requested`, clamped forward if it would exceed the maximum frame rate.
    pub(super) fn clamp_deadline(&self, requested: Instant) -> Instant {
        let Some(last_emitted_at) = self.last_emitted_at else {
            return requested;
        };
        let min_allowed = last_emitted_at
            .checked_add(self.min_frame_interval)
            .unwrap_or(last_emitted_at);
        requested.max(min_allowed)
    }

    /// Records that a draw notification was emitted at `emitted_at`.
    pub(super) fn mark_emitted(&mut self, emitted_at: Instant) {
        self.last_emitted_at = Some(emitted_at);
    }
}

impl Default for FrameRateLimiter {
    fn default() -> Self {
        Self::new(MIN_FRAME_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn default_does_not_clamp() {
        let t0 = Instant::now();
        let limiter = FrameRateLimiter::default();
        assert_eq!(limiter.clamp_deadline(t0), t0);
    }

    #[test]
    fn clamps_to_min_interval_since_last_emit() {
        let t0 = Instant::now();
        let mut limiter = FrameRateLimiter::default();

        assert_eq!(limiter.clamp_deadline(t0), t0);
        limiter.mark_emitted(t0);

        let too_soon = t0 + Duration::from_millis(1);
        assert_eq!(limiter.clamp_deadline(too_soon), t0 + MIN_FRAME_INTERVAL);
    }

    #[test]
    fn honors_supplied_min_interval() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(33);
        let mut limiter = FrameRateLimiter::new(interval);
        limiter.mark_emitted(t0);

        assert_eq!(
            limiter.clamp_deadline(t0 + Duration::from_millis(1)),
            t0 + interval
        );
    }

    #[test]
    fn delayed_emission_limits_follow_up_from_actual_emit_time() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(33);
        let mut limiter = FrameRateLimiter::new(interval);
        let actual_emit = t0 + Duration::from_millis(100);
        limiter.mark_emitted(actual_emit);

        assert_eq!(
            limiter.clamp_deadline(t0 + Duration::from_millis(101)),
            actual_emit + interval
        );
    }
}
