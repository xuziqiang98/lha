#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptSelectionPoint {
    pub(crate) line_index: usize,
    pub(crate) column: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TranscriptSelection {
    pub(crate) anchor: Option<TranscriptSelectionPoint>,
    pub(crate) head: Option<TranscriptSelectionPoint>,
    pub(crate) dragging: bool,
}

impl TranscriptSelection {
    pub(crate) fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.dragging = false;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    pub(crate) fn ordered_endpoints(
        &self,
    ) -> Option<(TranscriptSelectionPoint, TranscriptSelectionPoint)> {
        let anchor = self.anchor?;
        let head = self.head?;
        if (head.line_index, head.column) < (anchor.line_index, anchor.column) {
            Some((head, anchor))
        } else {
            Some((anchor, head))
        }
    }
}
