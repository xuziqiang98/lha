use std::collections::VecDeque;

use ratatui::text::Line;

use crate::markdown_stream::MarkdownStreamCollector;
pub(crate) mod controller;

pub(crate) struct StreamState {
    pub(crate) collector: MarkdownStreamCollector,
    queued_lines: VecDeque<Line<'static>>,
    pub(crate) has_seen_delta: bool,
    revision: u64,
}

impl StreamState {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width),
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
            revision: 0,
        }
    }
    pub(crate) fn push_delta(&mut self, delta: &str) {
        if !delta.is_empty() {
            self.has_seen_delta = true;
            self.bump_revision();
        }
        self.collector.push_delta(delta);
    }
    pub(crate) fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
        self.has_seen_delta = false;
        self.bump_revision();
    }
    pub(crate) fn step(&mut self) -> Vec<Line<'static>> {
        let step = self
            .queued_lines
            .pop_front()
            .into_iter()
            .collect::<Vec<_>>();
        if !step.is_empty() {
            self.bump_revision();
        }
        step
    }
    pub(crate) fn drain_all(&mut self) -> Vec<Line<'static>> {
        let drained = self.queued_lines.drain(..).collect::<Vec<_>>();
        if !drained.is_empty() {
            self.bump_revision();
        }
        drained
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }
    pub(crate) fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        self.queued_lines.extend(lines);
        self.bump_revision();
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn live_tail_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self.queued_lines.iter().cloned().collect::<Vec<_>>();
        lines.extend(self.collector.preview_uncommitted_lines());
        lines
    }
    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}
