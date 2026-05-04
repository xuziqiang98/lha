use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::bubble;
use super::layout;
use super::model::DEFAULT_BUDDY_RARITY;
use super::sprites;
use super::state::BuddyState;
use super::style;

pub(crate) fn reserved_width(state: &BuddyState, terminal_width: u16) -> u16 {
    layout::reserved_width(state, terminal_width)
}

pub(crate) fn render_buddy(
    area: Rect,
    buf: &mut Buffer,
    state: &BuddyState,
    animations_enabled: bool,
) {
    if area.width == 0 || area.height < 2 || !state.is_visible() {
        return;
    }
    let Some(buddy) = state.buddy() else {
        return;
    };
    let sprite_color = style::rarity_color(buddy.rarity);
    let name_width = UnicodeWidthStr::width(buddy.name.as_str()) as u16;
    let sprite_width = layout::sprite_column_width(name_width);
    let sprite_area = if let Some(reaction) = state.visible_reaction() {
        let [bubble_area, sprite_area] = Layout::horizontal([
            Constraint::Length(layout::BUBBLE_WIDTH),
            Constraint::Length(sprite_width + layout::SPRITE_PADDING_X),
        ])
        .areas(area);
        bubble::render_bubble(bubble_area, buf, &reaction.text, state.reaction_fading());
        sprite_area
    } else {
        area
    };
    let sprite_column_area = centered_column(sprite_area, sprite_width);

    let frame_count = sprites::sprite_frame_count(buddy.species);
    let (frame, blink) = state.sprite_frame(frame_count, animations_enabled);
    let sprite = sprites::render_sprite(buddy.species, buddy.eye, buddy.hat, blink, frame);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(hearts) = state.pet_heart_frame() {
        lines.push(centered_line(
            hearts.to_string(),
            sprite_column_area.width,
            Style::default().magenta(),
        ));
    }
    for line in sprite {
        lines.push(centered_sprite_line(
            line,
            sprite_column_area.width,
            Style::default().fg(sprite_color),
        ));
    }
    let display_name = truncate_to_width(
        &buddy.name,
        sprite_column_area.width.saturating_sub(2) as usize,
    );
    let name = if buddy.rarity == DEFAULT_BUDDY_RARITY && !buddy.shiny {
        centered_line(
            display_name,
            sprite_column_area.width,
            Style::default().italic().dim(),
        )
    } else {
        centered_line(
            display_name,
            sprite_column_area.width,
            Style::default().bold().fg(sprite_color),
        )
    };
    lines.push(name);
    ratatui::widgets::Paragraph::new(lines).render(sprite_column_area, buf);
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

fn centered_column(area: Rect, width: u16) -> Rect {
    let width = width.min(area.width);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y,
        width,
        area.height,
    )
}

fn centered_sprite_line(text: String, width: u16, style: Style) -> Line<'static> {
    centered_line(text.trim_end().to_string(), width, style)
}

fn centered_line(text: String, width: u16, style: Style) -> Line<'static> {
    let text_width = UnicodeWidthStr::width(text.as_str()) as u16;
    let left_pad = width.saturating_sub(text_width) / 2;
    Line::from(vec![
        Span::from(" ".repeat(usize::from(left_pad))),
        Span::from(text).style(style),
    ])
}

#[cfg(test)]
mod tests {
    use adam_agent::config::types::TuiBuddy;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn reserves_width_only_when_visible_and_wide() {
        let state = BuddyState::default();
        assert_eq!(reserved_width(&state, layout::BUDDY_MIN_FULL_WIDTH), 0);

        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.ensure_active_buddy();

        assert_eq!(reserved_width(&state, layout::BUDDY_MIN_FULL_WIDTH - 1), 0);
        let width = reserved_width(&state, layout::BUDDY_MIN_FULL_WIDTH);
        assert!(width >= layout::SPRITE_BODY_WIDTH + layout::SPRITE_PADDING_X);

        state.set_reaction("hello".to_string());
        assert_eq!(
            reserved_width(&state, layout::BUDDY_MIN_FULL_WIDTH),
            width + layout::BUBBLE_WIDTH
        );
    }

    #[test]
    fn render_buddy_renders_full_layout() {
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.ensure_active_buddy();
        let name = state.buddy().expect("generated buddy").name.clone();

        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);

        render_buddy(area, &mut buf, &state, false);

        let rendered = buf
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains(&name));
        let non_empty_rows = (0..area.height)
            .filter(|row| (0..area.width).any(|col| !buf[(col, *row)].symbol().trim().is_empty()))
            .count();
        assert!(non_empty_rows > 1);
    }

    #[test]
    fn render_buddy_centers_name() {
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.ensure_active_buddy();
        let name = state.buddy().expect("generated buddy").name.clone();

        let area = Rect::new(0, 0, 24, 6);
        let mut buf = Buffer::empty(area);

        render_buddy(area, &mut buf, &state, false);

        let name_row = area.height - 1;
        let row = (0..area.width)
            .map(|x| buf[(x, name_row)].symbol().chars().next().unwrap_or(' '))
            .collect::<String>();
        let name_x = row.find(&name).expect("buddy name rendered") as u16;
        let expected_x = (area.width - UnicodeWidthStr::width(name.as_str()) as u16) / 2;

        assert_eq!(name_x, expected_x);
    }
}
