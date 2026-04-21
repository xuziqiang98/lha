use codex_llm::TranscriptItem;
use codex_llm_types::ContentItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputQueue {
    Primary,
    Steering,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInput {
    items: Vec<TranscriptItem>,
}

impl SessionInput {
    pub fn new(items: Vec<TranscriptItem>) -> Self {
        Self { items }
    }

    pub fn from_item(item: TranscriptItem) -> Self {
        Self { items: vec![item] }
    }

    pub fn from_user_text(text: impl Into<String>) -> Self {
        Self::from_item(TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: text.into() }],
            end_turn: None,
        })
    }

    pub fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    pub fn into_items(self) -> Vec<TranscriptItem> {
        self.items
    }
}

impl From<Vec<TranscriptItem>> for SessionInput {
    fn from(value: Vec<TranscriptItem>) -> Self {
        Self::new(value)
    }
}

impl From<TranscriptItem> for SessionInput {
    fn from(value: TranscriptItem) -> Self {
        Self::from_item(value)
    }
}
