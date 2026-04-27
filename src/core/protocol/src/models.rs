use std::path::Path;

use adam_utils_image::load_and_resize_to_fit;
use mcp_types::CallToolResult;
use mcp_types::ContentBlock;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use crate::config_types::CollaborationMode;
use crate::config_types::SandboxMode;
use crate::protocol::AskForApproval;
use crate::protocol::COLLABORATION_MODE_CLOSE_TAG;
use crate::protocol::COLLABORATION_MODE_OPEN_TAG;
use crate::protocol::NetworkAccess;
use crate::protocol::SandboxPolicy;
use crate::protocol::WritableRoot;
use crate::user_input::UserInput;
use adam_execpolicy::Policy;
pub use adam_llm_types::BASE_INSTRUCTIONS_DEFAULT;
pub use adam_llm_types::BaseInstructions;
pub use adam_llm_types::ContentItem;
pub use adam_llm_types::ReasoningItemContent;
pub use adam_llm_types::ReasoningItemReasoningSummary;
pub use adam_llm_types::ToolResultContentItem;
pub use adam_llm_types::ToolResultPayload;
pub use adam_llm_types::TranscriptItem;
use adam_utils_image::error::ImageProcessingError;
use schemars::JsonSchema;

/// Controls whether a command should use the session sandbox or bypass it.
#[derive(
    Debug, Clone, Copy, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    /// Run with the configured sandbox
    #[default]
    UseDefault,
    /// Request to run outside the sandbox
    RequireEscalated,
}

impl SandboxPermissions {
    pub fn requires_escalated_permissions(self) -> bool {
        matches!(self, SandboxPermissions::RequireEscalated)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        queries: Option<Vec<String>>,
    },
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
    },
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pattern: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// Developer-provided guidance that is injected into a turn as a developer role
/// message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename = "developer_instructions", rename_all = "snake_case")]
pub struct DeveloperInstructions {
    text: String,
}

const APPROVAL_POLICY_NEVER: &str = include_str!("prompts/permissions/approval_policy/never.md");
const APPROVAL_POLICY_UNLESS_TRUSTED: &str =
    include_str!("prompts/permissions/approval_policy/unless_trusted.md");
const APPROVAL_POLICY_ON_FAILURE: &str =
    include_str!("prompts/permissions/approval_policy/on_failure.md");
const APPROVAL_POLICY_ON_REQUEST: &str =
    include_str!("prompts/permissions/approval_policy/on_request.md");
const APPROVAL_POLICY_ON_REQUEST_RULE: &str =
    include_str!("prompts/permissions/approval_policy/on_request_rule.md");

const SANDBOX_MODE_DANGER_FULL_ACCESS: &str =
    include_str!("prompts/permissions/sandbox_mode/danger_full_access.md");
const SANDBOX_MODE_WORKSPACE_WRITE: &str =
    include_str!("prompts/permissions/sandbox_mode/workspace_write.md");
const SANDBOX_MODE_READ_ONLY: &str = include_str!("prompts/permissions/sandbox_mode/read_only.md");

impl DeveloperInstructions {
    pub fn new<T: Into<String>>(text: T) -> Self {
        Self { text: text.into() }
    }

    pub fn from(
        approval_policy: AskForApproval,
        exec_policy: &Policy,
        request_rule_enabled: bool,
    ) -> DeveloperInstructions {
        let text = match approval_policy {
            AskForApproval::Never => APPROVAL_POLICY_NEVER.to_string(),
            AskForApproval::UnlessTrusted => APPROVAL_POLICY_UNLESS_TRUSTED.to_string(),
            AskForApproval::OnFailure => APPROVAL_POLICY_ON_FAILURE.to_string(),
            AskForApproval::OnRequest => {
                if !request_rule_enabled {
                    APPROVAL_POLICY_ON_REQUEST.to_string()
                } else {
                    let command_prefixes =
                        format_allow_prefixes(exec_policy.get_allowed_prefixes());
                    match command_prefixes {
                        Some(prefixes) => {
                            format!(
                                "{APPROVAL_POLICY_ON_REQUEST_RULE}\nApproved command prefixes:\n{prefixes}"
                            )
                        }
                        None => APPROVAL_POLICY_ON_REQUEST_RULE.to_string(),
                    }
                }
            }
        };

        DeveloperInstructions::new(text)
    }

    pub fn into_text(self) -> String {
        self.text
    }

    pub fn concat(self, other: impl Into<DeveloperInstructions>) -> Self {
        let mut text = self.text;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&other.into().text);
        Self { text }
    }

    pub fn personality_spec_message(spec: String) -> Self {
        let message = format!(
            "<personality_spec> The user has requested a new communication style. Future messages should adhere to the following personality: \n{spec} </personality_spec>"
        );
        DeveloperInstructions::new(message)
    }

    pub fn from_policy(
        sandbox_policy: &SandboxPolicy,
        approval_policy: AskForApproval,
        exec_policy: &Policy,
        request_rule_enabled: bool,
        cwd: &Path,
    ) -> Self {
        let network_access = if sandbox_policy.has_full_network_access() {
            NetworkAccess::Enabled
        } else {
            NetworkAccess::Restricted
        };

        let (sandbox_mode, writable_roots) = match sandbox_policy {
            SandboxPolicy::DangerFullAccess => (SandboxMode::DangerFullAccess, None),
            SandboxPolicy::ReadOnly => (SandboxMode::ReadOnly, None),
            SandboxPolicy::ExternalSandbox { .. } => (SandboxMode::DangerFullAccess, None),
            SandboxPolicy::WorkspaceWrite { .. } => {
                let roots = sandbox_policy.get_writable_roots_with_cwd(cwd);
                (SandboxMode::WorkspaceWrite, Some(roots))
            }
        };

        DeveloperInstructions::from_permissions_with_network(
            sandbox_mode,
            network_access,
            approval_policy,
            exec_policy,
            request_rule_enabled,
            writable_roots,
        )
    }

    /// Returns developer instructions from a collaboration mode if they exist and are non-empty.
    pub fn from_collaboration_mode(collaboration_mode: &CollaborationMode) -> Option<Self> {
        collaboration_mode
            .settings
            .developer_instructions
            .as_ref()
            .filter(|instructions| !instructions.is_empty())
            .map(|instructions| {
                DeveloperInstructions::new(format!(
                    "{COLLABORATION_MODE_OPEN_TAG}{instructions}{COLLABORATION_MODE_CLOSE_TAG}"
                ))
            })
    }

    fn from_permissions_with_network(
        sandbox_mode: SandboxMode,
        network_access: NetworkAccess,
        approval_policy: AskForApproval,
        exec_policy: &Policy,
        request_rule_enabled: bool,
        writable_roots: Option<Vec<WritableRoot>>,
    ) -> Self {
        let start_tag = DeveloperInstructions::new("<permissions instructions>");
        let end_tag = DeveloperInstructions::new("</permissions instructions>");
        start_tag
            .concat(DeveloperInstructions::sandbox_text(
                sandbox_mode,
                network_access,
            ))
            .concat(DeveloperInstructions::from(
                approval_policy,
                exec_policy,
                request_rule_enabled,
            ))
            .concat(DeveloperInstructions::from_writable_roots(writable_roots))
            .concat(end_tag)
    }

    fn from_writable_roots(writable_roots: Option<Vec<WritableRoot>>) -> Self {
        let Some(roots) = writable_roots else {
            return DeveloperInstructions::new("");
        };

        if roots.is_empty() {
            return DeveloperInstructions::new("");
        }

        let roots_list: Vec<String> = roots
            .iter()
            .map(|r| format!("`{}`", r.root.to_string_lossy()))
            .collect();
        let text = if roots_list.len() == 1 {
            format!(" The writable root is {}.", roots_list[0])
        } else {
            format!(" The writable roots are {}.", roots_list.join(", "))
        };
        DeveloperInstructions::new(text)
    }

    fn sandbox_text(mode: SandboxMode, network_access: NetworkAccess) -> DeveloperInstructions {
        let template = match mode {
            SandboxMode::DangerFullAccess => SANDBOX_MODE_DANGER_FULL_ACCESS.trim_end(),
            SandboxMode::WorkspaceWrite => SANDBOX_MODE_WORKSPACE_WRITE.trim_end(),
            SandboxMode::ReadOnly => SANDBOX_MODE_READ_ONLY.trim_end(),
        };
        let text = template.replace("{network_access}", &network_access.to_string());

        DeveloperInstructions::new(text)
    }
}

const MAX_RENDERED_PREFIXES: usize = 100;
const MAX_ALLOW_PREFIX_TEXT_BYTES: usize = 5000;
const TRUNCATED_MARKER: &str = "...\n[Some commands were truncated]";

pub fn format_allow_prefixes(prefixes: Vec<Vec<String>>) -> Option<String> {
    let mut truncated = false;
    if prefixes.len() > MAX_RENDERED_PREFIXES {
        truncated = true;
    }

    let mut prefixes = prefixes;
    prefixes.sort_by(|a, b| {
        a.len()
            .cmp(&b.len())
            .then_with(|| prefix_combined_str_len(a).cmp(&prefix_combined_str_len(b)))
            .then_with(|| a.cmp(b))
    });

    let full_text = prefixes
        .into_iter()
        .take(MAX_RENDERED_PREFIXES)
        .map(|prefix| format!("- {}", render_command_prefix(&prefix)))
        .collect::<Vec<_>>()
        .join("\n");

    // truncate to last UTF8 char
    let mut output = full_text;
    let byte_idx = output
        .char_indices()
        .nth(MAX_ALLOW_PREFIX_TEXT_BYTES)
        .map(|(i, _)| i);
    if let Some(byte_idx) = byte_idx {
        truncated = true;
        output = output[..byte_idx].to_string();
    }

    if truncated {
        Some(format!("{output}{TRUNCATED_MARKER}"))
    } else {
        Some(output)
    }
}

fn prefix_combined_str_len(prefix: &[String]) -> usize {
    prefix.iter().map(String::len).sum()
}

fn render_command_prefix(prefix: &[String]) -> String {
    let tokens = prefix
        .iter()
        .map(|token| serde_json::to_string(token).unwrap_or_else(|_| format!("{token:?}")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{tokens}]")
}

impl From<DeveloperInstructions> for TranscriptItem {
    fn from(di: DeveloperInstructions) -> Self {
        TranscriptItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: di.into_text(),
            }],
            end_turn: None,
        }
    }
}

impl From<SandboxMode> for DeveloperInstructions {
    fn from(mode: SandboxMode) -> Self {
        let network_access = match mode {
            SandboxMode::DangerFullAccess => NetworkAccess::Enabled,
            SandboxMode::WorkspaceWrite | SandboxMode::ReadOnly => NetworkAccess::Restricted,
        };

        DeveloperInstructions::sandbox_text(mode, network_access)
    }
}

fn local_image_error_placeholder(
    path: &std::path::Path,
    error: impl std::fmt::Display,
) -> ContentItem {
    ContentItem::InputText {
        text: format!(
            "Codex could not read the local image at `{}`: {}",
            path.display(),
            error
        ),
    }
}

pub const VIEW_IMAGE_TOOL_NAME: &str = "view_image";

const IMAGE_OPEN_TAG: &str = "<image>";
const IMAGE_CLOSE_TAG: &str = "</image>";
const LOCAL_IMAGE_OPEN_TAG_PREFIX: &str = "<image name=";
const LOCAL_IMAGE_OPEN_TAG_SUFFIX: &str = ">";
const LOCAL_IMAGE_CLOSE_TAG: &str = IMAGE_CLOSE_TAG;

pub fn image_open_tag_text() -> String {
    IMAGE_OPEN_TAG.to_string()
}

pub fn image_close_tag_text() -> String {
    IMAGE_CLOSE_TAG.to_string()
}

pub fn local_image_label_text(label_number: usize) -> String {
    format!("[Image #{label_number}]")
}

pub fn local_image_open_tag_text(label_number: usize) -> String {
    let label = local_image_label_text(label_number);
    format!("{LOCAL_IMAGE_OPEN_TAG_PREFIX}{label}{LOCAL_IMAGE_OPEN_TAG_SUFFIX}")
}

pub fn is_local_image_open_tag_text(text: &str) -> bool {
    text.strip_prefix(LOCAL_IMAGE_OPEN_TAG_PREFIX)
        .is_some_and(|rest| rest.ends_with(LOCAL_IMAGE_OPEN_TAG_SUFFIX))
}

pub fn is_local_image_close_tag_text(text: &str) -> bool {
    is_image_close_tag_text(text)
}

pub fn is_image_open_tag_text(text: &str) -> bool {
    text == IMAGE_OPEN_TAG
}

pub fn is_image_close_tag_text(text: &str) -> bool {
    text == IMAGE_CLOSE_TAG
}

fn invalid_image_error_placeholder(
    path: &std::path::Path,
    error: impl std::fmt::Display,
) -> ContentItem {
    ContentItem::InputText {
        text: format!(
            "Image located at `{}` is invalid: {}",
            path.display(),
            error
        ),
    }
}

fn unsupported_image_error_placeholder(path: &std::path::Path, mime: &str) -> ContentItem {
    ContentItem::InputText {
        text: format!(
            "Codex cannot attach image at `{}`: unsupported image format `{}`.",
            path.display(),
            mime
        ),
    }
}

pub fn local_image_content_items_with_label_number(
    path: &std::path::Path,
    label_number: Option<usize>,
) -> Vec<ContentItem> {
    match load_and_resize_to_fit(path) {
        Ok(image) => {
            let mut items = Vec::with_capacity(3);
            if let Some(label_number) = label_number {
                items.push(ContentItem::InputText {
                    text: local_image_open_tag_text(label_number),
                });
            }
            items.push(ContentItem::InputImage {
                image_url: image.into_data_url(),
            });
            if label_number.is_some() {
                items.push(ContentItem::InputText {
                    text: LOCAL_IMAGE_CLOSE_TAG.to_string(),
                });
            }
            items
        }
        Err(err) => {
            if matches!(&err, ImageProcessingError::Read { .. }) {
                vec![local_image_error_placeholder(path, &err)]
            } else if err.is_invalid_image() {
                vec![invalid_image_error_placeholder(path, &err)]
            } else {
                let Some(mime_guess) = mime_guess::from_path(path).first() else {
                    return vec![local_image_error_placeholder(
                        path,
                        "unsupported MIME type (unknown)",
                    )];
                };
                let mime = mime_guess.essence_str().to_owned();
                if !mime.starts_with("image/") {
                    return vec![local_image_error_placeholder(
                        path,
                        format!("unsupported MIME type `{mime}`"),
                    )];
                }
                vec![unsupported_image_error_placeholder(path, &mime)]
            }
        }
    }
}

pub fn transcript_item_from_user_input(items: Vec<UserInput>) -> TranscriptItem {
    let mut image_index = 0;
    TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: items
            .into_iter()
            .flat_map(|c| match c {
                UserInput::Text { text, .. } => vec![ContentItem::InputText { text }],
                UserInput::Image { image_url } => vec![
                    ContentItem::InputText {
                        text: image_open_tag_text(),
                    },
                    ContentItem::InputImage { image_url },
                    ContentItem::InputText {
                        text: image_close_tag_text(),
                    },
                ],
                UserInput::LocalImage { path } => {
                    image_index += 1;
                    local_image_content_items_with_label_number(&path, Some(image_index))
                }
                UserInput::Skill { .. } | UserInput::Mention { .. } => Vec::new(),
            })
            .collect::<Vec<ContentItem>>(),
        end_turn: None,
    }
}

pub fn tool_result_payload_from_call_tool_result(
    call_tool_result: &CallToolResult,
) -> ToolResultPayload {
    let CallToolResult {
        content,
        structured_content,
        is_error,
    } = call_tool_result;

    let success = Some(is_error != &Some(true));

    if let Some(structured_content) = structured_content
        && !structured_content.is_null()
    {
        return match serde_json::to_string(structured_content) {
            Ok(content) => ToolResultPayload::Structured {
                content,
                content_items: None,
                success,
            },
            Err(err) => ToolResultPayload::Structured {
                content: err.to_string(),
                content_items: None,
                success: Some(false),
            },
        };
    }

    let serialized_content = match serde_json::to_string(content) {
        Ok(content) => content,
        Err(err) => {
            return ToolResultPayload::Structured {
                content: err.to_string(),
                content_items: None,
                success: Some(false),
            };
        }
    };

    ToolResultPayload::Structured {
        content: serialized_content,
        content_items: content_blocks_to_tool_result_items(content),
        success,
    }
}

fn content_blocks_to_tool_result_items(
    blocks: &[ContentBlock],
) -> Option<Vec<ToolResultContentItem>> {
    let mut saw_image = false;
    let mut items = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            ContentBlock::TextContent(text) => {
                items.push(ToolResultContentItem::InputText {
                    text: text.text.clone(),
                });
            }
            ContentBlock::ImageContent(image) => {
                saw_image = true;
                let image_url = if image.data.starts_with("data:") {
                    image.data.clone()
                } else {
                    format!("data:{};base64,{}", image.mime_type, image.data)
                };
                items.push(ToolResultContentItem::InputImage { image_url });
            }
            _ => return None,
        }
    }

    if saw_image { Some(items) } else { None }
}

/// If the `name` of a `TranscriptItem::FunctionCall` is either `container.exec`
/// or `shell`, the `arguments` field should deserialize to this struct.
#[derive(Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct ShellToolCallParams {
    pub command: Vec<String>,
    pub workdir: Option<String>,

    /// This is the maximum time in milliseconds that the command is allowed to run.
    #[serde(alias = "timeout")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_permissions: Option<SandboxPermissions>,
    /// Suggests a command prefix to persist for future sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix_rule: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// If the `name` of a `TranscriptItem::FunctionCall` is `shell_command`, the
/// `arguments` field should deserialize to this struct.
#[derive(Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct ShellCommandToolCallParams {
    pub command: String,
    pub workdir: Option<String>,

    /// Whether to run the shell with login shell semantics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<bool>,
    /// This is the maximum time in milliseconds that the command is allowed to run.
    #[serde(alias = "timeout")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_permissions: Option<SandboxPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix_rule: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

// (Moved event mapping logic into adam-coding-agent to avoid coupling protocol to UI-facing events.)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_types::SandboxMode;
    use crate::protocol::AskForApproval;
    use adam_execpolicy::Policy;
    use adam_llm_types::TranscriptItem;
    use anyhow::Result;
    use mcp_types::ImageContent;
    use mcp_types::TextContent;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn converts_sandbox_mode_into_developer_instructions() {
        let workspace_write: DeveloperInstructions = SandboxMode::WorkspaceWrite.into();
        assert_eq!(
            workspace_write,
            DeveloperInstructions::new(
                "Filesystem sandboxing defines which files can be read or written. `sandbox_mode` is `workspace-write`: The sandbox permits reading files, and editing files in `cwd` and `writable_roots`. Editing files in other directories requires approval. Network access is restricted."
            )
        );

        let read_only: DeveloperInstructions = SandboxMode::ReadOnly.into();
        assert_eq!(
            read_only,
            DeveloperInstructions::new(
                "Filesystem sandboxing defines which files can be read or written. `sandbox_mode` is `read-only`: The sandbox only permits reading files. Network access is restricted."
            )
        );
    }

    #[test]
    fn builds_permissions_with_network_access_override() {
        let instructions = DeveloperInstructions::from_permissions_with_network(
            SandboxMode::WorkspaceWrite,
            NetworkAccess::Enabled,
            AskForApproval::OnRequest,
            &Policy::empty(),
            false,
            None,
        );

        let text = instructions.into_text();
        assert!(
            text.contains("Network access is enabled."),
            "expected network access to be enabled in message"
        );
        assert!(
            text.contains("`approval_policy` is `on-request`"),
            "expected approval guidance to be included"
        );
    }

    #[test]
    fn builds_permissions_from_policy() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };

        let instructions = DeveloperInstructions::from_policy(
            &policy,
            AskForApproval::UnlessTrusted,
            &Policy::empty(),
            false,
            &PathBuf::from("/tmp"),
        );
        let text = instructions.into_text();
        assert!(text.contains("Network access is enabled."));
        assert!(text.contains("`approval_policy` is `unless-trusted`"));
    }

    #[test]
    fn includes_request_rule_instructions_when_enabled() {
        let mut exec_policy = Policy::empty();
        exec_policy
            .add_prefix_rule(
                &["git".to_string(), "pull".to_string()],
                adam_execpolicy::Decision::Allow,
            )
            .expect("add rule");
        let instructions = DeveloperInstructions::from_permissions_with_network(
            SandboxMode::WorkspaceWrite,
            NetworkAccess::Enabled,
            AskForApproval::OnRequest,
            &exec_policy,
            true,
            None,
        );

        let text = instructions.into_text();
        assert!(text.contains("prefix_rule"));
        assert!(text.contains("Approved command prefixes"));
        assert!(text.contains(r#"["git", "pull"]"#));
    }

    #[test]
    fn render_command_prefix_list_sorts_by_len_then_total_len_then_alphabetical() {
        let prefixes = vec![
            vec!["b".to_string(), "zz".to_string()],
            vec!["aa".to_string()],
            vec!["b".to_string()],
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["a".to_string()],
            vec!["b".to_string(), "a".to_string()],
        ];

        let output = format_allow_prefixes(prefixes).expect("rendered list");
        assert_eq!(
            output,
            r#"- ["a"]
- ["b"]
- ["aa"]
- ["b", "a"]
- ["b", "zz"]
- ["a", "b", "c"]"#
                .to_string(),
        );
    }

    #[test]
    fn render_command_prefix_list_limits_output_to_max_prefixes() {
        let prefixes = (0..(MAX_RENDERED_PREFIXES + 5))
            .map(|i| vec![format!("{i:03}")])
            .collect::<Vec<_>>();

        let output = format_allow_prefixes(prefixes).expect("rendered list");
        assert_eq!(output.ends_with(TRUNCATED_MARKER), true);
        eprintln!("output: {output}");
        assert_eq!(output.lines().count(), MAX_RENDERED_PREFIXES + 1);
    }

    #[test]
    fn format_allow_prefixes_limits_output() {
        let mut exec_policy = Policy::empty();
        for i in 0..200 {
            exec_policy
                .add_prefix_rule(
                    &[format!("tool-{i:03}"), "x".repeat(500)],
                    adam_execpolicy::Decision::Allow,
                )
                .expect("add rule");
        }

        let output =
            format_allow_prefixes(exec_policy.get_allowed_prefixes()).expect("formatted prefixes");
        assert!(
            output.len() <= MAX_ALLOW_PREFIX_TEXT_BYTES + TRUNCATED_MARKER.len(),
            "output length exceeds expected limit: {output}",
        );
    }

    #[test]
    fn tool_result_payload_from_call_tool_result_preserves_image_blocks() -> Result<()> {
        let call_tool_result = CallToolResult {
            content: vec![
                ContentBlock::TextContent(TextContent {
                    annotations: None,
                    text: "caption".into(),
                    r#type: "text".into(),
                }),
                ContentBlock::ImageContent(ImageContent {
                    annotations: None,
                    data: "BASE64".into(),
                    mime_type: "image/png".into(),
                    r#type: "image".into(),
                }),
            ],
            is_error: None,
            structured_content: None,
        };

        let ToolResultPayload::Structured {
            content,
            content_items,
            success,
        } = tool_result_payload_from_call_tool_result(&call_tool_result)
        else {
            panic!("expected structured payload");
        };
        assert_eq!(success, Some(true));
        let items = content_items.expect("content items");
        assert_eq!(
            items,
            vec![
                ToolResultContentItem::InputText {
                    text: "caption".into(),
                },
                ToolResultContentItem::InputImage {
                    image_url: "data:image/png;base64,BASE64".into(),
                },
            ]
        );
        assert_eq!(content, serde_json::to_string(&call_tool_result.content)?);
        Ok(())
    }

    #[test]
    fn tool_result_payload_from_call_tool_result_prefers_structured_content() -> Result<()> {
        let call_tool_result = CallToolResult {
            content: vec![ContentBlock::TextContent(TextContent {
                annotations: None,
                text: "caption".into(),
                r#type: "text".into(),
            })],
            is_error: Some(true),
            structured_content: Some(serde_json::json!({
                "summary": "structured wins",
                "count": 2,
            })),
        };

        let ToolResultPayload::Structured {
            content,
            content_items,
            success,
        } = tool_result_payload_from_call_tool_result(&call_tool_result)
        else {
            panic!("expected structured payload");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&content)?,
            serde_json::json!({
                "summary": "structured wins",
                "count": 2,
            })
        );
        assert_eq!(content_items, None);
        assert_eq!(success, Some(false));
        Ok(())
    }

    #[test]
    fn transcript_item_roundtrips_web_search_hosted_activity() -> Result<()> {
        let payload = serde_json::to_value(WebSearchAction::FindInPage {
            url: Some("https://example.com/docs".into()),
            pattern: Some("installation".into()),
        })?;
        let item = TranscriptItem::HostedActivity {
            id: Some("ws_partial".into()),
            activity_type: "web_search".into(),
            status: Some("in_progress".into()),
            payload: payload.clone(),
        };

        let json = serde_json::to_value(&item)?;
        assert_eq!(
            json,
            serde_json::json!({
                "type": "hosted_activity",
                "id": "ws_partial",
                "activity_type": "web_search",
                "status": "in_progress",
                "payload": payload,
            })
        );
        let parsed: TranscriptItem = serde_json::from_value(json)?;
        assert_eq!(parsed, item);
        Ok(())
    }

    #[test]
    fn deserialize_shell_tool_call_params() -> Result<()> {
        let json = r#"{
            "command": ["ls", "-l"],
            "workdir": "/tmp",
            "timeout": 1000
        }"#;

        let params: ShellToolCallParams = serde_json::from_str(json)?;
        assert_eq!(
            ShellToolCallParams {
                command: vec!["ls".to_string(), "-l".to_string()],
                workdir: Some("/tmp".to_string()),
                timeout_ms: Some(1000),
                sandbox_permissions: None,
                prefix_rule: None,
                justification: None,
            },
            params
        );
        Ok(())
    }

    #[test]
    fn wraps_image_user_input_with_tags() -> Result<()> {
        let image_url = "data:image/png;base64,abc".to_string();

        let item = transcript_item_from_user_input(vec![UserInput::Image {
            image_url: image_url.clone(),
        }]);

        match item {
            TranscriptItem::Message { content, .. } => {
                let expected = vec![
                    ContentItem::InputText {
                        text: image_open_tag_text(),
                    },
                    ContentItem::InputImage { image_url },
                    ContentItem::InputText {
                        text: image_close_tag_text(),
                    },
                ];
                assert_eq!(content, expected);
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn local_image_read_error_adds_placeholder() -> Result<()> {
        let dir = tempdir()?;
        let missing_path = dir.path().join("missing-image.png");

        let item = transcript_item_from_user_input(vec![UserInput::LocalImage {
            path: missing_path.clone(),
        }]);

        match item {
            TranscriptItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentItem::InputText { text } => {
                        let display_path = missing_path.display().to_string();
                        assert!(
                            text.contains(&display_path),
                            "placeholder should mention missing path: {text}"
                        );
                        assert!(
                            text.contains("could not read"),
                            "placeholder should mention read issue: {text}"
                        );
                    }
                    other => panic!("expected placeholder text but found {other:?}"),
                }
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn local_image_non_image_adds_placeholder() -> Result<()> {
        let dir = tempdir()?;
        let json_path = dir.path().join("example.json");
        std::fs::write(&json_path, br#"{"hello":"world"}"#)?;

        let item = transcript_item_from_user_input(vec![UserInput::LocalImage {
            path: json_path.clone(),
        }]);

        match item {
            TranscriptItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentItem::InputText { text } => {
                        assert!(
                            text.contains("unsupported MIME type `application/json`"),
                            "placeholder should mention unsupported MIME: {text}"
                        );
                        assert!(
                            text.contains(&json_path.display().to_string()),
                            "placeholder should mention path: {text}"
                        );
                    }
                    other => panic!("expected placeholder text but found {other:?}"),
                }
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }

    #[test]
    fn local_image_unsupported_image_format_adds_placeholder() -> Result<()> {
        let dir = tempdir()?;
        let svg_path = dir.path().join("example.svg");
        std::fs::write(
            &svg_path,
            br#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>"#,
        )?;

        let item = transcript_item_from_user_input(vec![UserInput::LocalImage {
            path: svg_path.clone(),
        }]);

        match item {
            TranscriptItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                let expected = format!(
                    "Codex cannot attach image at `{}`: unsupported image format `image/svg+xml`.",
                    svg_path.display()
                );
                match &content[0] {
                    ContentItem::InputText { text } => assert_eq!(text.as_str(), expected),
                    other => panic!("expected placeholder text but found {other:?}"),
                }
            }
            other => panic!("expected message response but got {other:?}"),
        }

        Ok(())
    }
}
