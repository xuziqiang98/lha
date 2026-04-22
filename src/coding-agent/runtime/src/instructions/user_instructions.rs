use serde::Deserialize;
use serde::Serialize;

use codex_protocol::legacy_transcript::ConversationItem;
use codex_protocol::models::ContentItem;

pub const USER_INSTRUCTIONS_OPEN_TAG_LEGACY: &str = "<user_instructions>";
pub const USER_INSTRUCTIONS_PREFIX: &str = "# AGENTS.md instructions for ";
const SKILL_OPEN_TAG: &str = "<skill>\n";
const BACKFILLED_SKILL_OPEN_TAG: &str = "<skill source=\"compact_backfill\">\n";
const SKILL_CLOSE_TAG: &str = "\n</skill>";
const SKILL_NAME_OPEN_TAG: &str = "<name>";
const SKILL_NAME_CLOSE_TAG: &str = "</name>\n";
const SKILL_PATH_OPEN_TAG: &str = "<path>";
const SKILL_PATH_CLOSE_TAG: &str = "</path>\n";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "user_instructions", rename_all = "snake_case")]
pub(crate) struct UserInstructions {
    pub directory: String,
    pub text: String,
}

impl UserInstructions {
    pub fn is_user_instructions(message: &[ContentItem]) -> bool {
        if let [ContentItem::InputText { text }] = message {
            text.starts_with(USER_INSTRUCTIONS_PREFIX)
                || text.starts_with(USER_INSTRUCTIONS_OPEN_TAG_LEGACY)
        } else {
            false
        }
    }
}

impl From<UserInstructions> for ConversationItem {
    fn from(ui: UserInstructions) -> Self {
        ConversationItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!(
                    "{USER_INSTRUCTIONS_PREFIX}{directory}\n\n<INSTRUCTIONS>\n{contents}\n</INSTRUCTIONS>",
                    directory = ui.directory,
                    contents = ui.text
                ),
            }],
            end_turn: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "skill_instructions", rename_all = "snake_case")]
pub(crate) struct SkillInstructions {
    pub name: String,
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillInstructionSource {
    Direct,
    CompactBackfill,
}

impl SkillInstructions {
    pub fn is_skill_instructions(message: &[ContentItem]) -> bool {
        Self::from_message_with_source(message).is_some()
    }

    #[allow(dead_code)]
    pub fn from_message(message: &[ContentItem]) -> Option<Self> {
        Self::from_message_with_source(message).map(|(skill, _)| skill)
    }

    pub fn from_message_with_source(
        message: &[ContentItem],
    ) -> Option<(Self, SkillInstructionSource)> {
        if let [ContentItem::InputText { text }] = message {
            Self::parse_with_source(text)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_with_source(text).map(|(skill, _)| skill)
    }

    pub fn parse_with_source(text: &str) -> Option<(Self, SkillInstructionSource)> {
        let (source, body) = parse_skill_open_tag(text)?;
        let body = body.strip_suffix(SKILL_CLOSE_TAG)?;
        let (name, body) = parse_skill_field(body, SKILL_NAME_OPEN_TAG, SKILL_NAME_CLOSE_TAG)?;
        let (path, contents) = parse_skill_field(body, SKILL_PATH_OPEN_TAG, SKILL_PATH_CLOSE_TAG)?;

        Some((
            Self {
                name,
                path,
                contents: contents.to_string(),
            },
            source,
        ))
    }

    pub fn into_backfilled_response_item(self) -> ConversationItem {
        self.into_response_item_with_source(SkillInstructionSource::CompactBackfill)
    }

    fn into_response_item_with_source(self, source: SkillInstructionSource) -> ConversationItem {
        let open_tag = match source {
            SkillInstructionSource::Direct => SKILL_OPEN_TAG,
            SkillInstructionSource::CompactBackfill => BACKFILLED_SKILL_OPEN_TAG,
        };

        ConversationItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!(
                    "{open_tag}<name>{name}</name>\n<path>{path}</path>\n{contents}{SKILL_CLOSE_TAG}",
                    name = self.name,
                    path = self.path,
                    contents = self.contents
                ),
            }],
            end_turn: None,
        }
    }
}

impl From<SkillInstructions> for ConversationItem {
    fn from(si: SkillInstructions) -> Self {
        si.into_response_item_with_source(SkillInstructionSource::Direct)
    }
}

fn parse_skill_open_tag(text: &str) -> Option<(SkillInstructionSource, &str)> {
    if let Some(body) = text.strip_prefix(SKILL_OPEN_TAG) {
        Some((SkillInstructionSource::Direct, body))
    } else {
        text.strip_prefix(BACKFILLED_SKILL_OPEN_TAG)
            .map(|body| (SkillInstructionSource::CompactBackfill, body))
    }
}

fn parse_skill_field<'a>(
    text: &'a str,
    open_tag: &str,
    close_tag: &str,
) -> Option<(String, &'a str)> {
    let text = text.strip_prefix(open_tag)?;
    let end = text.find(close_tag)?;
    let value = text[..end].to_string();
    let rest = &text[end + close_tag.len()..];
    Some((value, rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_user_instructions() {
        let user_instructions = UserInstructions {
            directory: "test_directory".to_string(),
            text: "test_text".to_string(),
        };
        let response_item: ConversationItem = user_instructions.into();

        let ConversationItem::Message { role, content, .. } = response_item else {
            panic!("expected ConversationItem::Message");
        };

        assert_eq!(role, "user");

        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected one InputText content item");
        };

        assert_eq!(
            text,
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        );
    }

    #[test]
    fn test_is_user_instructions() {
        assert!(UserInstructions::is_user_instructions(
            &[ContentItem::InputText {
                text: "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>".to_string(),
            }]
        ));
        assert!(UserInstructions::is_user_instructions(&[
            ContentItem::InputText {
                text: "<user_instructions>test_text</user_instructions>".to_string(),
            }
        ]));
        assert!(!UserInstructions::is_user_instructions(&[
            ContentItem::InputText {
                text: "test_text".to_string(),
            }
        ]));
    }

    #[test]
    fn test_skill_instructions() {
        let skill_instructions = SkillInstructions {
            name: "demo-skill".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "body".to_string(),
        };
        let response_item: ConversationItem = skill_instructions.into();

        let ConversationItem::Message { role, content, .. } = response_item else {
            panic!("expected ConversationItem::Message");
        };

        assert_eq!(role, "user");

        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected one InputText content item");
        };

        assert_eq!(
            text,
            "<skill>\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        );
    }

    #[test]
    fn test_is_skill_instructions() {
        assert!(SkillInstructions::is_skill_instructions(&[
            ContentItem::InputText {
                text: "<skill>\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>"
                    .to_string(),
            }
        ]));
        assert!(SkillInstructions::is_skill_instructions(&[
            ContentItem::InputText {
                text: "<skill source=\"compact_backfill\">\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>"
                    .to_string(),
            }
        ]));
        assert!(!SkillInstructions::is_skill_instructions(&[
            ContentItem::InputText {
                text: "regular text".to_string(),
            }
        ]));
    }

    #[test]
    fn test_parse_skill_instructions() {
        let parsed = SkillInstructions::parse_with_source(
            "<skill>\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        );

        assert_eq!(
            parsed,
            Some((
                SkillInstructions {
                    name: "demo-skill".to_string(),
                    path: "skills/demo/SKILL.md".to_string(),
                    contents: "body".to_string(),
                },
                SkillInstructionSource::Direct,
            ))
        );
    }

    #[test]
    fn test_parse_skill_instructions_rejects_invalid_messages() {
        assert_eq!(
            SkillInstructions::parse("<skill>\n<name>demo-skill</name>\nbody\n</skill>"),
            None
        );
        assert_eq!(SkillInstructions::parse("regular text"), None);
        assert_eq!(
            SkillInstructions::parse(
                "<skill source=\"unknown\">\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
            ),
            None
        );
    }

    #[test]
    fn test_skill_instructions_round_trip() {
        let expected = SkillInstructions {
            name: "demo-skill".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "body\nwith more".to_string(),
        };
        let response_item: ConversationItem = expected.clone().into();
        let ConversationItem::Message { content, .. } = response_item else {
            panic!("expected ConversationItem::Message");
        };

        let parsed = SkillInstructions::from_message(&content);

        assert_eq!(parsed, Some(expected));
    }

    #[test]
    fn test_parse_backfilled_skill_instructions() {
        let parsed = SkillInstructions::parse_with_source(
            "<skill source=\"compact_backfill\">\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        );

        assert_eq!(
            parsed,
            Some((
                SkillInstructions {
                    name: "demo-skill".to_string(),
                    path: "skills/demo/SKILL.md".to_string(),
                    contents: "body".to_string(),
                },
                SkillInstructionSource::CompactBackfill,
            ))
        );
    }

    #[test]
    fn test_backfilled_skill_instructions_round_trip() {
        let expected = SkillInstructions {
            name: "demo-skill".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "body\nwith more".to_string(),
        };
        let response_item = expected.clone().into_backfilled_response_item();
        let ConversationItem::Message { content, .. } = response_item else {
            panic!("expected ConversationItem::Message");
        };

        let parsed = SkillInstructions::from_message_with_source(&content);

        assert_eq!(
            parsed,
            Some((expected, SkillInstructionSource::CompactBackfill))
        );
    }
}
