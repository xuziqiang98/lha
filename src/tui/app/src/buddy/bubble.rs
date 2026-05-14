use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::block::Block;
use ratatui::widgets::block::BorderType;

use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

pub(crate) fn render_bubble(
    area: Rect,
    buf: &mut Buffer,
    text: &str,
    fading: bool,
    border_color: Color,
) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let wrapped = word_wrap_lines(
        [Line::from(text.to_string())],
        RtOptions::new(area.width.saturating_sub(2) as usize).break_words(false),
    );
    let wrapped = wrapped
        .into_iter()
        .take(area.height.saturating_sub(1) as usize)
        .map(|line| {
            if fading {
                line.italic().dim()
            } else {
                line.italic()
            }
        })
        .collect::<Vec<_>>();
    let border_style = if fading {
        Style::default().fg(border_color).dim()
    } else {
        Style::default().fg(border_color)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);
    Paragraph::new(wrapped).block(block).render(area, buf);
    let tail_x = area.x.saturating_add(area.width.saturating_sub(1));
    let tail_y = area.y.saturating_add(area.height / 2);
    let tail = if fading {
        "─".fg(border_color).dim()
    } else {
        "─".fg(border_color)
    };
    buf.set_line(tail_x, tail_y, &Line::from(vec![tail]), 1);
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::style::Modifier;

    use super::*;

    #[test]
    fn render_bubble_uses_italic_text_before_fade() {
        let area = Rect::new(0, 0, 16, 4);
        let mut buf = Buffer::empty(area);

        render_bubble(area, &mut buf, "hello", false, Color::Magenta);

        let text_style = buf[(1, 1)].style();
        assert!(text_style.add_modifier.contains(Modifier::ITALIC));
        assert!(!text_style.add_modifier.contains(Modifier::DIM));

        let border_style = buf[(0, 0)].style();
        assert_eq!(border_style.fg, Some(Color::Magenta));

        let tail_style = buf[(15, 2)].style();
        assert_eq!(tail_style.fg, Some(Color::Magenta));
    }

    #[test]
    fn render_bubble_keeps_italic_text_when_fading() {
        let area = Rect::new(0, 0, 16, 4);
        let mut buf = Buffer::empty(area);

        render_bubble(area, &mut buf, "hello", true, Color::Yellow);

        let text_style = buf[(1, 1)].style();
        assert!(text_style.add_modifier.contains(Modifier::ITALIC));
        assert!(text_style.add_modifier.contains(Modifier::DIM));

        let border_style = buf[(0, 0)].style();
        assert_eq!(border_style.fg, Some(Color::Yellow));
        assert!(border_style.add_modifier.contains(Modifier::DIM));

        let tail_style = buf[(15, 2)].style();
        assert_eq!(tail_style.fg, Some(Color::Yellow));
        assert!(tail_style.add_modifier.contains(Modifier::DIM));
    }
}
