/// Coalesces provider deltas until the display clock commits the latest source.
#[derive(Debug, Default)]
pub(crate) struct StreamDisplayBuffer {
    received: String,
    committed_len: usize,
    pending: bool,
}

impl StreamDisplayBuffer {
    fn push(&mut self, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let should_start_clock = !self.pending;
        self.received.push_str(delta);
        self.pending = true;
        should_start_clock
    }

    fn commit_pending(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        self.committed_len = self.received.len();
        self.pending = false;
        true
    }

    fn committed_source(&self) -> &str {
        &self.received[..self.committed_len]
    }

    fn is_idle(&self) -> bool {
        !self.pending
    }

    fn has_pending(&self) -> bool {
        self.pending
    }

    fn finalize(self) -> String {
        self.received
    }
}

/// Controller for assistant markdown streams.
pub(crate) struct AgentMarkdownStreamController {
    display: StreamDisplayBuffer,
}

impl AgentMarkdownStreamController {
    pub(crate) fn new() -> Self {
        Self {
            display: StreamDisplayBuffer::default(),
        }
    }

    pub(crate) fn push(&mut self, delta: &str) -> bool {
        self.display.push(delta)
    }

    pub(crate) fn on_commit_tick(&mut self) -> (bool, bool) {
        let advanced = self.display.commit_pending();
        (advanced, self.display.is_idle())
    }

    pub(crate) fn committed_source(&self) -> &str {
        self.display.committed_source()
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.display.has_pending()
    }

    pub(crate) fn finalize(self) -> String {
        self.display.finalize()
    }
}

/// Controller that streams proposed plan markdown into a styled plan block.
pub(crate) struct PlanStreamController {
    display: StreamDisplayBuffer,
}

impl PlanStreamController {
    pub(crate) fn new() -> Self {
        Self {
            display: StreamDisplayBuffer::default(),
        }
    }

    pub(crate) fn push(&mut self, delta: &str) -> bool {
        self.display.push(delta)
    }

    pub(crate) fn on_commit_tick(&mut self) -> (bool, bool) {
        let advanced = self.display.commit_pending();
        (advanced, self.display.is_idle())
    }

    pub(crate) fn committed_source(&self) -> &str {
        self.display.committed_source()
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.display.has_pending()
    }

    pub(crate) fn finalize(self) -> String {
        self.display.finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_display_buffer_coalesces_all_pending_deltas_per_tick() {
        let mut display = StreamDisplayBuffer::default();

        assert!(display.push("先说明："));
        assert!(!display.push("`crates.io`"));
        assert!(!display.push(" 发布条件。"));
        assert_eq!(display.committed_source(), "");

        assert!(display.commit_pending());
        assert_eq!(display.committed_source(), "先说明：`crates.io` 发布条件。");
        assert!(display.is_idle());
        assert!(!display.commit_pending());
    }

    #[test]
    fn assistant_markdown_stream_commits_partial_text_on_display_tick() {
        let mut ctrl = AgentMarkdownStreamController::new();

        assert!(ctrl.push("partial"));
        assert_eq!(ctrl.committed_source(), "");
        assert_eq!(ctrl.on_commit_tick(), (true, true));
        assert_eq!(ctrl.committed_source(), "partial");
        assert_eq!(ctrl.on_commit_tick(), (false, true));
    }

    #[test]
    fn assistant_markdown_stream_finalize_keeps_uncommitted_tail() {
        let mut ctrl = AgentMarkdownStreamController::new();

        assert!(ctrl.push("complete\n"));
        assert_eq!(ctrl.on_commit_tick(), (true, true));
        assert!(ctrl.push("partial tail"));

        assert_eq!(ctrl.finalize(), "complete\npartial tail");
    }

    #[test]
    fn plan_stream_uses_the_same_display_clock_contract() {
        let mut ctrl = PlanStreamController::new();

        assert!(ctrl.push("# Title"));
        assert!(!ctrl.push("\n\n- step"));
        assert_eq!(ctrl.committed_source(), "");
        assert_eq!(ctrl.on_commit_tick(), (true, true));
        assert_eq!(ctrl.committed_source(), "# Title\n\n- step");
        assert_eq!(ctrl.on_commit_tick(), (false, true));
        assert_eq!(ctrl.finalize(), "# Title\n\n- step");
    }

    #[test]
    fn empty_stream_finalize_is_lossless() {
        assert_eq!(AgentMarkdownStreamController::new().finalize(), "");
        assert_eq!(PlanStreamController::new().finalize(), "");
    }
}
