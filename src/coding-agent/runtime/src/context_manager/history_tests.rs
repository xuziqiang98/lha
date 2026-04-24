use super::*;
use crate::instructions::SkillInstructions;
use crate::truncate;
use crate::truncate::TruncationPolicy;
use codex_llm::ToolCallPayload;
use codex_llm::ToolResultContentItem;
use codex_llm::ToolResultPayload;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::TranscriptItem;
use pretty_assertions::assert_eq;
use regex_lite::Regex;
use serde_json::Value;

const EXEC_FORMAT_MAX_BYTES: usize = 10_000;
const EXEC_FORMAT_MAX_TOKENS: usize = 2_500;

fn assistant_msg(text: &str) -> TranscriptItem {
    TranscriptItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        end_turn: None,
    }
}

fn create_history_with_items(items: Vec<TranscriptItem>) -> ContextManager {
    let mut h = ContextManager::new();
    // Use a generous but fixed token budget; tests only rely on truncation
    // behavior, not on a specific model's token limit.
    h.record_items(items.iter(), TruncationPolicy::Tokens(10_000));
    h
}

fn user_msg(text: &str) -> TranscriptItem {
    TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        end_turn: None,
    }
}

fn user_input_text_msg(text: &str) -> TranscriptItem {
    TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
    }
}

fn reasoning_msg(text: &str) -> TranscriptItem {
    TranscriptItem::Reasoning {
        id: String::new(),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "summary".to_string(),
        }],
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: text.to_string(),
        }]),
        encrypted_content: None,
    }
}

fn reasoning_with_encrypted_content(len: usize) -> TranscriptItem {
    TranscriptItem::Reasoning {
        id: String::new(),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "summary".to_string(),
        }],
        content: None,
        encrypted_content: Some("a".repeat(len)),
    }
}

fn tool_call_json(tool_name: &str, call_id: &str, arguments: &str) -> TranscriptItem {
    TranscriptItem::ToolCall {
        id: None,
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        payload: ToolCallPayload::JsonArguments {
            arguments: arguments.to_string(),
        },
    }
}

fn tool_call_text(tool_name: &str, call_id: &str, input: &str) -> TranscriptItem {
    TranscriptItem::ToolCall {
        id: None,
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        payload: ToolCallPayload::TextInput {
            input: input.to_string(),
        },
    }
}

fn tool_result_structured(
    tool_name: &str,
    call_id: &str,
    content: &str,
    content_items: Option<Vec<ToolResultContentItem>>,
    success: Option<bool>,
) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        payload: ToolResultPayload::Structured {
            content: content.to_string(),
            content_items,
            success,
        },
    }
}

fn tool_result_text(tool_name: &str, call_id: &str, output: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        payload: ToolResultPayload::Text {
            output: output.to_string(),
        },
    }
}

fn backfilled_skill_item(skill: SkillInstructions) -> TranscriptItem {
    skill.into_backfilled_transcript_item()
}

fn direct_skill_item(skill: SkillInstructions) -> TranscriptItem {
    skill.into()
}

fn truncate_exec_output(content: &str) -> String {
    truncate::truncate_text(content, TruncationPolicy::Tokens(EXEC_FORMAT_MAX_TOKENS))
}

#[test]
fn filters_non_api_messages() {
    let mut h = ContextManager::default();
    let policy = TruncationPolicy::Tokens(10_000);
    let system = TranscriptItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::OutputText {
            text: "ignored".to_string(),
        }],
        end_turn: None,
    };
    let reasoning = reasoning_msg("thinking...");
    let unknown = TranscriptItem::Unknown { raw: Value::Null };
    h.record_items([&system, &reasoning, &unknown], policy);

    let u = user_msg("hi");
    let a = assistant_msg("hello");
    h.record_items([&u, &a], policy);

    let items = h.raw_items();
    assert_eq!(
        items,
        vec![
            TranscriptItem::Reasoning {
                id: String::new(),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "summary".to_string(),
                }],
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: "thinking...".to_string(),
                }]),
                encrypted_content: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "hi".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "hello".to_string(),
                }],
                end_turn: None,
            },
        ]
    );
}

#[test]
fn non_last_reasoning_tokens_return_zero_when_no_user_messages() {
    let history = create_history_with_items(vec![reasoning_with_encrypted_content(800)]);

    assert_eq!(history.get_non_last_reasoning_items_tokens(), 0);
}

#[test]
fn non_last_reasoning_tokens_ignore_entries_after_last_user() {
    let history = create_history_with_items(vec![
        reasoning_with_encrypted_content(900),
        user_msg("first"),
        reasoning_with_encrypted_content(1_000),
        user_msg("second"),
        reasoning_with_encrypted_content(2_000),
    ]);
    assert_eq!(history.get_non_last_reasoning_items_tokens(), 32);
}

#[test]
fn get_history_for_compaction_prompt_drops_compact_backfilled_skills() {
    let items = vec![
        backfilled_skill_item(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "body".to_string(),
        }),
        assistant_msg("summary"),
    ];
    let history = create_history_with_items(items);

    assert_eq!(
        history.for_compaction_prompt(),
        vec![assistant_msg("summary")]
    );
}

#[test]
fn get_history_for_compaction_prompt_keeps_direct_skills() {
    let direct = direct_skill_item(SkillInstructions {
        name: "demo".to_string(),
        path: "skills/demo/SKILL.md".to_string(),
        contents: "body".to_string(),
    });
    let history = create_history_with_items(vec![direct.clone()]);

    assert_eq!(history.for_compaction_prompt(), vec![direct]);
}

#[test]
fn get_history_for_prompt_keeps_compact_backfilled_skills() {
    let backfilled = backfilled_skill_item(SkillInstructions {
        name: "demo".to_string(),
        path: "skills/demo/SKILL.md".to_string(),
        contents: "body".to_string(),
    });
    let history = create_history_with_items(vec![backfilled.clone()]);

    assert_eq!(history.for_prompt(), vec![backfilled]);
}

#[test]
fn remove_first_item_removes_matching_output_for_function_call() {
    let items = vec![
        tool_call_json("do_it", "call-1", "{}"),
        tool_result_structured("do_it", "call-1", "ok", None, None),
    ];
    let mut h = create_history_with_items(items);
    h.remove_first_item();
    assert_eq!(h.raw_items(), vec![]);
}

#[test]
fn remove_first_item_removes_matching_call_for_output() {
    let items = vec![
        tool_result_structured("do_it", "call-2", "ok", None, None),
        tool_call_json("do_it", "call-2", "{}"),
    ];
    let mut h = create_history_with_items(items);
    h.remove_first_item();
    assert_eq!(h.raw_items(), vec![]);
}

#[test]
fn replace_last_turn_images_replaces_tool_output_images() {
    let items = vec![
        user_input_text_msg("hi"),
        tool_result_structured(
            "view_image",
            "call-1",
            "ok",
            Some(vec![ToolResultContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
            }]),
            Some(true),
        ),
    ];
    let mut history = create_history_with_items(items);

    assert!(history.replace_last_turn_images("Invalid image"));

    assert_eq!(
        history.raw_items(),
        vec![
            user_input_text_msg("hi"),
            tool_result_structured(
                "view_image",
                "call-1",
                "ok",
                Some(vec![ToolResultContentItem::InputText {
                    text: "Invalid image".to_string(),
                }]),
                Some(true),
            ),
        ]
    );
}

#[test]
fn replace_last_turn_images_does_not_touch_user_images() {
    let items = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image_url: "data:image/png;base64,AAA".to_string(),
        }],
        end_turn: None,
    }];
    let mut history = create_history_with_items(items.clone());

    assert!(!history.replace_last_turn_images("Invalid image"));
    assert_eq!(history.raw_items(), items);
}

#[test]
fn remove_first_item_handles_local_shell_pair() {
    let arguments = serde_json::json!({
        "command": ["echo", "hi"],
    })
    .to_string();
    let items = vec![
        tool_call_json("local_shell", "call-3", &arguments),
        tool_result_structured("local_shell", "call-3", "ok", None, None),
    ];
    let mut h = create_history_with_items(items);
    h.remove_first_item();
    assert_eq!(h.raw_items(), vec![]);
}

#[test]
fn drop_last_n_user_turns_preserves_prefix() {
    let items = vec![
        assistant_msg("session prefix item"),
        user_msg("u1"),
        assistant_msg("a1"),
        user_msg("u2"),
        assistant_msg("a2"),
    ];

    let mut history = create_history_with_items(items);
    history.drop_last_n_user_turns(1);
    assert_eq!(
        history.for_prompt(),
        vec![
            assistant_msg("session prefix item"),
            user_msg("u1"),
            assistant_msg("a1"),
        ]
    );

    let mut history = create_history_with_items(vec![
        assistant_msg("session prefix item"),
        user_msg("u1"),
        assistant_msg("a1"),
        user_msg("u2"),
        assistant_msg("a2"),
    ]);
    history.drop_last_n_user_turns(99);
    assert_eq!(
        history.for_prompt(),
        vec![assistant_msg("session prefix item")]
    );
}

#[test]
fn drop_last_n_user_turns_ignores_session_prefix_user_messages() {
    let items = vec![
        user_input_text_msg("<environment_context>ctx</environment_context>"),
        user_input_text_msg("<user_instructions>do the thing</user_instructions>"),
        user_input_text_msg(
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        ),
        user_input_text_msg(
            "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        ),
        user_input_text_msg("<user_shell_command>echo 42</user_shell_command>"),
        user_input_text_msg("turn 1 user"),
        assistant_msg("turn 1 assistant"),
        user_input_text_msg("turn 2 user"),
        assistant_msg("turn 2 assistant"),
    ];

    let mut history = create_history_with_items(items);
    history.drop_last_n_user_turns(1);

    let expected_prefix_and_first_turn = vec![
        user_input_text_msg("<environment_context>ctx</environment_context>"),
        user_input_text_msg("<user_instructions>do the thing</user_instructions>"),
        user_input_text_msg(
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        ),
        user_input_text_msg(
            "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        ),
        user_input_text_msg("<user_shell_command>echo 42</user_shell_command>"),
        user_input_text_msg("turn 1 user"),
        assistant_msg("turn 1 assistant"),
    ];

    assert_eq!(history.for_prompt(), expected_prefix_and_first_turn);

    let expected_prefix_only = vec![
        user_input_text_msg("<environment_context>ctx</environment_context>"),
        user_input_text_msg("<user_instructions>do the thing</user_instructions>"),
        user_input_text_msg(
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        ),
        user_input_text_msg(
            "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        ),
        user_input_text_msg("<user_shell_command>echo 42</user_shell_command>"),
    ];

    let mut history = create_history_with_items(vec![
        user_input_text_msg("<environment_context>ctx</environment_context>"),
        user_input_text_msg("<user_instructions>do the thing</user_instructions>"),
        user_input_text_msg(
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        ),
        user_input_text_msg(
            "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        ),
        user_input_text_msg("<user_shell_command>echo 42</user_shell_command>"),
        user_input_text_msg("turn 1 user"),
        assistant_msg("turn 1 assistant"),
        user_input_text_msg("turn 2 user"),
        assistant_msg("turn 2 assistant"),
    ]);
    history.drop_last_n_user_turns(2);
    assert_eq!(history.for_prompt(), expected_prefix_only);

    let mut history = create_history_with_items(vec![
        user_input_text_msg("<environment_context>ctx</environment_context>"),
        user_input_text_msg("<user_instructions>do the thing</user_instructions>"),
        user_input_text_msg(
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        ),
        user_input_text_msg(
            "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        ),
        user_input_text_msg("<user_shell_command>echo 42</user_shell_command>"),
        user_input_text_msg("turn 1 user"),
        assistant_msg("turn 1 assistant"),
        user_input_text_msg("turn 2 user"),
        assistant_msg("turn 2 assistant"),
    ]);
    history.drop_last_n_user_turns(3);
    assert_eq!(history.for_prompt(), expected_prefix_only);
}

#[test]
fn remove_first_item_handles_custom_tool_pair() {
    let items = vec![
        tool_call_text("my_tool", "tool-1", "{}"),
        tool_result_text("my_tool", "tool-1", "ok"),
    ];
    let mut h = create_history_with_items(items);
    h.remove_first_item();
    assert_eq!(h.raw_items(), vec![]);
}

#[test]
fn normalization_retains_local_shell_outputs() {
    let arguments = serde_json::json!({
        "command": ["echo", "hi"],
    })
    .to_string();
    let items = vec![
        tool_call_json("local_shell", "shell-1", &arguments),
        tool_result_structured(
            "local_shell",
            "shell-1",
            "Total output lines: 1\n\nok",
            None,
            None,
        ),
    ];

    let history = create_history_with_items(items.clone());
    let normalized = history.for_prompt();
    assert_eq!(normalized, items);
}

#[test]
fn record_items_truncates_structured_tool_result_content() {
    let mut history = ContextManager::new();
    let policy = TruncationPolicy::Tokens(1_000);
    let long_line = "a very long line to trigger truncation\n";
    let long_output = long_line.repeat(2_500);
    let item = tool_result_structured("do_it", "call-100", &long_output, None, Some(true));

    history.record_items([&item], policy);

    assert_eq!(history.items.len(), 1);
    match &history.items[0] {
        TranscriptItem::ToolResult {
            payload: ToolResultPayload::Structured { content, .. },
            ..
        } => {
            assert_ne!(content, &long_output);
            assert!(
                content.contains("tokens truncated"),
                "expected token-based truncation marker, got {content}",
            );
        }
        other => panic!("unexpected history item: {other:?}"),
    }
}

#[test]
fn record_items_truncates_text_tool_result_content() {
    let mut history = ContextManager::new();
    let policy = TruncationPolicy::Tokens(1_000);
    let line = "custom output that is very long\n";
    let long_output = line.repeat(2_500);
    let item = tool_result_text("my_tool", "tool-200", &long_output);

    history.record_items([&item], policy);

    assert_eq!(history.items.len(), 1);
    match &history.items[0] {
        TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } => {
            assert_ne!(output, &long_output);
            assert!(
                output.contains("tokens truncated"),
                "expected token-based truncation marker, got {output}",
            );
            assert!(
                output.contains("tokens truncated") || output.contains("bytes truncated"),
                "expected truncation marker, got {output}",
            );
        }
        other => panic!("unexpected history item: {other:?}"),
    }
}

#[test]
fn record_items_respects_custom_token_limit() {
    let mut history = ContextManager::new();
    let policy = TruncationPolicy::Tokens(10);
    let long_output = "tokenized content repeated many times ".repeat(200);
    let item = tool_result_structured("do_it", "call-custom-limit", &long_output, None, Some(true));

    history.record_items([&item], policy);

    let stored = match &history.items[0] {
        TranscriptItem::ToolResult {
            payload: ToolResultPayload::Structured { content, .. },
            ..
        } => content,
        other => panic!("unexpected history item: {other:?}"),
    };
    assert!(stored.contains("tokens truncated"));
}

fn assert_truncated_message_matches(message: &str, line: &str, expected_removed: usize) {
    let pattern = truncated_message_pattern(line);
    let regex = Regex::new(&pattern).unwrap_or_else(|err| {
        panic!("failed to compile regex {pattern}: {err}");
    });
    let captures = regex
        .captures(message)
        .unwrap_or_else(|| panic!("message failed to match pattern {pattern}: {message}"));
    let body = captures
        .name("body")
        .expect("missing body capture")
        .as_str();
    assert!(
        body.len() <= EXEC_FORMAT_MAX_BYTES,
        "body exceeds byte limit: {} bytes",
        body.len()
    );
    let removed: usize = captures
        .name("removed")
        .expect("missing removed capture")
        .as_str()
        .parse()
        .unwrap_or_else(|err| panic!("invalid removed tokens: {err}"));
    assert_eq!(removed, expected_removed, "mismatched removed token count");
}

fn truncated_message_pattern(line: &str) -> String {
    let escaped_line = regex_lite::escape(line);
    format!(r"(?s)^(?P<body>{escaped_line}.*?)(?:\r?)?…(?P<removed>\d+) tokens truncated…(?:.*)?$")
}

#[test]
fn format_exec_output_truncates_large_error() {
    let line = "very long execution error line that should trigger truncation\n";
    let large_error = line.repeat(2_500);

    let truncated = truncate_exec_output(&large_error);

    assert_truncated_message_matches(&truncated, line, 36250);
    assert_ne!(truncated, large_error);
}

#[test]
fn format_exec_output_marks_byte_truncation_without_omitted_lines() {
    let long_line = "a".repeat(EXEC_FORMAT_MAX_BYTES + 10000);
    let truncated = truncate_exec_output(&long_line);
    assert_ne!(truncated, long_line);
    assert_truncated_message_matches(&truncated, "a", 2500);
    assert!(
        !truncated.contains("omitted"),
        "line omission marker should not appear when no lines were dropped: {truncated}"
    );
}

#[test]
fn format_exec_output_returns_original_when_within_limits() {
    let content = "example output\n".repeat(10);
    assert_eq!(truncate_exec_output(&content), content);
}

#[test]
fn format_exec_output_reports_omitted_lines_and_keeps_head_and_tail() {
    let total_lines = 2_000;
    let filler = "x".repeat(64);
    let content: String = (0..total_lines)
        .map(|idx| format!("line-{idx}-{filler}\n"))
        .collect();

    let truncated = truncate_exec_output(&content);
    assert_truncated_message_matches(&truncated, "line-0-", 34_723);
    assert!(
        truncated.contains("line-0-"),
        "expected head line to remain: {truncated}"
    );

    let last_line = format!("line-{}-", total_lines - 1);
    assert!(
        truncated.contains(&last_line),
        "expected tail line to remain: {truncated}"
    );
}

#[test]
fn format_exec_output_prefers_line_marker_when_both_limits_exceeded() {
    let total_lines = 300;
    let long_line = "x".repeat(256);
    let content: String = (0..total_lines)
        .map(|idx| format!("line-{idx}-{long_line}\n"))
        .collect();

    let truncated = truncate_exec_output(&content);

    assert_truncated_message_matches(&truncated, "line-0-", 17_423);
}

#[cfg(not(debug_assertions))]
#[test]
fn normalize_adds_missing_output_for_function_call() {
    let items = vec![tool_call_json("do_it", "call-x", "{}")];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(
        h.raw_items(),
        vec![
            tool_call_json("do_it", "call-x", "{}"),
            tool_result_structured("do_it", "call-x", "aborted", None, None),
        ]
    );
}

#[cfg(not(debug_assertions))]
#[test]
fn normalize_adds_missing_output_for_custom_tool_call() {
    let items = vec![tool_call_text("custom", "tool-x", "{}")];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(
        h.raw_items(),
        vec![
            tool_call_text("custom", "tool-x", "{}"),
            tool_result_text("custom", "tool-x", "aborted"),
        ]
    );
}

#[cfg(not(debug_assertions))]
#[test]
fn normalize_adds_missing_output_for_local_shell_call_with_id() {
    let arguments = serde_json::json!({
        "command": ["echo", "hi"],
    })
    .to_string();
    let items = vec![tool_call_json("local_shell", "shell-1", &arguments)];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(
        h.raw_items(),
        vec![
            tool_call_json("local_shell", "shell-1", &arguments),
            tool_result_structured("local_shell", "shell-1", "aborted", None, None),
        ]
    );
}

#[cfg(debug_assertions)]
#[test]
fn normalize_adds_missing_output_for_local_shell_call_with_id_inserts_output_in_debug() {
    let arguments = serde_json::json!({
        "command": ["echo", "hi"],
    })
    .to_string();
    let items = vec![tool_call_json("local_shell", "shell-1", &arguments)];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(
        h.raw_items(),
        vec![
            tool_call_json("local_shell", "shell-1", &arguments),
            tool_result_structured("local_shell", "shell-1", "aborted", None, None),
        ]
    );
}

#[test]
fn normalize_drops_empty_call_id_output_but_keeps_raw_history_until_normalized() {
    let items = vec![tool_result_structured(
        "local_shell",
        "",
        "LocalShellCall without call_id or id",
        None,
        None,
    )];
    let mut h = create_history_with_items(items.clone());

    assert_eq!(h.raw_items(), items);

    h.normalize_history();

    assert_eq!(h.raw_items(), vec![]);
}

#[cfg(not(debug_assertions))]
#[test]
fn normalize_removes_orphan_function_call_output() {
    let items = vec![tool_result_structured(
        "do_it", "orphan-1", "ok", None, None,
    )];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(h.raw_items(), vec![]);
}

#[cfg(not(debug_assertions))]
#[test]
fn normalize_removes_orphan_custom_tool_call_output() {
    let items = vec![tool_result_text("custom", "orphan-2", "ok")];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(h.raw_items(), vec![]);
}

#[cfg(not(debug_assertions))]
#[test]
fn normalize_mixed_inserts_and_removals() {
    let local_shell_arguments = serde_json::json!({
        "command": ["echo"],
    })
    .to_string();
    let items = vec![
        tool_call_json("f1", "c1", "{}"),
        tool_result_structured("orphan", "c2", "ok", None, None),
        tool_call_text("tool", "t1", "{}"),
        tool_call_json("local_shell", "s1", &local_shell_arguments),
    ];
    let mut h = create_history_with_items(items);

    h.normalize_history();

    assert_eq!(
        h.raw_items(),
        vec![
            tool_call_json("f1", "c1", "{}"),
            tool_result_structured("f1", "c1", "aborted", None, None),
            tool_call_text("tool", "t1", "{}"),
            tool_result_text("tool", "t1", "aborted"),
            tool_call_json("local_shell", "s1", &local_shell_arguments),
            tool_result_structured("local_shell", "s1", "aborted", None, None),
        ]
    );
}

#[test]
fn normalize_adds_missing_output_for_function_call_inserts_output() {
    let items = vec![tool_call_json("do_it", "call-x", "{}")];
    let mut h = create_history_with_items(items);
    h.normalize_history();
    assert_eq!(
        h.raw_items(),
        vec![
            tool_call_json("do_it", "call-x", "{}"),
            tool_result_structured("do_it", "call-x", "aborted", None, None),
        ]
    );
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn normalize_adds_missing_output_for_custom_tool_call_panics_in_debug() {
    let items = vec![tool_call_text("custom", "tool-x", "{}")];
    let mut h = create_history_with_items(items);
    h.normalize_history();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn normalize_removes_orphan_function_call_output_panics_in_debug() {
    let items = vec![tool_result_structured(
        "do_it", "orphan-1", "ok", None, None,
    )];
    let mut h = create_history_with_items(items);
    h.normalize_history();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn normalize_removes_orphan_custom_tool_call_output_panics_in_debug() {
    let items = vec![tool_result_text("custom", "orphan-2", "ok")];
    let mut h = create_history_with_items(items);
    h.normalize_history();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn normalize_mixed_inserts_and_removals_panics_in_debug() {
    let local_shell_arguments = serde_json::json!({
        "command": ["echo"],
    })
    .to_string();
    let items = vec![
        tool_call_json("f1", "c1", "{}"),
        tool_result_structured("orphan", "c2", "ok", None, None),
        tool_call_text("tool", "t1", "{}"),
        tool_call_json("local_shell", "s1", &local_shell_arguments),
    ];
    let mut h = create_history_with_items(items);
    h.normalize_history();
}
