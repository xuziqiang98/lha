use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::style::proposed_plan_style;
use ratatui::prelude::Stylize;
use ratatui::text::Line;

use super::StreamState;

/// Controller that manages newline-gated streaming, header emission, and
/// commit animation across streams.
#[cfg(test)]
pub(crate) struct StreamController {
    state: StreamState,
    finishing_after_drain: bool,
    header_emitted: bool,
}

#[cfg(test)]
impl StreamController {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self {
            state: StreamState::new(width),
            finishing_after_drain: false,
            header_emitted: false,
        }
    }

    /// Push a delta; if it contains a newline, commit completed lines and start animation.
    pub(crate) fn push(&mut self, delta: &str) -> bool {
        let state = &mut self.state;
        state.push_delta(delta);
        if delta.contains('\n') {
            let newly_completed = state.collector.commit_complete_lines();
            if !newly_completed.is_empty() {
                state.enqueue(newly_completed);
                return true;
            }
        }
        false
    }

    /// Finalize the active stream. Drain and emit now.
    pub(crate) fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        // Finalize collector first.
        let remaining = {
            let state = &mut self.state;
            state.collector.finalize_and_drain()
        };
        // Collect all output first to avoid emitting headers when there is no content.
        let mut out_lines = Vec::new();
        {
            let state = &mut self.state;
            if !remaining.is_empty() {
                state.enqueue(remaining);
            }
            let step = state.drain_all();
            out_lines.extend(step);
        }

        // Cleanup
        self.state.clear();
        self.finishing_after_drain = false;
        self.emit(out_lines)
    }

    /// Step animation: commit at most one queued line and handle end-of-drain cleanup.
    pub(crate) fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.state.step();
        (self.emit(step), self.state.is_idle())
    }

    fn emit(&mut self, lines: Vec<Line<'static>>) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }
        Some(Box::new(history_cell::AgentMessageCell::new(lines, {
            let header_emitted = self.header_emitted;
            self.header_emitted = true;
            !header_emitted
        })))
    }
}

/// Controller for assistant markdown streams.
///
/// The committed history cell keeps the raw markdown so it can reflow when the
/// terminal width changes. While streaming, this controller only tracks how
/// many rendered lines should be visible to preserve the existing reveal
/// animation.
pub(crate) struct AgentMarkdownStreamController {
    buffer: String,
    completed_source_len: usize,
    visible_rendered_lines: usize,
    queued_reveal_lines: usize,
}

impl AgentMarkdownStreamController {
    pub(crate) fn new() -> Self {
        Self {
            buffer: String::new(),
            completed_source_len: 0,
            visible_rendered_lines: 0,
            queued_reveal_lines: 0,
        }
    }

    /// Push a delta and queue newly completed rendered lines when a newline
    /// closes one or more logical markdown lines.
    pub(crate) fn push(&mut self, delta: &str, width: Option<usize>) -> bool {
        self.buffer.push_str(delta);
        if !delta.contains('\n') {
            return false;
        }

        let Some(last_newline_idx) = self.buffer.rfind('\n') else {
            return false;
        };
        let completed_source_len = last_newline_idx + 1;
        if completed_source_len <= self.completed_source_len {
            return false;
        }

        self.completed_source_len = completed_source_len;
        let completed_source = &self.buffer[..self.completed_source_len];
        let completed_rendered_lines = rendered_line_count(completed_source, width);
        let pending_target = self
            .visible_rendered_lines
            .saturating_add(self.queued_reveal_lines);
        if completed_rendered_lines > pending_target {
            self.queued_reveal_lines = self
                .queued_reveal_lines
                .saturating_add(completed_rendered_lines - pending_target);
        }

        self.queued_reveal_lines > 0
    }

    pub(crate) fn on_commit_tick(&mut self) -> (bool, bool) {
        if self.queued_reveal_lines == 0 {
            return (false, true);
        }
        self.visible_rendered_lines = self.visible_rendered_lines.saturating_add(1);
        self.queued_reveal_lines -= 1;
        (true, self.queued_reveal_lines == 0)
    }

    pub(crate) fn completed_source(&self) -> &str {
        &self.buffer[..self.completed_source_len]
    }

    pub(crate) fn visible_rendered_lines(&self) -> usize {
        self.visible_rendered_lines
    }

    pub(crate) fn finalize(self) -> String {
        self.buffer
    }
}

fn rendered_line_count(source: &str, width: Option<usize>) -> usize {
    let mut rendered: Vec<Line<'static>> = Vec::new();
    crate::markdown::append_markdown(source, width, &mut rendered);
    if rendered
        .last()
        .is_some_and(crate::render::line_utils::is_blank_line_spaces_only)
    {
        rendered.len().saturating_sub(1)
    } else {
        rendered.len()
    }
}

/// Controller that streams proposed plan markdown into a styled plan block.
pub(crate) struct PlanStreamController {
    state: StreamState,
    header_emitted: bool,
    emitted_any: bool,
}

impl PlanStreamController {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self {
            state: StreamState::new(width),
            header_emitted: false,
            emitted_any: false,
        }
    }

    /// Push a delta; if it contains a newline, commit completed lines and start animation.
    pub(crate) fn push(&mut self, delta: &str) -> bool {
        let state = &mut self.state;
        state.push_delta(delta);
        if delta.contains('\n') {
            let newly_completed = state.collector.commit_complete_lines();
            if !newly_completed.is_empty() {
                state.enqueue(newly_completed);
                return true;
            }
        }
        false
    }

    /// Finalize the active stream. Drain and emit now.
    pub(crate) fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        let remaining = {
            let state = &mut self.state;
            state.collector.finalize_and_drain()
        };
        let mut out_lines = Vec::new();
        {
            let state = &mut self.state;
            if !remaining.is_empty() {
                state.enqueue(remaining);
            }
            let step = state.drain_all();
            out_lines.extend(step);
        }

        self.state.clear();
        if out_lines.is_empty() && self.emitted_any {
            return Some(Box::new(history_cell::new_proposed_plan_stream(
                vec![history_cell::proposed_plan_trailing_body_gap_line()],
                true,
            )));
        }
        self.emit(out_lines, true)
    }

    /// Step animation: commit at most one queued line and handle end-of-drain cleanup.
    pub(crate) fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.state.step();
        (self.emit(step, false), self.state.is_idle())
    }

    fn emit(
        &mut self,
        lines: Vec<Line<'static>>,
        include_trailing_gap: bool,
    ) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }

        let cell = self.build_cell(lines, include_trailing_gap);
        self.header_emitted = true;
        self.emitted_any = true;
        Some(Box::new(cell))
    }

    fn build_cell(
        &self,
        lines: Vec<Line<'static>>,
        include_trailing_gap: bool,
    ) -> history_cell::ProposedPlanStreamCell {
        let mut out_lines: Vec<Line<'static>> = Vec::new();
        let is_stream_continuation = self.header_emitted;
        if !self.header_emitted {
            out_lines.push(vec!["• ".dim(), "Proposed Plan".bold()].into());
            out_lines.extend(history_cell::proposed_plan_header_gap_lines());
        }

        let mut plan_lines: Vec<Line<'static>> = Vec::new();
        plan_lines.extend(lines);

        let plan_lines = history_cell::prefix_proposed_plan_body_lines(plan_lines)
            .into_iter()
            .map(|line| line.style(proposed_plan_style()))
            .collect::<Vec<_>>();
        out_lines.extend(plan_lines);
        if include_trailing_gap {
            out_lines.push(history_cell::proposed_plan_trailing_body_gap_line());
        }

        history_cell::new_proposed_plan_stream(out_lines, is_stream_continuation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_to_plain_strings(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[tokio::test]
    async fn assistant_commits_only_complete_lines_until_finalize() {
        let mut ctrl = StreamController::new(None);

        assert!(!ctrl.push("partial"));
        let (cell, idle) = ctrl.on_commit_tick();
        assert!(cell.is_none());
        assert!(idle);

        let cell = ctrl
            .finalize()
            .expect("expected final partial assistant line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["• partial".to_string()]
        );

        let mut ctrl = StreamController::new(None);
        assert!(ctrl.push("one\ntwo\npartial"));

        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected completed assistant line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["• one".to_string()]
        );
        assert!(!idle);

        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected second completed assistant line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["  two".to_string()]
        );
        assert!(idle);

        let cell = ctrl
            .finalize()
            .expect("expected final partial assistant line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["  partial".to_string()]
        );
    }

    #[tokio::test]
    async fn assistant_markdown_stream_reveals_only_after_newline() {
        let mut ctrl = AgentMarkdownStreamController::new();

        assert!(!ctrl.push("partial", Some(20)));
        assert_eq!(ctrl.completed_source(), "");
        assert_eq!(ctrl.visible_rendered_lines(), 0);
        assert_eq!(ctrl.on_commit_tick(), (false, true));

        assert!(ctrl.push("\n", Some(20)));
        assert_eq!(ctrl.completed_source(), "partial\n");
        assert_eq!(ctrl.on_commit_tick(), (true, true));
        assert_eq!(ctrl.visible_rendered_lines(), 1);
    }

    #[tokio::test]
    async fn assistant_markdown_stream_reveals_one_rendered_line_per_tick() {
        let mut ctrl = AgentMarkdownStreamController::new();

        assert!(ctrl.push(
            "This is a simple sentence that wraps across several rendered lines.\n",
            Some(16),
        ));
        assert_eq!(ctrl.visible_rendered_lines(), 0);

        assert_eq!(ctrl.on_commit_tick(), (true, false));
        assert_eq!(ctrl.visible_rendered_lines(), 1);
        assert_eq!(ctrl.on_commit_tick(), (true, false));
        assert_eq!(ctrl.visible_rendered_lines(), 2);

        while !ctrl.on_commit_tick().1 {}
        assert_eq!(
            ctrl.visible_rendered_lines(),
            rendered_line_count(ctrl.completed_source(), Some(16))
        );
    }

    #[tokio::test]
    async fn assistant_markdown_stream_finalize_keeps_partial_tail() {
        let mut ctrl = AgentMarkdownStreamController::new();

        assert!(ctrl.push("complete\n", Some(80)));
        assert_eq!(ctrl.on_commit_tick(), (true, true));
        assert!(!ctrl.push("partial tail", Some(80)));

        assert_eq!(ctrl.finalize(), "complete\npartial tail");
    }

    #[tokio::test]
    async fn plan_commits_only_complete_lines_until_finalize() {
        let mut ctrl = PlanStreamController::new(None);

        assert!(!ctrl.push("partial"));
        let (cell, idle) = ctrl.on_commit_tick();
        assert!(cell.is_none());
        assert!(idle);

        let cell = ctrl.finalize().expect("expected final partial plan line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec![
                "• Proposed Plan".to_string(),
                "".to_string(),
                "".to_string(),
                "  partial".to_string(),
                "".to_string(),
            ]
        );

        let mut ctrl = PlanStreamController::new(None);
        assert!(ctrl.push("- step\npartial"));

        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected completed plan line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec![
                "• Proposed Plan".to_string(),
                "".to_string(),
                "".to_string(),
                "  - step".to_string(),
            ]
        );
        assert!(idle);

        let cell = ctrl.finalize().expect("expected final partial plan line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["    partial".to_string(), "".to_string()]
        );
    }

    #[tokio::test]
    async fn plan_finalize_after_drained_stream_returns_trailing_gap() {
        let mut ctrl = PlanStreamController::new(None);
        assert!(ctrl.push("- step\n"));
        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected completed plan line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec![
                "• Proposed Plan".to_string(),
                "".to_string(),
                "".to_string(),
                "  - step".to_string(),
            ]
        );
        assert!(idle);

        let cell = ctrl
            .finalize()
            .expect("expected trailing gap after drained stream");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["".to_string()]
        );
        assert!(cell.is_stream_continuation());
    }

    #[tokio::test]
    async fn plan_heading_separator_blank_line_is_styled_when_streamed() {
        let mut ctrl = PlanStreamController::new(None);
        assert!(ctrl.push("# Title\n"));
        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected heading plan line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec![
                "• Proposed Plan".to_string(),
                "".to_string(),
                "".to_string(),
                "  # Title".to_string(),
            ]
        );
        assert!(idle);

        assert!(ctrl.push("\nbody\n"));
        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected blank separator plan line");
        let lines = cell.display_lines(u16::MAX);
        assert_eq!(lines_to_plain_strings(&lines), vec!["".to_string()]);
        assert_eq!(lines[0].style.bg, proposed_plan_style().bg);
        assert!(cell.is_stream_continuation());
        assert!(!idle);

        let (cell, idle) = ctrl.on_commit_tick();
        let cell = cell.expect("expected body plan line");
        assert_eq!(
            lines_to_plain_strings(&cell.display_lines(u16::MAX)),
            vec!["  body".to_string()]
        );
        assert!(idle);
    }

    #[tokio::test]
    async fn plan_finalize_without_streamed_content_returns_none() {
        let mut ctrl = PlanStreamController::new(None);
        assert!(ctrl.finalize().is_none());
    }

    #[tokio::test]
    async fn controller_loose_vs_tight_with_commit_ticks_matches_full() {
        let mut ctrl = StreamController::new(None);
        let mut lines = Vec::new();

        // Exact deltas from the session log (section: Loose vs. tight list items)
        let deltas = vec![
            "\n\n",
            "Loose",
            " vs",
            ".",
            " tight",
            " list",
            " items",
            ":\n",
            "1",
            ".",
            " Tight",
            " item",
            "\n",
            "2",
            ".",
            " Another",
            " tight",
            " item",
            "\n\n",
            "1",
            ".",
            " Loose",
            " item",
            " with",
            " its",
            " own",
            " paragraph",
            ".\n\n",
            "  ",
            " This",
            " paragraph",
            " belongs",
            " to",
            " the",
            " same",
            " list",
            " item",
            ".\n\n",
            "2",
            ".",
            " Second",
            " loose",
            " item",
            " with",
            " a",
            " nested",
            " list",
            " after",
            " a",
            " blank",
            " line",
            ".\n\n",
            "  ",
            " -",
            " Nested",
            " bullet",
            " under",
            " a",
            " loose",
            " item",
            "\n",
            "  ",
            " -",
            " Another",
            " nested",
            " bullet",
            "\n\n",
        ];

        // Simulate streaming with a commit tick attempt after each delta.
        for d in deltas.iter() {
            ctrl.push(d);
            while let (Some(cell), idle) = ctrl.on_commit_tick() {
                lines.extend(cell.transcript_lines(u16::MAX));
                if idle {
                    break;
                }
            }
        }
        // Finalize and flush remaining lines now.
        if let Some(cell) = ctrl.finalize() {
            lines.extend(cell.transcript_lines(u16::MAX));
        }

        let streamed: Vec<_> = lines_to_plain_strings(&lines)
            .into_iter()
            // skip • and 2-space indentation
            .map(|s| s.chars().skip(2).collect::<String>())
            .collect();

        // Full render of the same source
        let source: String = deltas.iter().copied().collect();
        let mut rendered: Vec<ratatui::text::Line<'static>> = Vec::new();
        crate::markdown::append_markdown(&source, None, &mut rendered);
        let rendered_strs = lines_to_plain_strings(&rendered);

        assert_eq!(streamed, rendered_strs);

        // Also assert exact expected plain strings for clarity.
        let expected = vec![
            "Loose vs. tight list items:".to_string(),
            "".to_string(),
            "1. Tight item".to_string(),
            "2. Another tight item".to_string(),
            "3. Loose item with its own paragraph.".to_string(),
            "".to_string(),
            "   This paragraph belongs to the same list item.".to_string(),
            "4. Second loose item with a nested list after a blank line.".to_string(),
            "    - Nested bullet under a loose item".to_string(),
            "    - Another nested bullet".to_string(),
        ];
        assert_eq!(
            streamed, expected,
            "expected exact rendered lines for loose/tight section"
        );
    }
}
