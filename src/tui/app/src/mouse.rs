use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    const fn sign(self) -> isize {
        match self {
            ScrollDirection::Up => -1,
            ScrollDirection::Down => 1,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct MouseScrollState {
    last_event_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollUpdate {
    pub(crate) delta_lines: isize,
}

impl MouseScrollState {
    pub(crate) fn on_scroll(&mut self, direction: ScrollDirection) -> ScrollUpdate {
        let now = Instant::now();
        let is_trackpad = self
            .last_event_at
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(35));
        self.last_event_at = Some(now);

        let lines_per_tick = if is_trackpad { 1 } else { 3 };
        ScrollUpdate {
            delta_lines: direction.sign() * lines_per_tick,
        }
    }
}
