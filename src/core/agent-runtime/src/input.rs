use codex_protocol::models::ContentItem;
use codex_protocol::models::ConversationItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputQueue {
    Primary,
    Steering,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInput {
    items: Vec<ConversationItem>,
}

impl SessionInput {
    pub fn new(items: Vec<ConversationItem>) -> Self {
        Self { items }
    }

    pub fn from_item(item: ConversationItem) -> Self {
        Self { items: vec![item] }
    }

    pub fn from_user_text(text: impl Into<String>) -> Self {
        Self::from_item(ConversationItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: text.into() }],
            end_turn: None,
        })
    }

    pub fn items(&self) -> &[ConversationItem] {
        &self.items
    }

    pub fn into_items(self) -> Vec<ConversationItem> {
        self.items
    }
}

impl From<Vec<ConversationItem>> for SessionInput {
    fn from(value: Vec<ConversationItem>) -> Self {
        Self::new(value)
    }
}

impl From<ConversationItem> for SessionInput {
    fn from(value: ConversationItem) -> Self {
        Self::from_item(value)
    }
}
