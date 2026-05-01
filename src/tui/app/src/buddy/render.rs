use adam_agent::config::types::BuddySpecies;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::state::BuddyState;

pub(crate) const BUDDY_MIN_TERMINAL_WIDTH: u16 = 84;
pub(crate) const BUDDY_WIDTH: u16 = 18;

pub(crate) fn reserved_width(state: &BuddyState, terminal_width: u16) -> u16 {
    if state.is_visible() && terminal_width >= BUDDY_MIN_TERMINAL_WIDTH {
        BUDDY_WIDTH
    } else {
        0
    }
}

pub(crate) fn render_buddy(
    area: Rect,
    buf: &mut Buffer,
    state: &BuddyState,
    animations_enabled: bool,
) {
    if area.width == 0 || area.height < 3 || !state.is_visible() {
        return;
    }
    let Some(buddy) = state.buddy() else {
        return;
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(reaction) = state.visible_reaction() {
        let reaction = truncate_to_width(&reaction.text, area.width.saturating_sub(2) as usize);
        let span = if state.reaction_fading() {
            format!("“{reaction}”").dim()
        } else {
            format!("“{reaction}”").magenta()
        };
        lines.push(Line::from(span));
    }

    let name = truncate_to_width(&buddy.name, area.width.saturating_sub(2) as usize);
    let name = if state.pet_active() {
        Line::from(vec!["♥ ".magenta(), name.bold()])
    } else {
        Line::from(name.bold())
    };
    lines.push(name);
    lines.push(Line::from(
        face_for(buddy.species, animations_enabled).cyan(),
    ));
    lines.push(Line::from(body_for(buddy.species).dim()));

    let visible_lines = lines
        .into_iter()
        .take(area.height as usize)
        .collect::<Vec<_>>();
    ratatui::widgets::Paragraph::new(visible_lines).render(area, buf);
}

fn face_for(species: BuddySpecies, animations_enabled: bool) -> &'static str {
    let blink = animations_enabled && (chrono::Utc::now().timestamp_millis() / 500) % 8 == 0;
    match (species, blink) {
        (BuddySpecies::Duck, false) => "(•ᴗ•)",
        (BuddySpecies::Duck, true) => "(-ᴗ-)",
        (BuddySpecies::Cat, false) => "=^•ᴗ•^=",
        (BuddySpecies::Cat, true) => "=^-ᴗ-^=",
        (BuddySpecies::Blob, false) => "( ᵔᴗᵔ )",
        (BuddySpecies::Blob, true) => "( -ᴗ- )",
        (BuddySpecies::Robot, false) => "[•_•]",
        (BuddySpecies::Robot, true) => "[-_-]",
        (BuddySpecies::Turtle, false) => "(•‿•)>",
        (BuddySpecies::Turtle, true) => "(-‿-)>",
    }
}

fn body_for(species: BuddySpecies) -> &'static str {
    match species {
        BuddySpecies::Duck => " /|_|\\",
        BuddySpecies::Cat => "  /| |\\",
        BuddySpecies::Blob => "  (___)",
        BuddySpecies::Robot => "  /|_|\\",
        BuddySpecies::Turtle => "  /___\\",
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let ellipsis = "…";
    let target = max_width.saturating_sub(UnicodeWidthStr::width(ellipsis));
    let mut out = String::new();
    for ch in text.chars() {
        let next_width =
            UnicodeWidthStr::width(out.as_str()) + UnicodeWidthStr::width(ch.to_string().as_str());
        if next_width > target {
            break;
        }
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use adam_agent::config::types::BuddySpecies;
    use adam_agent::config::types::TuiBuddy;

    use super::*;

    #[test]
    fn reserves_width_only_when_visible_and_wide() {
        let state = BuddyState::default();
        assert_eq!(reserved_width(&state, BUDDY_MIN_TERMINAL_WIDTH), 0);

        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            name: Some("Byte".to_string()),
            species: Some(BuddySpecies::Duck),
            observer: Default::default(),
        });

        assert_eq!(reserved_width(&state, BUDDY_MIN_TERMINAL_WIDTH - 1), 0);
        assert_eq!(
            reserved_width(&state, BUDDY_MIN_TERMINAL_WIDTH),
            BUDDY_WIDTH
        );
    }
}
