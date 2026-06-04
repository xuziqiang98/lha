use crate::product::tui_app::color::blend;
use crate::product::tui_app::color::is_light;
use crate::product::tui_app::terminal_palette::best_color;
use crate::product::tui_app::terminal_palette::best_color_distinct_from;
use crate::product::tui_app::terminal_palette::default_bg;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;

pub fn user_message_style() -> Style {
    user_message_style_for(default_bg())
}

pub fn proposed_plan_style() -> Style {
    proposed_plan_style_for(default_bg())
}

/// Returns the style for a user-authored message using the provided terminal background.
pub fn user_message_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    match terminal_bg {
        Some(bg) => Style::default().bg(user_message_bg(bg)),
        None => Style::default(),
    }
}

pub fn proposed_plan_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    match terminal_bg {
        Some(bg) => Style::default().bg(proposed_plan_bg(bg)),
        None => Style::default(),
    }
}

pub fn transcript_selection_style() -> Style {
    match default_bg() {
        Some(bg) => Style::default().bg(transcript_selection_bg(bg)),
        None => Style::default().reversed(),
    }
}

#[allow(clippy::disallowed_methods)]
pub fn user_message_bg(terminal_bg: (u8, u8, u8)) -> Color {
    best_color(user_message_bg_rgb(terminal_bg))
}

fn user_message_bg_rgb(terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let (top, alpha) = if is_light(terminal_bg) {
        ((0, 0, 0), 0.04)
    } else {
        ((255, 255, 255), 0.12)
    };
    blend(top, terminal_bg, alpha)
}

#[allow(clippy::disallowed_methods)]
pub fn proposed_plan_bg(terminal_bg: (u8, u8, u8)) -> Color {
    best_color(proposed_plan_bg_rgb(terminal_bg))
}

fn proposed_plan_bg_rgb(terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
    user_message_bg_rgb(terminal_bg)
}

#[allow(clippy::disallowed_methods)]
pub fn transcript_selection_bg(terminal_bg: (u8, u8, u8)) -> Color {
    best_color_distinct_from(transcript_selection_bg_rgb(terminal_bg), terminal_bg)
}

fn transcript_selection_bg_rgb(terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let (top, alpha) = if is_light(terminal_bg) {
        ((0, 92, 128), 0.18)
    } else {
        ((0, 68, 96), 0.38)
    };
    blend(top, terminal_bg, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::tui_app::terminal_palette::nearest_xterm_fixed_color;
    use crate::product::tui_app::terminal_palette::nearest_xterm_fixed_color_distinct_from;

    const SAMPLE_BACKGROUNDS: &[(u8, u8, u8)] = &[
        (0, 0, 0),
        (24, 24, 24),
        (48, 48, 48),
        (80, 80, 80),
        (120, 120, 120),
        (240, 240, 240),
        (255, 255, 255),
    ];

    fn luminance((r, g, b): (u8, u8, u8)) -> f32 {
        0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
    }

    #[test]
    fn transcript_selection_background_is_darker_than_user_message_background() {
        for &terminal_bg in SAMPLE_BACKGROUNDS {
            let selection_bg = transcript_selection_bg_rgb(terminal_bg);
            let user_bg = user_message_bg_rgb(terminal_bg);

            assert!(
                luminance(selection_bg) < luminance(user_bg),
                "expected selection bg {selection_bg:?} to be darker than user bg {user_bg:?} for terminal bg {terminal_bg:?}"
            );
        }
    }

    #[test]
    fn transcript_selection_background_is_darker_than_proposed_plan_background() {
        for &terminal_bg in SAMPLE_BACKGROUNDS {
            let selection_bg = transcript_selection_bg_rgb(terminal_bg);
            let plan_bg = proposed_plan_bg_rgb(terminal_bg);

            assert!(
                luminance(selection_bg) < luminance(plan_bg),
                "expected selection bg {selection_bg:?} to be darker than plan bg {plan_bg:?} for terminal bg {terminal_bg:?}"
            );
        }
    }

    #[test]
    fn transcript_selection_background_remains_distinct_from_terminal_background() {
        for &terminal_bg in SAMPLE_BACKGROUNDS {
            let selection_bg = transcript_selection_bg_rgb(terminal_bg);

            assert_ne!(
                selection_bg, terminal_bg,
                "expected selection bg to differ from terminal bg {terminal_bg:?}"
            );
        }
    }

    #[test]
    fn transcript_selection_xterm_background_is_distinct_from_terminal_background() {
        for &terminal_bg in SAMPLE_BACKGROUNDS {
            let selection_bg = transcript_selection_bg_rgb(terminal_bg);
            let (selection_idx, _) =
                nearest_xterm_fixed_color_distinct_from(selection_bg, terminal_bg)
                    .expect("selection color");
            let (terminal_idx, _) = nearest_xterm_fixed_color(terminal_bg).expect("terminal color");

            assert_ne!(
                selection_idx, terminal_idx,
                "expected xterm selection bg {selection_bg:?} to avoid terminal bg {terminal_bg:?}"
            );
        }
    }

    #[test]
    fn transcript_selection_xterm_background_is_darker_than_user_message_background() {
        for &terminal_bg in SAMPLE_BACKGROUNDS {
            let selection_bg = transcript_selection_bg_rgb(terminal_bg);
            let (_, selection_rgb) =
                nearest_xterm_fixed_color_distinct_from(selection_bg, terminal_bg)
                    .expect("selection color");
            let (_, user_rgb) =
                nearest_xterm_fixed_color(user_message_bg_rgb(terminal_bg)).expect("user color");

            assert!(
                luminance(selection_rgb) < luminance(user_rgb),
                "expected xterm selection bg {selection_rgb:?} to be darker than user bg {user_rgb:?} for terminal bg {terminal_bg:?}"
            );
        }
    }

    #[test]
    fn transcript_selection_xterm_background_is_darker_than_proposed_plan_background() {
        for &terminal_bg in SAMPLE_BACKGROUNDS {
            let selection_bg = transcript_selection_bg_rgb(terminal_bg);
            let (_, selection_rgb) =
                nearest_xterm_fixed_color_distinct_from(selection_bg, terminal_bg)
                    .expect("selection color");
            let (_, plan_rgb) =
                nearest_xterm_fixed_color(proposed_plan_bg_rgb(terminal_bg)).expect("plan color");

            assert!(
                luminance(selection_rgb) < luminance(plan_rgb),
                "expected xterm selection bg {selection_rgb:?} to be darker than plan bg {plan_rgb:?} for terminal bg {terminal_bg:?}"
            );
        }
    }
}
