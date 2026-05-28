#[cfg(test)]
use std::collections::VecDeque;

#[cfg(test)]
use ratatui::text::Line;

#[cfg(test)]
use crate::markdown_stream::MarkdownStreamCollector;
pub(crate) mod controller;

#[cfg(test)]
pub(crate) struct StreamState {
    pub(crate) collector: MarkdownStreamCollector,
    queued_lines: VecDeque<Line<'static>>,
}

#[cfg(test)]
impl StreamState {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width),
            queued_lines: VecDeque::new(),
        }
    }
    pub(crate) fn push_delta(&mut self, delta: &str) {
        self.collector.push_delta(delta);
    }
    pub(crate) fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
    }
    pub(crate) fn step(&mut self) -> Vec<Line<'static>> {
        self.queued_lines
            .pop_front()
            .into_iter()
            .collect::<Vec<_>>()
    }
    pub(crate) fn drain_all(&mut self) -> Vec<Line<'static>> {
        self.queued_lines.drain(..).collect::<Vec<_>>()
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }
    pub(crate) fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        self.queued_lines.extend(lines);
    }
}
