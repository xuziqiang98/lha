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
    let reaction = state.visible_reaction();
    let content_width = if reaction.is_some() {
        layout::BUBBLE_WIDTH
            .saturating_add(sprite_width)
            .saturating_add(layout::SPRITE_PADDING_X)
    } else {
        sprite_width.saturating_add(layout::SPRITE_PADDING_X)
    }
    .min(area.width);
    let content_area = Rect::new(
        area.x
            .saturating_add(area.width.saturating_sub(content_width)),
        area.y,
        content_width,
        area.height,
    );
    let sprite_area = if let Some(reaction) = reaction {
        let [bubble_area, sprite_area] = Layout::horizontal([
            Constraint::Length(layout::BUBBLE_WIDTH),
            Constraint::Length(sprite_width + layout::SPRITE_PADDING_X),
        ])
        .areas(content_area);
        bubble::render_bubble(
            bubble_area,
            buf,
            &reaction.text,
            state.reaction_fading(),
            sprite_color,
        );
        sprite_area
    } else {
        content_area
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
        area.x.saturating_add(area.width.saturating_sub(width)),
        area.y,
        width,
        area.height,
    )
}

fn centered_sprite_line(text: String, width: u16, style: Style) -> Line<'static> {
    centered_line(text, width, style)
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
    use crate::product::agent::config::types::BuddyEye;
    use crate::product::agent::config::types::BuddyHat;
    use crate::product::agent::config::types::BuddyRarity;
    use crate::product::agent::config::types::BuddySpecies;
    use crate::product::agent::config::types::TuiBuddy;
    use crate::product::protocol::config_types::IdentityKind;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::product::tui_app::buddy::model::Buddy;
    use crate::product::tui_app::buddy::model::BuddyStats;

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

        let name_x = rendered_name_x(&buf, area, &name);
        let name_width = UnicodeWidthStr::width(name.as_str()) as u16;
        let sprite_width = layout::sprite_column_width(name_width);
        let expected_x = area.width.saturating_sub(sprite_width) + (sprite_width - name_width) / 2;

        assert_eq!(name_x, expected_x);
    }

    #[test]
    fn render_buddy_keeps_name_right_anchor_when_reaction_visible() {
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.ensure_active_buddy();
        let name = state.buddy().expect("generated buddy").name.clone();
        let name_width = UnicodeWidthStr::width(name.as_str()) as u16;
        let sprite_width = layout::sprite_column_width(name_width);

        let area = Rect::new(0, 0, 64, 6);
        let mut idle_buf = Buffer::empty(area);
        render_buddy(area, &mut idle_buf, &state, false);
        let idle_name_x = rendered_name_x(&idle_buf, area, &name);

        state.set_reaction("hello".to_string());
        let mut speaking_buf = Buffer::empty(area);
        render_buddy(area, &mut speaking_buf, &state, false);
        let speaking_name_x = rendered_name_x(&speaking_buf, area, &name);

        assert_eq!(speaking_name_x, idle_name_x);
        assert!(
            render_rows(&speaking_buf, area)
                .iter()
                .any(|row| row.contains("hello"))
        );

        let bubble_left =
            area.width - layout::BUBBLE_WIDTH - sprite_width - layout::SPRITE_PADDING_X;
        assert_eq!(speaking_buf[(bubble_left, 0)].symbol(), "╭");
    }

    #[test]
    fn render_buddy_centers_pixel_dragon_sprite_on_name_axis() {
        let mut state = BuddyState::default();
        state.set_config(TuiBuddy {
            enabled: true,
            muted: false,
            ..TuiBuddy::default()
        });
        state.set_buddy_for_test(pixel_dragon_buddy());

        let area = Rect::new(0, 0, 18, 6);
        let mut buf = Buffer::empty(area);

        render_buddy(area, &mut buf, &state, false);

        let rows = render_rows(&buf, area);
        let hat_center = substring_center_x2(&rows[0], "-+-");
        let face_center = substring_center_x2(&rows[2], "<  ×  ×  >");
        let name_center = substring_center_x2(&rows[5], "Pixel");

        assert_eq!(hat_center, name_center, "rows: {rows:?}");
        assert!(face_center.abs_diff(name_center) <= 1, "rows: {rows:?}");
    }

    pub(crate) fn pixel_dragon_buddy() -> Buddy {
        Buddy {
            name: "Pixel".to_string(),
            species: BuddySpecies::Dragon,
            eye: BuddyEye::Cross,
            hat: BuddyHat::Propeller,
            rarity: BuddyRarity::Epic,
            shiny: false,
            personality: "terminal philosopher".to_string(),
            stats: BuddyStats {
                debugging: 100,
                patience: 45,
                chaos: 50,
                wisdom: 49,
                snark: 33,
            },
            identity_kind: IdentityKind::Nobody,
        }
    }

    fn render_rows(buf: &Buffer, area: Rect) -> Vec<String> {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect()
            })
            .collect()
    }

    fn rendered_name_x(buf: &Buffer, area: Rect, name: &str) -> u16 {
        let name_width = UnicodeWidthStr::width(name) as u16;
        (0..area.height)
            .rev()
            .find_map(|row| {
                (0..=area.width.saturating_sub(name_width)).find(|x| {
                    (0..name_width)
                        .map(|offset| buf[(x + offset, row)].symbol())
                        .collect::<String>()
                        == name
                })
            })
            .expect("buddy name rendered")
    }

    fn substring_center_x2(row: &str, needle: &str) -> usize {
        let start = row.find(needle).expect("needle rendered");
        2 * UnicodeWidthStr::width(&row[..start]) + UnicodeWidthStr::width(needle)
    }
}
