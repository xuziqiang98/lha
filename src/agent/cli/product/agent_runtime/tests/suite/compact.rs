#![allow(clippy::expect_used)]
use crate::product::agent::CodexAuth;
use crate::product::agent::compact::SUMMARIZATION_PROMPT;
use crate::product::agent::compact::SUMMARY_PREFIX;
use crate::product::agent::config::Config;
use crate::product::agent::features::Feature;
use crate::product::agent::protocol::AskForApproval;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ItemCompletedEvent;
use crate::product::agent::protocol::ItemStartedEvent;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::RolloutItem;
use crate::product::agent::protocol::RolloutLine;
use crate::product::agent::protocol::SandboxPolicy;
use crate::product::agent::protocol::WarningEvent;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::ReasoningSummary;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::TranscriptItem;
use crate::product::protocol::plan_tool::PlanItemArg;
use crate::product::protocol::plan_tool::StepStatus;
use crate::product::protocol::plan_tool::UpdatePlanArgs;
use crate::product::protocol::protocol::ThreadGoalStatus;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::responses::ev_local_shell_call;
use crate::test_support::core::responses::ev_reasoning_item;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use crate::test_support::core::wait_for_event_match;
use lha_llm::RuntimeEndpoint;
use lha_llm::ToolCallPayload;
use lha_llm::ToolResultPayload;
use lha_llm::built_in_runtime_endpoints;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::HashSet;
use std::collections::VecDeque;

use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_completed_with_tokens;
use crate::test_support::core::responses::ev_function_call;
use crate::test_support::core::responses::mount_compact_json_once;
use crate::test_support::core::responses::mount_response_sequence;
use crate::test_support::core::responses::mount_sse_once;
use crate::test_support::core::responses::mount_sse_once_match;
use crate::test_support::core::responses::mount_sse_sequence;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::sse_failed;
use crate::test_support::core::responses::sse_response;
use crate::test_support::core::responses::start_mock_server;
use crate::test_support::core::test_codex::TestCodex;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
// --- Test helpers -----------------------------------------------------------

pub(super) const FIRST_REPLY: &str = "FIRST_REPLY";
pub(super) const SUMMARY_TEXT: &str = "SUMMARY_ONLY_CONTEXT";
const THIRD_USER_MSG: &str = "next turn";
const AUTO_SUMMARY_TEXT: &str = "AUTO_SUMMARY";
const FIRST_AUTO_MSG: &str = "token limit start";
const SECOND_AUTO_MSG: &str = "token limit push";
const MULTI_AUTO_MSG: &str = "multi auto";
const SECOND_LARGE_REPLY: &str = "SECOND_LARGE_REPLY";
const FIRST_AUTO_SUMMARY: &str = "FIRST_AUTO_SUMMARY";
const SECOND_AUTO_SUMMARY: &str = "SECOND_AUTO_SUMMARY";
const FINAL_REPLY: &str = "FINAL_REPLY";
const INPUT_SLIMMING_CONFLICT_WARNING: &str = "Input slimming strategies are mutually exclusive; enable only one of input_slimming or input_slimming_live_zone.";
const CONTEXT_LIMIT_MESSAGE: &str =
    "Your input exceeds the context window of this model. Please adjust your input and try again.";
const DUMMY_FUNCTION_NAME: &str = "unsupported_tool";
const DUMMY_CALL_ID: &str = "call-multi-auto";
const FUNCTION_CALL_LIMIT_MSG: &str = "function call limit push";
const POST_AUTO_USER_MSG: &str = "post auto follow-up";

pub(super) const COMPACT_WARNING_MESSAGE: &str = "Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted.";

fn auto_summary(summary: &str) -> String {
    summary.to_string()
}

fn summary_with_prefix(summary: &str) -> String {
    format!("{SUMMARY_PREFIX}\n{summary}")
}

fn write_skill(home: &Path, name: &str, description: &str, body: &str) -> std::path::PathBuf {
    let skill_dir = home.join("skills").join(name);
    fs::create_dir_all(&skill_dir).expect("create skill dir");
    let contents = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    let path = skill_dir.join("SKILL.md");
    fs::write(&path, contents).expect("write skill");
    path
}

fn drop_call_id(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(obj) => {
            obj.retain(|k, _| k != "call_id");
            for v in obj.values_mut() {
                drop_call_id(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                drop_call_id(v);
            }
        }
        _ => {}
    }
}

fn set_test_compact_prompt(config: &mut Config) {
    config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
}

fn body_contains_text(body: &str, text: &str) -> bool {
    body.contains(&json_fragment(text))
}

fn contains_user_text(input: &[serde_json::Value], expected: &str) -> bool {
    input.iter().any(|item| {
        item.get("type").and_then(|v| v.as_str()) == Some("message")
            && item.get("role").and_then(|v| v.as_str()) == Some("user")
            && item
                .get("content")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| {
                    arr.iter()
                        .any(|entry| entry.get("text").and_then(|v| v.as_str()) == Some(expected))
                })
    })
}

fn message_input_texts(body: &serde_json::Value, role: &str) -> Vec<String> {
    body.get("input")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("message"))
        .filter(|item| item.get("role").and_then(serde_json::Value::as_str) == Some(role))
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter(|span| span.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|span| {
            span.get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn contains_assistant_text(input: &[serde_json::Value], expected: &str) -> bool {
    input.iter().any(|item| {
        item.get("type").and_then(|v| v.as_str()) == Some("message")
            && item.get("role").and_then(|v| v.as_str()) == Some("assistant")
            && item
                .get("content")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| {
                    arr.iter()
                        .any(|entry| entry.get("text").and_then(|v| v.as_str()) == Some(expected))
                })
    })
}

fn contains_function_call(input: &[serde_json::Value], name: &str, arguments: &str) -> bool {
    input.iter().any(|item| {
        item.get("type").and_then(|v| v.as_str()) == Some("function_call")
            && item.get("name").and_then(|v| v.as_str()) == Some(name)
            && item.get("arguments").and_then(|v| v.as_str()) == Some(arguments)
    })
}

fn contains_function_call_output(
    input: &[serde_json::Value],
    call_id: &str,
    expected_output: &str,
) -> bool {
    input.iter().any(|item| {
        item.get("type").and_then(|v| v.as_str()) == Some("function_call_output")
            && item.get("call_id").and_then(|v| v.as_str()) == Some(call_id)
            && item.get("output").and_then(|v| v.as_str()) == Some(expected_output)
    })
}

fn json_fragment(text: &str) -> String {
    serde_json::to_string(text)
        .expect("serialize text to JSON")
        .trim_matches('"')
        .to_string()
}

fn non_openai_model_provider(server: &MockServer) -> RuntimeEndpoint {
    let mut provider = built_in_runtime_endpoints()["openai"].clone();
    provider.name = "OpenAI (test)".into();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider
}

fn openai_model_provider(server: &MockServer) -> RuntimeEndpoint {
    let mut provider = built_in_runtime_endpoints()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider
}

fn input_slimming_hash_for_test(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex = format!("{digest:x}");
    hex[..24].to_string()
}

fn input_slimming_hashes_from_text_for_test(text: &str) -> HashSet<String> {
    let mut hashes = HashSet::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("<<lha-input:") {
        let after_prefix = &remaining[start + "<<lha-input:".len()..];
        let Some(end) = after_prefix.find(">>") else {
            break;
        };
        let hash = &after_prefix[..end];
        if hash.len() == 24 && hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
            hashes.insert(hash.to_string());
        }
        remaining = &after_prefix[end + 2..];
    }
    hashes
}

fn ranked_marker_section_hashes_for_test(text: &str) -> HashSet<String> {
    let Some(section_start) = text.find("Ranked retrievable markers:") else {
        return HashSet::new();
    };
    let section = &text[section_start..];
    input_slimming_hashes_from_text_for_test(section)
}

fn input_slimming_sidecar_hashes_from_rollout(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<RolloutLine>(line).ok())
        .filter_map(|line| match line.item {
            RolloutItem::InputSlimmingStoredInput(item) => Some(item.hash),
            _ => None,
        })
        .collect()
}

fn read_file_fixture_contents(prefix: &str, line_count: usize) -> String {
    let mut contents = String::new();
    for line in 1..=line_count {
        contents.push_str(&format!("{prefix}-line-{line}\n"));
    }
    contents
}

fn read_file_tool_output(prefix: &str, line_count: usize) -> String {
    (1..=line_count)
        .map(|line| format!("L{line}: {prefix}-line-{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn switch_to_programmer_for_compact_test(test: &TestCodex) {
    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(Identity {
                kind: IdentityKind::Programmer,
                settings: Settings {
                    model: test.session_configured.model.clone(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            personality: None,
        })
        .await
        .expect("switch to programmer");
}

fn unknown_chat_model_provider(server: &MockServer) -> RuntimeEndpoint {
    let mut provider = non_openai_model_provider(server);
    provider.set_chat_turns();
    provider.set_realtime_turn_streaming_enabled(false);
    provider
}

fn ripgrep_available() -> bool {
    StdCommand::new("rg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn chat_sse_delta_and_stop(text: &str) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&json!({
            "choices": [{"delta": {"content": text}, "finish_reason": "stop"}]
        }))
        .expect("serialize chat stop response")
    )
}

fn chat_sse_delta_and_stop_with_tokens(text: &str, total_tokens: i64) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&json!({
            "choices": [{"delta": {"content": text}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": total_tokens,
                "completion_tokens": 0,
                "total_tokens": total_tokens
            }
        }))
        .expect("serialize chat stop response with usage")
    )
}

fn chat_sse_length() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&json!({
            "choices": [{"delta": {}, "finish_reason": "length"}]
        }))
        .expect("serialize chat length response")
    )
}

fn messages_sse_text_and_stop_with_tokens(
    response_id: &str,
    text: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> String {
    let mut body = String::new();
    body.push_str("event: message_start\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "type": "message_start",
            "message": {
                "id": response_id,
                "usage": { "input_tokens": input_tokens },
            },
        }))
        .expect("serialize Messages start event"),
    ));
    body.push_str("event: content_block_delta\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": text,
            },
        }))
        .expect("serialize Messages text delta"),
    ));
    body.push_str("event: message_delta\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": output_tokens },
        }))
        .expect("serialize Messages delta event"),
    ));
    body.push_str("event: message_stop\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "type": "message_stop",
        }))
        .expect("serialize Messages stop event"),
    ));
    body
}

struct ChatSeqResponder {
    num_calls: AtomicUsize,
    bodies: Vec<String>,
}

impl wiremock::Respond for ChatSeqResponder {
    fn respond(&self, _: &wiremock::Request) -> wiremock::ResponseTemplate {
        let idx = self.num_calls.fetch_add(1, Ordering::SeqCst);
        match self.bodies.get(idx) {
            Some(body) => ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body.clone()),
            None => panic!("no chat completion response for index {idx}"),
        }
    }
}

struct MessagesSeqResponder {
    num_calls: AtomicUsize,
    bodies: Vec<String>,
}

impl wiremock::Respond for MessagesSeqResponder {
    fn respond(&self, _: &wiremock::Request) -> wiremock::ResponseTemplate {
        let idx = self.num_calls.fetch_add(1, Ordering::SeqCst);
        match self.bodies.get(idx) {
            Some(body) => ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body.clone()),
            None => panic!("no Messages response for index {idx}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summarize_context_three_requests_and_instructions() {
    skip_if_no_network!();

    // Set up a mock server that we can inspect after the run.
    let server = start_mock_server().await;

    // SSE 1: assistant replies normally so it is recorded in history.
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);

    // SSE 2: summarizer returns a summary message.
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);

    // SSE 3: minimal completed; we only need to capture the request body.
    let sse3 = sse(vec![ev_completed("r3")]);

    // Mount the three expected requests in sequence so the assertions below can
    // inspect them without relying on specific prompt markers.
    let request_log = mount_sse_sequence(&server, vec![sse1, sse2, sse3]).await;

    // Build config pointing to the mock server and spawn LHA.
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200_000);
    });
    let test = builder.build(&server).await.unwrap();
    let codex = test.codex.clone();
    let rollout_path = test.session_configured.rollout_path.expect("rollout path");

    // 1) Normal user input – should hit server once.
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello world".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // 2) Summarize – second hit should include the summarization prompt.
    codex.submit(Op::Compact).await.unwrap();
    let warning_event = wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    let EventMsg::Warning(WarningEvent { message }) = warning_event else {
        panic!("expected warning event after compact");
    };
    assert_eq!(message, COMPACT_WARNING_MESSAGE);
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // 3) Next user input – third hit; history should include only the summary.
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Inspect the three captured requests.
    let requests = request_log.requests();
    assert_eq!(requests.len(), 3, "expected exactly three requests");
    let body1 = requests[0].body_json();
    let body2 = requests[1].body_json();
    let body3 = requests[2].body_json();

    // Manual compact should keep the baseline developer instructions.
    let instr1 = body1.get("instructions").and_then(|v| v.as_str()).unwrap();
    let instr2 = body2.get("instructions").and_then(|v| v.as_str()).unwrap();
    assert_eq!(
        instr1, instr2,
        "manual compact should keep the standard developer instructions"
    );

    // The summarization request should include the injected user input marker.
    let body2_str = body2.to_string();
    let input2 = body2.get("input").and_then(|v| v.as_array()).unwrap();
    let has_compact_prompt = body_contains_text(&body2_str, SUMMARIZATION_PROMPT);
    assert!(
        has_compact_prompt,
        "compaction request should include the summarize trigger"
    );
    // The last item is the user message created from the injected input.
    let last2 = input2.last().unwrap();
    assert_eq!(last2.get("type").unwrap().as_str().unwrap(), "message");
    assert_eq!(last2.get("role").unwrap().as_str().unwrap(), "user");
    let text2 = last2["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text2, SUMMARIZATION_PROMPT,
        "expected summarize trigger, got `{text2}`"
    );

    // Third request must contain the refreshed instructions, compacted user history, and new user message.
    let input3 = body3.get("input").and_then(|v| v.as_array()).unwrap();

    assert!(
        input3.len() >= 3,
        "expected refreshed context and new user message in third request"
    );

    let mut messages: Vec<(String, String)> = Vec::new();
    let expected_summary_message = summary_with_prefix(SUMMARY_TEXT);

    for item in input3 {
        if let Some("message") = item.get("type").and_then(|v| v.as_str()) {
            let role = item
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let text = item
                .get("content")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|entry| entry.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            messages.push((role, text));
        }
    }

    // No previous assistant messages should remain and the new user message is present.
    let assistant_count = messages.iter().filter(|(r, _)| r == "assistant").count();
    assert_eq!(assistant_count, 0, "assistant history should be cleared");
    assert!(
        messages
            .iter()
            .any(|(r, t)| r == "user" && t == THIRD_USER_MSG),
        "third request should include the new user message"
    );
    assert!(
        messages
            .iter()
            .any(|(r, t)| r == "user" && t == "hello world"),
        "third request should include the original user message"
    );
    assert!(
        messages
            .iter()
            .any(|(r, t)| r == "user" && t == &expected_summary_message),
        "third request should include the summary message"
    );
    assert!(
        !messages
            .iter()
            .any(|(_, text)| text.contains(SUMMARIZATION_PROMPT)),
        "third request should not include the summarize trigger"
    );

    // Shut down LHA to flush rollout entries before inspecting the file.
    codex.submit(Op::Shutdown).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    // Verify rollout contains APITurn entries for each API call and a Compacted entry.
    println!("rollout path: {}", rollout_path.display());
    let text = std::fs::read_to_string(&rollout_path).unwrap_or_else(|e| {
        panic!(
            "failed to read rollout file {}: {e}",
            rollout_path.display()
        )
    });
    let mut api_turn_count = 0usize;
    let mut saw_compacted_summary = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry): Result<RolloutLine, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        match entry.item {
            RolloutItem::TurnContext(_) => {
                api_turn_count += 1;
            }
            RolloutItem::Compacted(ci) => {
                if ci.message == expected_summary_message {
                    saw_compacted_summary = true;
                }
            }
            _ => {}
        }
    }

    assert!(
        api_turn_count == 3,
        "expected three APITurn entries in rollout"
    );
    assert!(
        saw_compacted_summary,
        "expected a Compacted entry containing the summarizer output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_backfills_latest_plan_as_assistant_context_without_active_goal() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let plan_block = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>\n";
    let full_plan_message = format!("Intro\n{plan_block}Outro");

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", &full_plan_message),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", FINAL_REPLY),
                ev_completed("r3"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap();
    let codex = test.codex;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: test.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir().expect("current dir"),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 3);

    let follow_up_input = requests[2]
        .body_json()
        .get("input")
        .and_then(|value| value.as_array())
        .cloned()
        .expect("follow-up input should be an array");
    let expected_plan = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>";
    assert!(
        contains_assistant_text(&follow_up_input, expected_plan),
        "expected compacted follow-up history to include backfilled proposed plan"
    );
    assert!(
        contains_user_text(
            &follow_up_input,
            "A proposed plan from before compaction is preserved below."
        ),
        "expected compacted follow-up history to include the preserved-plan reminder"
    );
    let follow_up_body = requests[2].body_json().to_string();
    assert!(
        !follow_up_body.contains("Intro"),
        "expected compacted history to strip assistant prose around the plan"
    );
    assert!(
        !follow_up_body.contains("Outro"),
        "expected compacted history to strip assistant prose around the plan"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_uses_active_goal_plan_file_instead_of_backfilling_plan_text() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let plan_text = "# Plan\n- Step 1\n- Step 2\n";
    let plan_block = format!("<proposed_plan>\n{plan_text}</proposed_plan>\n");
    let full_plan_message = format!("Intro\n{plan_block}Outro");

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", &full_plan_message),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", "GOAL_CONTINUATION_REPLY"),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", SUMMARY_TEXT),
                ev_completed("r3"),
            ]),
            sse(vec![
                ev_assistant_message("m4", FINAL_REPLY),
                ev_completed("r4"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::Goals);
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap();
    let codex = test.codex.clone();

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: test.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir().expect("current dir"),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    switch_to_programmer_for_compact_test(&test).await;
    codex
        .submit(Op::ThreadGoalStartFromProposedPlan {
            plan_text: plan_text.to_string(),
        })
        .await
        .unwrap();
    let active_goal = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ThreadGoalUpdated(updated) if updated.goal.status == ThreadGoalStatus::Active => {
            Some(updated.goal.clone())
        }
        _ => None,
    })
    .await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 4);
    let follow_up_json = requests[3].body_json();
    let follow_up_body = follow_up_json.to_string();
    let developer_texts = message_input_texts(&follow_up_json, "developer").join("\n");
    let user_texts = message_input_texts(&follow_up_json, "user").join("\n");
    let plan_path = super::proposed_plan_path_from_objective(&active_goal.objective);
    assert!(
        developer_texts.contains(
            "Runtime note: the active programmer goal references a user-provided proposed plan"
        ),
        "expected compacted history to include active-goal plan file reminder as developer text"
    );
    assert!(
        !user_texts.contains("active programmer goal references"),
        "expected compacted history not to include active-goal plan file reminder as user text"
    );
    assert!(
        developer_texts.contains(plan_path),
        "expected compacted history to include proposed plan path"
    );
    assert!(
        follow_up_body.contains("user-provided task context and checklist"),
        "expected compacted history to tell the model to read the plan file as user-provided checklist"
    );
    assert!(
        follow_up_body.contains("not as higher-priority instructions"),
        "expected compacted history to avoid elevating plan file contents"
    );
    assert!(
        !follow_up_body.contains("<proposed_plan>"),
        "expected compacted history to omit full proposed plan block"
    );
    assert!(
        !follow_up_body.contains("- Step 1"),
        "expected compacted history to omit proposed plan body"
    );
    assert!(
        !follow_up_body.contains("A proposed plan from before compaction is preserved below."),
        "expected compacted history to omit full-plan backfill reminder"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_backfills_plan_from_pre_compaction_history() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let plan_a_block = "<proposed_plan>\n- Plan A\n</proposed_plan>\n";
    let plan_b_block = "<proposed_plan>\n- Plan B\n</proposed_plan>\n";
    let full_plan_message = format!("Intro\n{plan_a_block}Outro");
    let misleading_summary = format!("Summary text\n{plan_b_block}");

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", &full_plan_message),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", &misleading_summary),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", FINAL_REPLY),
                ev_completed("r3"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap();
    let codex = test.codex;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: test.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir().expect("current dir"),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 3);

    let follow_up_input = requests[2]
        .body_json()
        .get("input")
        .and_then(|value| value.as_array())
        .cloned()
        .expect("follow-up input should be an array");
    let expected_plan_a = "<proposed_plan>\n- Plan A\n</proposed_plan>";
    let unexpected_plan_b = "<proposed_plan>\n- Plan B\n</proposed_plan>";

    assert!(
        contains_assistant_text(&follow_up_input, expected_plan_a),
        "expected compacted follow-up history to backfill Plan A from pre-compaction history"
    );
    assert!(
        !contains_assistant_text(&follow_up_input, unexpected_plan_b),
        "expected compacted follow-up history to ignore plan blocks that only appear in the compact summary"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_persists_replacement_history_in_rollout() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let plan_block = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>\n";
    let full_plan_message = format!("Intro\n{plan_block}Outro");

    mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", &full_plan_message),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed("r2"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap();
    let codex = test.codex.clone();
    let rollout_path = test.session_configured.rollout_path.expect("rollout path");

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: test.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir().expect("current dir"),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Shutdown).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let rollout_text = std::fs::read_to_string(&rollout_path).unwrap_or_else(|e| {
        panic!(
            "failed to read rollout file {}: {e}",
            rollout_path.display()
        )
    });
    let expected_summary_message = summary_with_prefix(SUMMARY_TEXT);
    let expected_plan = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>";
    let mut saw_persisted_replacement_history = false;

    for line in rollout_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(entry) = serde_json::from_str::<RolloutLine>(line) else {
            continue;
        };
        if let RolloutItem::Compacted(compacted) = entry.item
            && compacted.message == expected_summary_message
            && compacted
                .replacement_history
                .as_ref()
                .is_some_and(|history| {
                    history.iter().any(|item| {
                        matches!(
                            item,
                            TranscriptItem::Message { role, content, .. }
                                if role == "assistant"
                                    && content.iter().any(|content_item| {
                                        matches!(
                                            content_item,
                                            ContentItem::OutputText { text }
                                                if text == expected_plan
                                        )
                                    })
                        )
                    })
                })
        {
            saw_persisted_replacement_history = true;
            break;
        }
    }

    assert!(
        saw_persisted_replacement_history,
        "expected rollout to persist local compaction replacement history with the backfilled plan"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_backfills_latest_unfinished_update_plan() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let call_id = "plan-call-1";
    let update_plan_args = serde_json::json!({
        "explanation": "Keep moving",
        "plan": [
            { "step": "Inspect compact flow", "status": "completed" },
            { "step": "Backfill checklist", "status": "in_progress" }
        ]
    })
    .to_string();

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call(call_id, "update_plan", &update_plan_args),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", FINAL_REPLY),
                ev_completed("r3"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let codex = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap()
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "please track progress".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 3);

    let follow_up_input = requests[2]
        .body_json()
        .get("input")
        .and_then(|value| value.as_array())
        .cloned()
        .expect("follow-up input should be an array");

    assert!(
        contains_function_call(&follow_up_input, "update_plan", &update_plan_args),
        "expected compacted follow-up history to include a backfilled update_plan call"
    );
    assert!(
        contains_function_call_output(
            &follow_up_input,
            "compact_backfill_update_plan",
            "Plan updated",
        ),
        "expected compacted follow-up history to include a matching update_plan output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_backfills_recent_skills_into_follow_up_history() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let skill_body = "follow the demo skill";
    let model_provider = non_openai_model_provider(&server);
    let test = test_codex()
        .with_pre_build_hook(|home| {
            write_skill(home, "demo", "demo skill", skill_body);
        })
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap();
    let codex = test.codex.clone();
    let skill_path = std::fs::canonicalize(test.lha_home_path().join("skills/demo/SKILL.md"))
        .expect("canonicalize skill path");

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", FIRST_REPLY),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", "SECOND_SUMMARY"),
                ev_completed("r3"),
            ]),
            sse(vec![
                ev_assistant_message("m4", FINAL_REPLY),
                ev_completed("r4"),
            ]),
        ],
    )
    .await;

    codex
        .submit(Op::UserTurn {
            items: vec![
                UserInput::Text {
                    text: "please use $demo".to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Skill {
                    name: "demo".to_string(),
                    path: skill_path.clone(),
                },
            ],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    let second_compact_body = requests[2].body_json().to_string();
    let follow_up_body = requests[3].body_json().to_string();
    let skill_path_text = skill_path.to_string_lossy();
    let synthetic_skill_prefix = "<skill source=\\\"compact_backfill\\\">\\n<name>demo</name>";

    assert!(
        !second_compact_body.contains(synthetic_skill_prefix),
        "expected second compact prompt to exclude synthetic skill backfill"
    );
    assert!(
        !second_compact_body.contains(skill_path_text.as_ref()),
        "expected second compact prompt to exclude synthetic skill path"
    );
    assert!(
        !second_compact_body.contains(skill_body),
        "expected second compact prompt to exclude synthetic skill contents"
    );

    assert!(
        follow_up_body.contains(synthetic_skill_prefix),
        "expected follow-up request to preserve synthetic skill backfill"
    );
    assert!(
        follow_up_body.contains(skill_path_text.as_ref()),
        "expected follow-up request to retain synthetic skill path"
    );
    assert!(
        follow_up_body.contains(skill_body),
        "expected follow-up request to retain synthetic skill contents"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_does_not_backfill_completed_update_plan() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let call_id = "plan-call-complete";
    let completed_plan_args = serde_json::json!({
        "explanation": "Done",
        "plan": [
            { "step": "Inspect compact flow", "status": "completed" }
        ]
    })
    .to_string();

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call(call_id, "update_plan", &completed_plan_args),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", FINAL_REPLY),
                ev_completed("r3"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let codex = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap()
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "finish the checklist".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    let follow_up_body = requests[2].body_json().to_string();
    assert!(
        !follow_up_body.contains("\"name\":\"update_plan\""),
        "expected compacted follow-up history to omit fully completed update_plan state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_does_not_revive_older_unfinished_update_plan_after_completion() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let unfinished_plan_args = serde_json::json!({
        "explanation": "Keep moving",
        "plan": [
            { "step": "Inspect compact flow", "status": "completed" },
            { "step": "Backfill checklist", "status": "in_progress" }
        ]
    })
    .to_string();
    let completed_plan_args = serde_json::json!({
        "explanation": "Done",
        "plan": [
            { "step": "Inspect compact flow", "status": "completed" },
            { "step": "Backfill checklist", "status": "completed" }
        ]
    })
    .to_string();

    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call("plan-call-1", "update_plan", &unfinished_plan_args),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_function_call("plan-call-2", "update_plan", &completed_plan_args),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m3", SUMMARY_TEXT),
                ev_completed("r3"),
            ]),
            sse(vec![
                ev_assistant_message("m4", FINAL_REPLY),
                ev_completed("r4"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let codex = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap()
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "please track progress".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "finish the checklist".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: THIRD_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    let follow_up_body = requests[3].body_json().to_string();
    assert!(
        !follow_up_body.contains("\"name\":\"update_plan\""),
        "expected compacted follow-up history to omit stale unfinished checklist after a later completed update_plan"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compact_persists_backfilled_update_plan_in_rollout() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let call_id = "plan-rollout-call";
    let update_plan_args = UpdatePlanArgs {
        explanation: Some("Keep moving".to_string()),
        plan: vec![
            PlanItemArg {
                step: "Inspect compact flow".to_string(),
                status: StepStatus::Completed,
            },
            PlanItemArg {
                step: "Backfill checklist".to_string(),
                status: StepStatus::InProgress,
            },
        ],
    };
    let update_plan_args_json =
        serde_json::to_string(&update_plan_args).expect("serialize update_plan args");

    mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call(call_id, "update_plan", &update_plan_args_json),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", SUMMARY_TEXT),
                ev_completed("r2"),
            ]),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .unwrap();
    let codex = test.codex.clone();
    let rollout_path = test.session_configured.rollout_path.expect("rollout path");

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "track work".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Shutdown).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let rollout_text = std::fs::read_to_string(&rollout_path).unwrap_or_else(|e| {
        panic!(
            "failed to read rollout file {}: {e}",
            rollout_path.display()
        )
    });
    let expected_summary_message = summary_with_prefix(SUMMARY_TEXT);
    let mut saw_persisted_replacement_history = false;

    for line in rollout_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(entry) = serde_json::from_str::<RolloutLine>(line) else {
            continue;
        };
        if let RolloutItem::Compacted(compacted) = entry.item
            && compacted.message == expected_summary_message
            && compacted
                .replacement_history
                .as_ref()
                .is_some_and(|history| {
                    history.iter().any(|item| {
                        matches!(
                            item,
                            TranscriptItem::ToolCall {
                                tool_name,
                                payload: ToolCallPayload::JsonArguments { arguments },
                                ..
                            } if tool_name == "update_plan" && arguments == &update_plan_args_json
                        )
                    }) && history.iter().any(|item| {
                        matches!(
                            item,
                            TranscriptItem::ToolResult {
                                call_id,
                                payload: ToolResultPayload::Structured { content, .. },
                                ..
                            } if call_id == "compact_backfill_update_plan" && content == "Plan updated"
                        )
                    })
                })
        {
            saw_persisted_replacement_history = true;
            break;
        }
    }

    assert!(
        saw_persisted_replacement_history,
        "expected rollout to persist local compaction replacement history with the backfilled update_plan"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_uses_custom_prompt() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse_stream = sse(vec![ev_completed("r1")]);
    let response_mock = mount_sse_once(&server, sse_stream).await;

    let custom_prompt = "Use this compact prompt instead";

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        config.compact_prompt = Some(custom_prompt.to_string());
    });
    let codex = builder
        .build(&server)
        .await
        .expect("create conversation")
        .codex;

    codex.submit(Op::Compact).await.expect("trigger compact");
    let warning_event = wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    let EventMsg::Warning(WarningEvent { message }) = warning_event else {
        panic!("expected warning event after compact");
    };
    assert_eq!(message, COMPACT_WARNING_MESSAGE);
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let body = response_mock.single_request().body_json();

    let input = body
        .get("input")
        .and_then(|v| v.as_array())
        .expect("input array");
    let mut found_custom_prompt = false;
    let mut found_default_prompt = false;

    for item in input {
        if item["type"].as_str() != Some("message") {
            continue;
        }
        let text = item["content"][0]["text"].as_str().unwrap_or_default();
        if text == custom_prompt {
            found_custom_prompt = true;
        }
        if text == SUMMARIZATION_PROMPT {
            found_default_prompt = true;
        }
    }

    let used_prompt = found_custom_prompt || found_default_prompt;
    if used_prompt {
        assert!(found_custom_prompt, "custom prompt should be injected");
        assert!(
            !found_default_prompt,
            "default prompt should be replaced when a compact prompt is used"
        );
    } else {
        assert!(
            !found_default_prompt,
            "summarization prompt should not appear if compaction omits a prompt"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_emits_api_and_local_token_usage_events() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    // Compact run where the API reports zero tokens in usage. Our local
    // estimator should still compute a non-zero context size for the compacted
    // history.
    let sse_compact = sse(vec![
        ev_assistant_message("m1", SUMMARY_TEXT),
        ev_completed_with_tokens("r1", 0),
    ]);
    mount_sse_once(&server, sse_compact).await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    // Trigger manual compact and collect TokenCount events for the compact turn.
    codex.submit(Op::Compact).await.unwrap();

    // First TokenCount: from the compact API call (usage.total_tokens = 0).
    let first = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::TokenCount(tc) => tc
            .info
            .as_ref()
            .map(|info| info.last_token_usage.total_tokens),
        _ => None,
    })
    .await;

    // Second TokenCount: from the local post-compaction estimate.
    let last = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::TokenCount(tc) => tc
            .info
            .as_ref()
            .map(|info| info.last_token_usage.total_tokens),
        _ => None,
    })
    .await;

    // Ensure the compact task itself completes.
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    assert_eq!(
        first, 0,
        "expected first TokenCount from compact API usage to be zero"
    );
    assert!(
        last > 0,
        "second TokenCount should reflect a non-zero estimated context size after compaction"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_emits_context_compaction_items() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);
    mount_sse_sequence(&server, vec![sse1, sse2]).await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "manual compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();

    let mut started_item = None;
    let mut completed_item = None;
    let mut legacy_event = false;
    let mut saw_turn_complete = false;

    while !saw_turn_complete || started_item.is_none() || completed_item.is_none() || !legacy_event
    {
        let event = codex.next_event().await.unwrap();
        match event.msg {
            EventMsg::ItemStarted(ItemStartedEvent {
                item: TurnItem::ContextCompaction(item),
                ..
            }) => {
                started_item = Some(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::ContextCompaction(item),
                ..
            }) => {
                completed_item = Some(item);
            }
            EventMsg::ContextCompacted(_) => {
                legacy_event = true;
            }
            EventMsg::TurnComplete(_) => {
                saw_turn_complete = true;
            }
            _ => {}
        }
    }

    let started_item = started_item.expect("context compaction item started");
    let completed_item = completed_item.expect("context compaction item completed");
    assert_eq!(started_item.id, completed_item.id);
    assert!(legacy_event);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_auto_compact_per_task_runs_after_token_limit_hit() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let non_openai_provider_name = non_openai_model_provider(&server).name;
    let codex = test_codex()
        .with_config(move |config| {
            config.model_provider.name = non_openai_provider_name;
        })
        .build(&server)
        .await
        .expect("build codex")
        .codex;

    // user message
    let user_message = "create an app";

    // Prepare the mock responses from the model

    // summary texts from model
    let first_summary_text = "The task is to create an app. I started to create a react app.";
    let second_summary_text = "The task is to create an app. I started to create a react app. then I realized that I need to create a node app.";
    let third_summary_text = "The task is to create an app. I started to create a react app. then I realized that I need to create a node app. then I realized that I need to create a python app.";
    // summary texts with prefix
    let prefixed_first_summary = summary_with_prefix(first_summary_text);
    let prefixed_second_summary = summary_with_prefix(second_summary_text);
    let prefixed_third_summary = summary_with_prefix(third_summary_text);
    // token used count after long work
    let token_count_used = 270_000;
    // token used count after compaction
    let token_count_used_after_compaction = 80000;

    // mock responses from the model

    let reasoning_response_1 = ev_reasoning_item("m1", &["I will create a react app"], &[]);
    let encrypted_content_1 = reasoning_response_1["item"]["encrypted_content"]
        .as_str()
        .unwrap();

    // first chunk of work
    let model_reasoning_response_1_sse = sse(vec![
        reasoning_response_1.clone(),
        ev_local_shell_call("r1-shell", "completed", vec!["echo", "make-react"]),
        ev_completed_with_tokens("r1", token_count_used),
    ]);

    // first compaction response
    let model_compact_response_1_sse = sse(vec![
        ev_assistant_message("m2", first_summary_text),
        ev_completed_with_tokens("r2", token_count_used_after_compaction),
    ]);

    let reasoning_response_2 = ev_reasoning_item("m3", &["I will create a node app"], &[]);
    let encrypted_content_2 = reasoning_response_2["item"]["encrypted_content"]
        .as_str()
        .unwrap();

    // second chunk of work
    let model_reasoning_response_2_sse = sse(vec![
        reasoning_response_2.clone(),
        ev_local_shell_call("r3-shell", "completed", vec!["echo", "make-node"]),
        ev_completed_with_tokens("r3", token_count_used),
    ]);

    // second compaction response
    let model_compact_response_2_sse = sse(vec![
        ev_assistant_message("m4", second_summary_text),
        ev_completed_with_tokens("r4", token_count_used_after_compaction),
    ]);

    let reasoning_response_3 = ev_reasoning_item("m6", &["I will create a python app"], &[]);
    let encrypted_content_3 = reasoning_response_3["item"]["encrypted_content"]
        .as_str()
        .unwrap();

    // third chunk of work
    let model_reasoning_response_3_sse = sse(vec![
        ev_reasoning_item("m6", &["I will create a python app"], &[]),
        ev_local_shell_call("r6-shell", "completed", vec!["echo", "make-python"]),
        ev_completed_with_tokens("r6", token_count_used),
    ]);

    // third compaction response
    let model_compact_response_3_sse = sse(vec![
        ev_assistant_message("m7", third_summary_text),
        ev_completed_with_tokens("r7", token_count_used_after_compaction),
    ]);

    // final response
    let model_final_response_sse = sse(vec![
        ev_assistant_message(
            "m8",
            "The task is to create an app. I started to create a react app. then I realized that I need to create a node app. then I realized that I need to create a python app.",
        ),
        ev_completed_with_tokens("r8", token_count_used_after_compaction + 1000),
    ]);

    // mount the mock responses from the model
    let bodies = vec![
        model_reasoning_response_1_sse,
        model_compact_response_1_sse,
        model_reasoning_response_2_sse,
        model_compact_response_2_sse,
        model_reasoning_response_3_sse,
        model_compact_response_3_sse,
        model_final_response_sse,
    ];
    let request_log = mount_sse_sequence(&server, bodies).await;

    // Start the conversation with the user message
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: user_message.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit user input");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // collect the requests payloads from the model
    let requests_payloads = request_log.requests();
    let body = requests_payloads[0].body_json();
    let input = body.get("input").and_then(|v| v.as_array()).unwrap();

    fn normalize_inputs(values: &[serde_json::Value]) -> Vec<serde_json::Value> {
        values
            .iter()
            .filter(|value| {
                if value
                    .get("type")
                    .and_then(|ty| ty.as_str())
                    .is_some_and(|ty| ty == "function_call_output")
                {
                    return false;
                }

                let text = value
                    .get("content")
                    .and_then(|content| content.as_array())
                    .and_then(|content| content.first())
                    .and_then(|item| item.get("text"))
                    .and_then(|text| text.as_str());

                // Ignore cached prefix messages (project docs + permissions) since they are not
                // relevant to compaction behavior and can change as bundled prompts evolve.
                let role = value.get("role").and_then(|role| role.as_str());
                if role == Some("developer")
                    && text.is_some_and(|text| text.contains("`sandbox_mode`"))
                {
                    return false;
                }
                !text.is_some_and(|text| text.starts_with("# AGENTS.md instructions for "))
            })
            .cloned()
            .collect()
    }

    let initial_input = normalize_inputs(input);
    let environment_message = initial_input[0]["content"][0]["text"].as_str().unwrap();

    // test 1: after compaction, we should have one environment message, one user message, and one user message with summary prefix
    let compaction_indices = [2, 4, 6];
    let expected_summaries = [
        prefixed_first_summary.as_str(),
        prefixed_second_summary.as_str(),
        prefixed_third_summary.as_str(),
    ];
    for (i, expected_summary) in compaction_indices.into_iter().zip(expected_summaries) {
        let body = requests_payloads.clone()[i].body_json();
        let input = body.get("input").and_then(|v| v.as_array()).unwrap();
        let input = normalize_inputs(input);
        assert_eq!(input.len(), 3);
        let environment_message = input[0]["content"][0]["text"].as_str().unwrap();
        let user_message_received = input[1]["content"][0]["text"].as_str().unwrap();
        let summary_message = input[2]["content"][0]["text"].as_str().unwrap();
        assert_eq!(environment_message, environment_message);
        assert_eq!(user_message_received, user_message);
        assert_eq!(
            summary_message, expected_summary,
            "compaction request at index {i} should include the prefixed summary"
        );
    }

    // test 2: the expected requests inputs should be as follows:
    let expected_requests_inputs = json!([
    [
        // 0: first request of the user message.
      {
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ,
    [
        // 1: first automatic compaction request.
      {
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": null,
        "encrypted_content": encrypted_content_1,
        "summary": [
          {
            "text": "I will create a react app",
            "type": "summary_text"
          }
        ],
        "type": "reasoning"
      },
      {
        "action": {
          "command": [
            "echo",
            "make-react"
          ],
          "env": null,
          "timeout_ms": null,
          "type": "exec",
          "user": null,
          "working_directory": null
        },
        "call_id": "r1-shell",
        "status": "completed",
        "type": "local_shell_call"
      },
      {
        "call_id": "r1-shell",
        "output": "execution error: Io(Os { code: 2, kind: NotFound, message: \"No such file or directory\" })",
        "type": "function_call_output"
      },
      {
        "content": [
          {
            "text": SUMMARIZATION_PROMPT,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ,
    [
      // 2: request after first automatic compaction.
      {
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": prefixed_first_summary.clone(),
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ,
    [
        // 3: request for second automatic compaction.
      {
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": prefixed_first_summary.clone(),
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": null,
        "encrypted_content": encrypted_content_2,
        "summary": [
          {
            "text": "I will create a node app",
            "type": "summary_text"
          }
        ],
        "type": "reasoning"
      },
      {
        "action": {
          "command": [
            "echo",
            "make-node"
          ],
          "env": null,
          "timeout_ms": null,
          "type": "exec",
          "user": null,
          "working_directory": null
        },
        "call_id": "r3-shell",
        "status": "completed",
        "type": "local_shell_call"
      },
      {
        "call_id": "r3-shell",
        "output": "execution error: Io(Os { code: 2, kind: NotFound, message: \"No such file or directory\" })",
        "type": "function_call_output"
      },
      {
        "content": [
          {
            "text": SUMMARIZATION_PROMPT,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ,
    // 4: request after second automatic compaction.
    [
      {
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": prefixed_second_summary.clone(),
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ,
    [
      // 5: request for third automatic compaction.
      {
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": prefixed_second_summary.clone(),
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": null,
        "encrypted_content": encrypted_content_3,
        "summary": [
          {
            "text": "I will create a python app",
            "type": "summary_text"
          }
        ],
        "type": "reasoning"
      },
      {
        "action": {
          "command": [
            "echo",
            "make-python"
          ],
          "env": null,
          "timeout_ms": null,
          "type": "exec",
          "user": null,
          "working_directory": null
        },
        "call_id": "r6-shell",
        "status": "completed",
        "type": "local_shell_call"
      },
      {
        "call_id": "r6-shell",
        "output": "execution error: Io(Os { code: 2, kind: NotFound, message: \"No such file or directory\" })",
        "type": "function_call_output"
      },
      {
        "content": [
          {
            "text": SUMMARIZATION_PROMPT,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ,
    [
      {
        // 6: request after third automatic compaction.
        "content": [
          {
            "text": environment_message,
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": "create an app",
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      },
      {
        "content": [
          {
            "text": prefixed_third_summary.clone(),
            "type": "input_text"
          }
        ],
        "role": "user",
        "type": "message"
      }
    ]
    ]);

    for (i, request) in requests_payloads.iter().enumerate() {
        let body = request.body_json();
        let input = body.get("input").and_then(|v| v.as_array()).unwrap();
        let expected_input = expected_requests_inputs[i].as_array().unwrap();
        assert_eq!(normalize_inputs(input), normalize_inputs(expected_input));
    }

    // test 3: the number of requests should be 7
    assert_eq!(requests_payloads.len(), 7);
}

// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn auto_compact_runs_after_token_limit_hit() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed_with_tokens("r1", 70_000),
    ]);

    let sse2 = sse(vec![
        ev_assistant_message("m2", "SECOND_REPLY"),
        ev_completed_with_tokens("r2", 330_000),
    ]);

    let sse3 = sse(vec![
        ev_assistant_message("m3", AUTO_SUMMARY_TEXT),
        ev_completed_with_tokens("r3", 200),
    ]);
    let sse4 = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r4", 120),
    ]);
    let prefixed_auto_summary = AUTO_SUMMARY_TEXT;

    let request_log = mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse4]).await;

    let model_provider = non_openai_model_provider(&server);

    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200_000);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: FIRST_AUTO_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: SECOND_AUTO_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: POST_AUTO_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    let request_bodies: Vec<String> = requests
        .iter()
        .map(|request| request.body_json().to_string())
        .collect();
    assert_eq!(
        request_bodies.len(),
        4,
        "expected user turns, a compaction request, and the follow-up turn; got {}",
        request_bodies.len()
    );
    let auto_compact_count = request_bodies
        .iter()
        .filter(|body| body_contains_text(body, SUMMARIZATION_PROMPT))
        .count();
    assert_eq!(
        auto_compact_count, 1,
        "expected exactly one auto compact request"
    );
    let auto_compact_index = request_bodies
        .iter()
        .enumerate()
        .find_map(|(idx, body)| body_contains_text(body, SUMMARIZATION_PROMPT).then_some(idx))
        .expect("auto compact request missing");
    assert_eq!(
        auto_compact_index, 2,
        "auto compact should add a third request"
    );

    let follow_up_index = request_bodies
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, body)| {
            (body.contains(POST_AUTO_USER_MSG) && !body_contains_text(body, SUMMARIZATION_PROMPT))
                .then_some(idx)
        })
        .expect("follow-up request missing");
    assert_eq!(follow_up_index, 3, "follow-up request should be last");

    let body_first = requests[0].body_json();
    let body_auto = requests[auto_compact_index].body_json();
    let body_follow_up = requests[follow_up_index].body_json();
    let instructions = body_auto
        .get("instructions")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let baseline_instructions = body_first
        .get("instructions")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        instructions, baseline_instructions,
        "auto compact should keep the standard developer instructions",
    );

    let input_auto = body_auto.get("input").and_then(|v| v.as_array()).unwrap();
    let last_auto = input_auto
        .last()
        .expect("auto compact request should append a user message");
    assert_eq!(
        last_auto.get("type").and_then(|v| v.as_str()),
        Some("message")
    );
    assert_eq!(last_auto.get("role").and_then(|v| v.as_str()), Some("user"));
    let last_text = last_auto
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or_default();
    assert_eq!(
        last_text, SUMMARIZATION_PROMPT,
        "auto compact should send the summarization prompt as a user message",
    );

    let input_follow_up = body_follow_up
        .get("input")
        .and_then(|v| v.as_array())
        .unwrap();
    let user_texts: Vec<String> = input_follow_up
        .iter()
        .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
        .filter(|item| item.get("role").and_then(|v| v.as_str()) == Some("user"))
        .filter_map(|item| {
            item.get("content")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|entry| entry.get("text"))
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
        })
        .collect();
    assert!(
        user_texts.iter().any(|text| text == FIRST_AUTO_MSG),
        "auto compact follow-up request should include the first user message"
    );
    assert!(
        user_texts.iter().any(|text| text == SECOND_AUTO_MSG),
        "auto compact follow-up request should include the second user message"
    );
    assert!(
        user_texts.iter().any(|text| text == POST_AUTO_USER_MSG),
        "auto compact follow-up request should include the new user message"
    );
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains(prefixed_auto_summary)),
        "auto compact follow-up request should include the summary message"
    );
}

// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn auto_compact_emits_context_compaction_items() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed_with_tokens("r1", 70_000),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", "SECOND_REPLY"),
        ev_completed_with_tokens("r2", 330_000),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", AUTO_SUMMARY_TEXT),
        ev_completed_with_tokens("r3", 200),
    ]);
    let sse4 = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r4", 120),
    ]);

    mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse4]).await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200_000);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    let mut started_item = None;
    let mut completed_item = None;
    let mut legacy_event = false;

    for user in [FIRST_AUTO_MSG, SECOND_AUTO_MSG, POST_AUTO_USER_MSG] {
        codex
            .submit(Op::UserInput {
                items: vec![UserInput::Text {
                    text: user.into(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .await
            .unwrap();

        loop {
            let event = codex.next_event().await.unwrap();
            match event.msg {
                EventMsg::ItemStarted(ItemStartedEvent {
                    item: TurnItem::ContextCompaction(item),
                    ..
                }) => {
                    started_item = Some(item);
                }
                EventMsg::ItemCompleted(ItemCompletedEvent {
                    item: TurnItem::ContextCompaction(item),
                    ..
                }) => {
                    completed_item = Some(item);
                }
                EventMsg::ContextCompacted(_) => {
                    legacy_event = true;
                }
                EventMsg::TurnComplete(_) if !event.id.starts_with("auto-compact-") => {
                    break;
                }
                _ => {}
            }
        }
    }

    let started_item = started_item.expect("context compaction item started");
    let completed_item = completed_item.expect("context compaction item completed");
    assert_eq!(started_item.id, completed_item.id);
    assert!(legacy_event);
}

// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn auto_compact_starts_after_turn_started() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed_with_tokens("r1", 70_000),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", "SECOND_REPLY"),
        ev_completed_with_tokens("r2", 330_000),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", AUTO_SUMMARY_TEXT),
        ev_completed_with_tokens("r3", 200),
    ]);
    let sse4 = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r4", 120),
    ]);

    mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse4]).await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200_000);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: FIRST_AUTO_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: SECOND_AUTO_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: POST_AUTO_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    let first = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::TurnStarted(_) => Some("turn"),
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::ContextCompaction(_),
            ..
        }) => Some("compaction"),
        _ => None,
    })
    .await;
    assert_eq!(first, "turn", "compaction started before turn started");

    wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ItemStarted(ItemStartedEvent {
                item: TurnItem::ContextCompaction(_),
                ..
            })
        )
    })
    .await;

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_runs_after_resume_when_token_usage_is_over_limit() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let limit = 200_000;
    let over_limit_tokens = 250_000;
    let remote_summary = "REMOTE_COMPACT_SUMMARY";

    let compacted_history = vec![
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: remote_summary.to_string(),
            }],
            end_turn: None,
        },
        TranscriptItem::Reasoning {
            id: "compact-summary".to_string(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("ENCRYPTED_COMPACTION_SUMMARY".to_string()),
        },
    ];
    let compact_mock =
        mount_compact_json_once(&server, serde_json::json!({ "output": compacted_history })).await;

    let mut builder = test_codex().with_config(move |config| {
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(limit);
        config.features.enable(Feature::RemoteCompaction);
    });
    let initial = builder.build(&server).await.unwrap();
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    // A single over-limit completion should not auto-compact until the next user message.
    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("m1", FIRST_REPLY),
            ev_completed_with_tokens("r1", over_limit_tokens),
        ]),
    )
    .await;
    initial.submit_turn("OVER_LIMIT_TURN").await.unwrap();

    assert!(
        compact_mock.requests().is_empty(),
        "remote compaction should not run before the next user message"
    );

    let mut resume_builder = test_codex().with_config(move |config| {
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(limit);
        config.features.enable(Feature::RemoteCompaction);
    });
    let resumed = resume_builder
        .resume(&server, home, rollout_path)
        .await
        .unwrap();

    let follow_up_user = "AFTER_RESUME_USER";
    let sse_follow_up = sse(vec![
        ev_assistant_message("m2", FINAL_REPLY),
        ev_completed("r2"),
    ]);

    let follow_up_matcher = move |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains(follow_up_user) && body.contains(remote_summary)
    };
    mount_sse_once_match(&server, follow_up_matcher, sse_follow_up).await;

    resumed
        .codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: follow_up_user.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: resumed.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: resumed.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await
        .unwrap();

    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::ContextCompacted(_))
    })
    .await;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let compact_requests = compact_mock.requests();
    assert_eq!(
        compact_requests.len(),
        1,
        "remote compaction should run once after resume"
    );
    assert_eq!(
        compact_requests[0].path(),
        "/v1/responses/compact",
        "remote compaction should hit the compact endpoint"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_persists_rollout_entries() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed_with_tokens("r1", 70_000),
    ]);

    let sse2 = sse(vec![
        ev_assistant_message("m2", "SECOND_REPLY"),
        ev_completed_with_tokens("r2", 330_000),
    ]);

    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let sse3 = sse(vec![
        ev_assistant_message("m3", &auto_summary_payload),
        ev_completed_with_tokens("r3", 200),
    ]);
    let sse4 = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r4", 120),
    ]);

    let first_matcher = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains(FIRST_AUTO_MSG)
            && !body.contains(SECOND_AUTO_MSG)
            && !body_contains_text(body, SUMMARIZATION_PROMPT)
    };
    mount_sse_once_match(&server, first_matcher, sse1).await;

    let second_matcher = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains(SECOND_AUTO_MSG)
            && body.contains(FIRST_AUTO_MSG)
            && !body_contains_text(body, SUMMARIZATION_PROMPT)
    };
    mount_sse_once_match(&server, second_matcher, sse2).await;

    let third_matcher = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body_contains_text(body, SUMMARIZATION_PROMPT)
    };
    mount_sse_once_match(&server, third_matcher, sse3).await;

    let fourth_matcher = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains(POST_AUTO_USER_MSG) && !body_contains_text(body, SUMMARIZATION_PROMPT)
    };
    mount_sse_once_match(&server, fourth_matcher, sse4).await;

    let model_provider = non_openai_model_provider(&server);

    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200_000);
    });
    let test = builder.build(&server).await.unwrap();
    let codex = test.codex.clone();
    let session_configured = test.session_configured;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: FIRST_AUTO_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: SECOND_AUTO_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: POST_AUTO_USER_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Shutdown).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let rollout_path = session_configured.rollout_path.expect("rollout path");
    let text = std::fs::read_to_string(&rollout_path).unwrap_or_else(|e| {
        panic!(
            "failed to read rollout file {}: {e}",
            rollout_path.display()
        )
    });

    let mut turn_context_count = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry): Result<RolloutLine, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        match entry.item {
            RolloutItem::TurnContext(_) => {
                turn_context_count += 1;
            }
            RolloutItem::Compacted(_) => {}
            _ => {}
        }
    }

    assert!(
        turn_context_count >= 2,
        "expected at least two turn context entries, got {turn_context_count}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_retries_after_context_window_error() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let user_turn = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let compact_failed = sse_failed(
        "resp-fail",
        "context_length_exceeded",
        CONTEXT_LIMIT_MESSAGE,
    );
    let compact_succeeds = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);

    let request_log = mount_sse_sequence(
        &server,
        vec![
            user_turn.clone(),
            compact_failed.clone(),
            compact_succeeds.clone(),
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);

    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200_000);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "first turn".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    let EventMsg::BackgroundEvent(event) =
        wait_for_event(&codex, |ev| matches!(ev, EventMsg::BackgroundEvent(_))).await
    else {
        panic!("expected background event after compact retry");
    };
    assert!(
        event.message.contains("Trimmed 1 older thread item"),
        "background event should mention trimmed item count: {}",
        event.message
    );
    let warning_event = wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    let EventMsg::Warning(WarningEvent { message }) = warning_event else {
        panic!("expected warning event after compact retry");
    };
    assert_eq!(message, COMPACT_WARNING_MESSAGE);
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    for (idx, request) in requests.iter().enumerate() {
        eprintln!("first_test_request[{idx}]={}", request.body_json());
    }
    assert_eq!(
        requests.len(),
        3,
        "expected user turn and two compact attempts"
    );

    let compact_attempt = requests[1].body_json();
    let retry_attempt = requests[2].body_json();

    let compact_input = compact_attempt["input"]
        .as_array()
        .unwrap_or_else(|| panic!("compact attempt missing input array: {compact_attempt}"));
    let retry_input = retry_attempt["input"]
        .as_array()
        .unwrap_or_else(|| panic!("retry attempt missing input array: {retry_attempt}"));
    let compact_contains_prompt =
        body_contains_text(&compact_attempt.to_string(), SUMMARIZATION_PROMPT);
    let retry_contains_prompt =
        body_contains_text(&retry_attempt.to_string(), SUMMARIZATION_PROMPT);
    assert_eq!(
        compact_contains_prompt, retry_contains_prompt,
        "compact attempts should consistently include or omit the summarization prompt"
    );
    assert_eq!(
        retry_input.len(),
        compact_input.len().saturating_sub(1),
        "retry should drop exactly one history item (before {} vs after {})",
        compact_input.len(),
        retry_input.len()
    );
    if let (Some(first_before), Some(first_after)) = (compact_input.first(), retry_input.first()) {
        assert_ne!(
            first_before, first_after,
            "retry should drop the oldest conversation item"
        );
    } else {
        panic!("expected non-empty compact inputs");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_twice_preserves_latest_user_messages() {
    skip_if_no_network!();

    let first_user_message = "first manual turn";
    let second_user_message = "second manual turn";
    let final_user_message = "post compact follow-up";
    let first_summary = "FIRST_MANUAL_SUMMARY";
    let second_summary = "SECOND_MANUAL_SUMMARY";
    let expected_second_summary = summary_with_prefix(second_summary);

    let server = start_mock_server().await;

    let first_turn = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let first_compact_summary = auto_summary(first_summary);
    let first_compact = sse(vec![
        ev_assistant_message("m2", &first_compact_summary),
        ev_completed("r2"),
    ]);
    let second_turn = sse(vec![
        ev_assistant_message("m3", SECOND_LARGE_REPLY),
        ev_completed("r3"),
    ]);
    let second_compact_summary = auto_summary(second_summary);
    let second_compact = sse(vec![
        ev_assistant_message("m4", &second_compact_summary),
        ev_completed("r4"),
    ]);
    let final_turn = sse(vec![
        ev_assistant_message("m5", FINAL_REPLY),
        ev_completed("r5"),
    ]);

    let responses_mock = mount_sse_sequence(
        &server,
        vec![
            first_turn,
            first_compact,
            second_turn,
            second_compact,
            final_turn,
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);

    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: first_user_message.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: second_user_message.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await.unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: final_user_message.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = responses_mock.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected exactly 5 requests (user turn, compact, user turn, compact, final turn)"
    );
    let first_turn_input = requests[0].input();
    assert!(
        contains_user_text(&first_turn_input, first_user_message),
        "first turn request missing first user message"
    );
    assert!(
        !contains_user_text(&first_turn_input, SUMMARIZATION_PROMPT),
        "first turn request should not include summarization prompt"
    );

    let first_compact_input = requests[1].input();
    assert!(
        contains_user_text(&first_compact_input, first_user_message),
        "first compact request should include history before compaction"
    );

    let second_turn_input = requests[2].input();
    assert!(
        contains_user_text(&second_turn_input, second_user_message),
        "second turn request missing second user message"
    );
    assert!(
        contains_user_text(&second_turn_input, first_user_message),
        "second turn request should include the compacted user history"
    );

    let second_compact_input = requests[3].input();
    assert!(
        contains_user_text(&second_compact_input, second_user_message),
        "second compact request should include latest history"
    );

    let first_compact_has_prompt = contains_user_text(&first_compact_input, SUMMARIZATION_PROMPT);
    let second_compact_has_prompt = contains_user_text(&second_compact_input, SUMMARIZATION_PROMPT);
    assert_eq!(
        first_compact_has_prompt, second_compact_has_prompt,
        "compact requests should consistently include or omit the summarization prompt"
    );

    let mut final_output = requests
        .last()
        .unwrap_or_else(|| panic!("final turn request missing for {final_user_message}"))
        .input()
        .into_iter()
        .collect::<VecDeque<_>>();

    // Permissions developer message
    final_output.pop_front();
    // User instructions (project docs/skills)
    final_output.pop_front();
    // Environment context
    final_output.pop_front();

    let _ = final_output
        .iter_mut()
        .map(drop_call_id)
        .collect::<Vec<_>>();

    let expected = vec![
        json!({
            "content": vec![json!({
                "text": first_user_message,
                "type": "input_text",
            })],
            "role": "user",
            "type": "message",
        }),
        json!({
            "content": vec![json!({
                "text": second_user_message,
                "type": "input_text",
            })],
            "role": "user",
            "type": "message",
        }),
        json!({
            "content": vec![json!({
                "text": expected_second_summary,
                "type": "input_text",
            })],
            "role": "user",
            "type": "message",
        }),
        json!({
            "content": vec![json!({
                "text": final_user_message,
                "type": "input_text",
            })],
            "role": "user",
            "type": "message",
        }),
    ];
    assert_eq!(final_output, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_allows_multiple_attempts_when_interleaved_with_other_turn_events() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed_with_tokens("r1", 500),
    ]);
    let first_summary_payload = auto_summary(FIRST_AUTO_SUMMARY);
    let sse2 = sse(vec![
        ev_assistant_message("m2", &first_summary_payload),
        ev_completed_with_tokens("r2", 50),
    ]);
    let sse3 = sse(vec![
        ev_function_call(DUMMY_CALL_ID, DUMMY_FUNCTION_NAME, "{}"),
        ev_completed_with_tokens("r3", 150),
    ]);
    let sse4 = sse(vec![
        ev_assistant_message("m4", SECOND_LARGE_REPLY),
        ev_completed_with_tokens("r4", 450),
    ]);
    let second_summary_payload = auto_summary(SECOND_AUTO_SUMMARY);
    let sse5 = sse(vec![
        ev_assistant_message("m5", &second_summary_payload),
        ev_completed_with_tokens("r5", 60),
    ]);
    let sse6 = sse(vec![
        ev_assistant_message("m6", FINAL_REPLY),
        ev_completed_with_tokens("r6", 120),
    ]);
    let follow_up_user = "FOLLOW_UP_AUTO_COMPACT";
    let final_user = "FINAL_AUTO_COMPACT";

    let request_log = mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse4, sse5, sse6]).await;

    let model_provider = non_openai_model_provider(&server);

    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_auto_compact_token_limit = Some(200);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    let mut auto_compact_lifecycle_events = Vec::new();
    for user in [MULTI_AUTO_MSG, follow_up_user, final_user] {
        codex
            .submit(Op::UserInput {
                items: vec![UserInput::Text {
                    text: user.into(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .await
            .unwrap();

        loop {
            let event = codex.next_event().await.unwrap();
            if event.id.starts_with("auto-compact-")
                && matches!(
                    event.msg,
                    EventMsg::TurnStarted(_) | EventMsg::TurnComplete(_)
                )
            {
                auto_compact_lifecycle_events.push(event);
                continue;
            }
            if let EventMsg::TurnComplete(_) = &event.msg
                && !event.id.starts_with("auto-compact-")
            {
                break;
            }
        }
    }

    assert!(
        auto_compact_lifecycle_events.is_empty(),
        "auto compact should not emit task lifecycle events"
    );

    let request_bodies: Vec<String> = request_log
        .requests()
        .into_iter()
        .map(|request| request.body_json().to_string())
        .collect();
    assert_eq!(
        request_bodies.len(),
        6,
        "expected six requests including two auto compactions"
    );
    assert!(
        request_bodies[0].contains(MULTI_AUTO_MSG),
        "first request should contain the user input"
    );
    assert!(
        body_contains_text(&request_bodies[1], SUMMARIZATION_PROMPT),
        "first auto compact request should include the summarization prompt"
    );
    assert!(
        request_bodies[3].contains(&format!("unsupported call: {DUMMY_FUNCTION_NAME}")),
        "function call output should be sent before the second auto compact"
    );
    assert!(
        body_contains_text(&request_bodies[4], SUMMARIZATION_PROMPT),
        "second auto compact request should include the summarization prompt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_triggers_after_function_call_over_95_percent_usage() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let context_window = 20_000;
    let limit = 1;
    let follow_up_user = "FOLLOW_UP_AFTER_LIMIT";

    let first_turn = sse(vec![
        ev_function_call(DUMMY_CALL_ID, DUMMY_FUNCTION_NAME, "{}"),
        ev_completed_with_tokens("r1", 50),
    ]);
    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let auto_compact_turn = sse(vec![
        ev_assistant_message("m2", &auto_summary_payload),
        ev_completed_with_tokens("r2", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r3", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![first_turn, auto_compact_turn, post_compact_follow_up],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);

    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_context_window = Some(context_window);
        config.model_auto_compact_token_limit = Some(limit);
    });
    let codex = builder.build(&server).await.unwrap().codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: FUNCTION_CALL_LIMIT_MSG.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: follow_up_user.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;

    // Assert first request captured expected user message that triggers function call.
    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        3,
        "expected request, same-turn auto compact, and post-compact follow-up"
    );

    let first_request = requests[0].input();
    assert!(
        first_request.iter().any(|item| {
            item.get("type").and_then(|value| value.as_str()) == Some("message")
                && item
                    .get("content")
                    .and_then(|content| content.as_array())
                    .and_then(|entries| entries.first())
                    .and_then(|entry| entry.get("text"))
                    .and_then(|value| value.as_str())
                    == Some(FUNCTION_CALL_LIMIT_MSG)
        }),
        "first request should include the user message that triggers the function call"
    );

    let compact_body = requests[1].body_json().to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "auto compact request should include the summarization prompt after exceeding the auto-compact limit ({limit})"
    );

    let post_compact_body = requests[2].body_json().to_string();
    assert!(
        body_contains_text(
            &post_compact_body,
            &summary_with_prefix(&auto_summary_payload)
        ),
        "post-compact follow-up should use compacted history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_triggers_when_tool_output_pushes_next_step_over_limit() {
    skip_if_no_network!();
    if !ripgrep_available() {
        eprintln!("rg not available in PATH; skipping test");
        return;
    }

    let server = start_mock_server().await;
    let search_dir_name = "grep-auto-compact";
    let call_id = "grep-files-auto-compact";
    let arguments = json!({
        "pattern": "needle",
        "path": search_dir_name,
        "limit": 400,
    })
    .to_string();

    let first_turn = sse(vec![
        ev_function_call(call_id, "grep_files", &arguments),
        ev_completed_with_tokens("r1", 50),
    ]);
    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let auto_compact_turn = sse(vec![
        ev_assistant_message("m2", &auto_summary_payload),
        ev_completed_with_tokens("r2", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r3", 20),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![first_turn, auto_compact_turn, post_compact_follow_up],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
            config.model_context_window = Some(20_000);
            config.model_auto_compact_token_limit = Some(1_200);
        });
    let test = builder.build(&server).await.unwrap();

    let search_dir = test.cwd.path().join(search_dir_name);
    std::fs::create_dir_all(&search_dir).unwrap();
    for idx in 0..250 {
        let file_name = format!("needle_match_file_{idx:03}_aaaaaaaaaaaaaaaaaaaaaaaaaaaa.txt");
        std::fs::write(search_dir.join(file_name), "needle\n").unwrap();
    }

    test.submit_turn("search for needle matches").await.unwrap();

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        3,
        "expected request, same-turn auto compact, and post-compact follow-up"
    );

    let compact_request = &requests[1];
    let compact_body = compact_request.body_json().to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "second request should be the auto compact request"
    );

    let compact_tool_output = compact_request
        .function_call_output_text(call_id)
        .expect("auto compact request should include the stored tool output");
    assert!(
        compact_tool_output.contains("needle_match_file_000"),
        "auto compact should run after the grep_files output is recorded"
    );

    let follow_up_body = requests[2].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, &summary_with_prefix(&auto_summary_payload)),
        "post-compact follow-up should use compacted history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_slimming_raw_history_pressure_triggers_auto_compact() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let first_call_id = "raw-history-output-1";
    let second_call_id = "raw-history-output-2";
    let first_command = "for i in $(seq 1 1500); do echo raw-sentinel-output-1-line-$i; done";
    let second_command = "for i in $(seq 1 5000); do echo raw-sentinel-output-2-line-$i; done";
    let first_args = json!({
        "command": first_command,
        "timeout_ms": 5_000,
    })
    .to_string();
    let second_args = json!({
        "command": second_command,
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(first_call_id, "shell_command", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first raw output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_tool_turn = sse(vec![
        ev_function_call(second_call_id, "shell_command", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let auto_compact_turn = sse(vec![
        ev_assistant_message("m2", &auto_summary_payload),
        ev_completed_with_tokens("r4", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r5", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            second_tool_turn,
            auto_compact_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("gpt-5.1")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimming);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(50_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy("make the first raw output", SandboxPolicy::DangerFullAccess)
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn_with_policy(
        "make the second raw output",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected first tool turn, first follow-up, second tool turn, raw-pressure compact, and post-compact follow-up"
    );

    let second_turn_body = requests[2].body_json().to_string();
    assert!(
        second_turn_body.contains("<<lha-input:"),
        "historical first output should be slimmed in the ordinary second request"
    );

    let compact_body = requests[3].body_json().to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "fourth request should be the raw-pressure auto compact request"
    );
    assert!(
        compact_body.contains("raw-sentinel-output-1-line-1500"),
        "compact should include the raw first tool output"
    );
    assert!(
        compact_body.contains("raw-sentinel-output-2-line-2500"),
        "compact should include the raw second tool output"
    );
    assert!(
        !compact_body.contains("<<lha-input:"),
        "compact request should not use input-slimmed replacements"
    );
    assert!(
        !compact_body.contains("lha_input_retrieve"),
        "compact request should not advertise the input-slimming retrieval tool"
    );

    let follow_up_body = requests[4].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, &summary_with_prefix(&auto_summary_payload)),
        "post-compact follow-up should use compacted history"
    );
    assert!(
        !follow_up_body.contains("raw-sentinel-output-2-line-2500"),
        "post-compact follow-up should drop the raw tool output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_compact_does_not_persist_aborted_input_slimming_preview() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let call_id = "preflight-aborted-slimming-output";
    let command = "for i in $(seq 1 5000); do echo preflight-aborted-sentinel-line-$i; done";
    let args = json!({
        "command": command,
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(call_id, "shell_command", &args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "preflight-aborted output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let preflight_compact_turn = sse(vec![
        ev_assistant_message("m2", &auto_summary_payload),
        ev_completed_with_tokens("r3", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r4", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            preflight_compact_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("gpt-5.1")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimming);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy(
        "create preflight-aborted slimming output",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn("trigger preflight compact").await.unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        4,
        "expected initial tool turn, follow-up, preflight compact, and post-compact retry"
    );

    let compact_body = requests[2].body_json().to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "third request should be the preflight compact request"
    );
    assert!(
        compact_body.contains("preflight-aborted-sentinel-line-5000"),
        "raw preflight compact should include the unslimmed historical tool output"
    );
    assert!(
        !compact_body.contains("<<lha-input:"),
        "aborted input-slimming preview should not persist markers into raw compact"
    );
    assert!(
        !compact_body.contains("lha_input_retrieve"),
        "raw preflight compact should not advertise the input-slimming retrieval tool"
    );

    let follow_up_body = requests[3].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, &summary_with_prefix(&auto_summary_payload)),
        "post-compact retry should use compacted history"
    );

    let rollout_path = test.session_configured.rollout_path.expect("rollout path");
    let rollout_text = tokio::fs::read_to_string(rollout_path)
        .await
        .expect("read rollout");
    assert_eq!(
        input_slimming_sidecar_hashes_from_rollout(&rollout_text),
        HashSet::new(),
        "preflight-aborted input-slimming preview should not persist sidecars"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_slimming_conflict_uses_raw_compact_even_when_retrieval_aware_enabled() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let first_call_id = "conflict-raw-compact-output-1";
    let second_call_id = "conflict-raw-compact-output-2";
    let first_args = json!({
        "command": "for i in $(seq 1 1500); do echo conflict-raw-compact-1-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();
    let second_args = json!({
        "command": "for i in $(seq 1 5000); do echo conflict-raw-compact-2-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(first_call_id, "shell_command", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first conflict raw output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_tool_turn = sse(vec![
        ev_function_call(second_call_id, "shell_command", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let auto_compact_turn = sse(vec![
        ev_assistant_message("m2", &auto_summary_payload),
        ev_completed_with_tokens("r4", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r5", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            second_tool_turn,
            auto_compact_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("gpt-5.1")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimming);
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(50_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy(
        "make the first conflict raw compact output",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    let warning = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::Warning(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    assert_eq!(warning.message, INPUT_SLIMMING_CONFLICT_WARNING);
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn_with_policy(
        "make the second conflict raw compact output",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        5,
        "conflict mode should not defer raw-pressure compaction"
    );

    let second_turn_body = requests[2].body_json().to_string();
    assert!(
        !second_turn_body.contains("<<lha-input:"),
        "conflict mode should not create new input-slimming replacements"
    );

    let compact_body = requests[3].body_json().to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "fourth request should be the raw compact request"
    );
    assert!(
        compact_body.contains("conflict-raw-compact-1-line-1500"),
        "raw compact should include the first raw tool output"
    );
    assert!(
        compact_body.contains("conflict-raw-compact-2-line-2500"),
        "raw compact should include the second raw tool output"
    );
    assert!(
        !compact_body.contains("<<lha-input:"),
        "conflict compact request should not use input-slimmed replacements"
    );
    assert!(
        !compact_body.contains("lha_input_retrieve"),
        "conflict compact request should not advertise the retrieval tool"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_aware_compact_defers_raw_pressure_when_slimmed_send_fits() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let first_call_id = "retrieval-aware-output-1";
    let second_call_id = "retrieval-aware-output-2";
    let first_args = json!({
        "command": "for i in $(seq 1 1500); do echo retrieval-aware-sentinel-1-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();
    let second_args = json!({
        "command": "for i in $(seq 1 5000); do echo retrieval-aware-sentinel-2-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(first_call_id, "shell_command", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first retrieval-aware output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_tool_turn = sse(vec![
        ev_function_call(second_call_id, "shell_command", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let final_follow_up = sse(vec![
        ev_assistant_message("m2", FINAL_REPLY),
        ev_completed_with_tokens("r4", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            second_tool_turn,
            final_follow_up,
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("gpt-5.1")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(50_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy(
        "make the first retrieval-aware output",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn_with_policy(
        "make the second retrieval-aware output",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        4,
        "expected two tool turns and their follow-ups, with no auto compact request"
    );
    assert!(
        requests.iter().all(|request| !body_contains_text(
            &request.body_json().to_string(),
            SUMMARIZATION_PROMPT
        )),
        "retrieval-aware compact should defer raw pressure instead of compacting"
    );

    let final_body = requests[3].body_json().to_string();
    assert!(
        final_body.contains("<<lha-input:"),
        "live-zone slimming should run before the follow-up request"
    );
    assert!(
        final_body.contains("lha_input_retrieve"),
        "slimmed follow-up should keep the retrieval tool available"
    );
    assert!(
        !final_body.contains("retrieval-aware-sentinel-2-line-5000"),
        "slimmed follow-up should not send the full live-zone output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_aware_compact_uses_slimmed_compact_prompt_and_preserves_markers() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let first_call_id = "retrieval-aware-compact-output-1";
    let second_call_id = "retrieval-aware-compact-output-2";
    let first_args = json!({
        "command": "for i in $(seq 1 1500); do echo retrieval-aware-compact-sentinel-1-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();
    let second_args = json!({
        "command": "for i in $(seq 1 5000); do echo retrieval-aware-compact-sentinel-2-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(first_call_id, "shell_command", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first retrieval-aware compact output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_tool_turn = sse(vec![
        ev_function_call(second_call_id, "shell_command", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let fake_hash = "ffffffffffffffffffffffff";
    let compact_summary = format!(
        "Current task and decisions\n- Compact with unresolved evidence.\n- Fake marker <<lha-input:{fake_hash}>> should not persist."
    );
    let retrieval_aware_compact_turn = sse(vec![
        ev_assistant_message("m2", &compact_summary),
        ev_completed_with_tokens("r4", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r5", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            second_tool_turn,
            retrieval_aware_compact_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    let model_provider = openai_model_provider(&server);
    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_model("gpt-5.1")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.features.enable(Feature::RemoteCompaction);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy(
        "make retrieval-aware compact output one",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn_with_policy(
        "make retrieval-aware compact output two",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected retrieval-aware compact between the second tool turn and final follow-up"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.path() != "/v1/responses/compact"),
        "retrieval-aware compact should use the local tool-capable compact path"
    );
    let all_paths = server
        .received_requests()
        .await
        .expect("mock server should capture requests")
        .into_iter()
        .map(|request| request.url.path().to_string())
        .collect::<Vec<_>>();
    assert!(
        !all_paths.iter().any(|path| path == "/v1/responses/compact"),
        "retrieval-aware compact should bypass remote compaction"
    );

    let compact_body = requests[3].body_json();
    let compact_body_text = compact_body.to_string();
    assert!(
        body_contains_text(&compact_body_text, SUMMARIZATION_PROMPT),
        "retrieval-aware compact should include the summarization prompt"
    );
    assert!(
        compact_body_text.contains("<<lha-input:"),
        "retrieval-aware compact should use input-slimmed replacements"
    );
    assert!(
        compact_body_text.contains("lha_input_retrieve"),
        "retrieval-aware compact should expose the retrieve tool"
    );
    assert!(
        !compact_body_text.contains("retrieval-aware-compact-sentinel-2-line-5000"),
        "retrieval-aware compact should not send the full raw tool output"
    );
    let tools = compact_body["tools"]
        .as_array()
        .expect("retrieval-aware compact tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("name").and_then(|value| value.as_str()),
        Some("lha_input_retrieve")
    );

    let follow_up_body = requests[4].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, "Compact with unresolved evidence."),
        "post-compact follow-up should include the compact summary"
    );
    assert!(
        !follow_up_body.contains(&format!("<<lha-input:{fake_hash}>>")),
        "non-durable markers invented in the compact summary should be neutralized"
    );
    assert!(
        follow_up_body.contains(&format!("input slimming marker {fake_hash} omitted")),
        "post-compact follow-up should explain why non-durable markers were omitted"
    );
    assert!(
        follow_up_body.contains("Unresolved retrievable markers:"),
        "unretrieved markers should be preserved in the compacted summary"
    );
    assert!(
        follow_up_body.contains("<<lha-input:"),
        "post-compact follow-up should keep unresolved markers retrievable"
    );
    let follow_up_hashes = input_slimming_hashes_from_text_for_test(&follow_up_body);
    assert!(
        !follow_up_hashes.is_empty(),
        "post-compact follow-up should contain at least one durable marker"
    );
    let rollout_path = test.session_configured.rollout_path.expect("rollout path");
    let rollout_text = tokio::fs::read_to_string(rollout_path)
        .await
        .expect("read rollout");
    let sidecar_hashes = input_slimming_sidecar_hashes_from_rollout(&rollout_text);
    assert!(
        follow_up_hashes.is_subset(&sidecar_hashes),
        "persisted compact markers should all have input_slimming_stored_input sidecars"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranked_marker_compact_uses_slimmed_prompt_without_retrieve_tool() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RankedMarkerCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(12_000);
        });
    let test = builder.build(&server).await.unwrap();

    let prefix = "ranked-marker-slimmed-prompt";
    let line_count = 5_000;
    let file = test.cwd.path().join("ranked-marker-slimmed-prompt.txt");
    fs::write(&file, read_file_fixture_contents(prefix, line_count)).unwrap();
    let call_id = "ranked-marker-slimmed-prompt-output";
    let args = json!({
        "file_path": file.to_string_lossy(),
        "offset": 1,
        "limit": line_count,
    })
    .to_string();

    let read_turn = sse(vec![
        ev_function_call(call_id, "read_file", &args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let compact_turn = sse(vec![
        ev_assistant_message("m1", "Ranked compact summary without retrieved evidence."),
        ev_completed_with_tokens("r2", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m2", FINAL_REPLY),
        ev_completed_with_tokens("r3", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![read_turn, compact_turn, post_compact_follow_up],
    )
    .await;

    test.submit_turn("read ranked marker slimmed prompt file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        3,
        "expected ranked-marker compact before the tool follow-up"
    );
    let compact_body = requests[1].body_json();
    let compact_body_text = compact_body.to_string();
    assert!(
        body_contains_text(&compact_body_text, SUMMARIZATION_PROMPT),
        "ranked-marker compact should include the summarization prompt"
    );
    assert!(
        compact_body_text.contains("<<lha-input:"),
        "ranked-marker compact should use input-slimmed replacements"
    );
    assert!(
        !compact_body_text.contains("lha_input_retrieve"),
        "ranked-marker compact should not expose the retrieve tool"
    );
    assert!(
        compact_body
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty),
        "ranked-marker compact should send no tools"
    );
    assert!(
        !compact_body_text.contains(&format!("L{line_count}: {prefix}-line-{line_count}")),
        "ranked-marker compact should not send the full raw tool output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranked_marker_compact_appends_only_top_ranked_markers() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RankedMarkerCompact);
            config.tool_output_token_limit = Some(300_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(2_000);
        });
    let test = builder.build(&server).await.unwrap();

    let line_count = 1_200;
    let low_prefix = "ranked-low-marker";
    let high_prefix = "ranked-high-error-failed-panic-test-result";
    let mut calls = Vec::new();
    let mut function_events = Vec::new();
    for index in 0..13 {
        let prefix = if index == 0 {
            low_prefix.to_string()
        } else if index == 12 {
            high_prefix.to_string()
        } else {
            format!("ranked-ordinary-marker-{index}")
        };
        let file = test.cwd.path().join(format!("ranked-marker-{index}.txt"));
        fs::write(&file, read_file_fixture_contents(&prefix, line_count)).unwrap();
        let call_id = format!("ranked-marker-top-k-output-{index}");
        let args = json!({
            "file_path": file.to_string_lossy(),
            "offset": 1,
            "limit": line_count,
        })
        .to_string();
        function_events.push(ev_function_call(&call_id, "read_file", &args));
        calls.push((prefix, call_id));
    }
    function_events.push(ev_completed_with_tokens("r1", 1_000));

    let tool_turn = sse(function_events);
    let compact_turn = sse(vec![
        ev_assistant_message("m1", "Ranked compact summary for top-k markers."),
        ev_completed_with_tokens("r2", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m2", FINAL_REPLY),
        ev_completed_with_tokens("r3", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![tool_turn, compact_turn, post_compact_follow_up],
    )
    .await;

    test.submit_turn("read many ranked marker files")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        3,
        "expected ranked-marker compact before the follow-up"
    );
    let compact_body = requests[1].body_json().to_string();
    let high_hash = input_slimming_hash_for_test(&read_file_tool_output(high_prefix, line_count));
    let low_hash = input_slimming_hash_for_test(&read_file_tool_output(low_prefix, line_count));
    assert!(
        compact_body.contains(&format!("<<lha-input:{high_hash}>>")),
        "compact prompt should contain the high-signal marker"
    );
    assert!(
        compact_body.contains(&format!("<<lha-input:{low_hash}>>")),
        "compact prompt should contain the low-signal marker before ranking"
    );

    let follow_up_body = requests[2].body_json().to_string();
    let ranked_hashes = ranked_marker_section_hashes_for_test(&follow_up_body);
    assert!(
        follow_up_body.contains("Ranked retrievable markers:"),
        "post-compact follow-up should include ranked marker section"
    );
    assert!(
        ranked_hashes.len() <= 12,
        "ranked marker retention should keep at most 12 markers"
    );
    assert!(
        ranked_hashes.contains(&high_hash),
        "failure/test output should be retained"
    );
    assert!(
        !ranked_hashes.contains(&low_hash),
        "lowest-ranked marker should be omitted"
    );
    assert!(
        calls
            .iter()
            .all(|(_, call_id)| !follow_up_body.contains(call_id)),
        "ranked marker section should not preserve transient tool call ids"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranked_marker_compact_retrieval_count_boosts_marker() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RankedMarkerCompact);
            config.tool_output_token_limit = Some(300_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    let retrieved_prefix = "ranked-retrieved-marker";
    let line_count = 1_500;
    let retrieved_file = test.cwd.path().join("ranked-retrieved-marker.txt");
    fs::write(
        &retrieved_file,
        read_file_fixture_contents(retrieved_prefix, line_count),
    )
    .unwrap();
    let retrieved_hash =
        input_slimming_hash_for_test(&read_file_tool_output(retrieved_prefix, line_count));
    let read_args = json!({
        "file_path": retrieved_file.to_string_lossy(),
        "offset": 1,
        "limit": line_count,
    })
    .to_string();
    let retrieve_args = json!({
        "hash": retrieved_hash,
        "query": format!("{retrieved_prefix}-line-{line_count}"),
    })
    .to_string();

    let first_read_turn = sse(vec![
        ev_function_call("ranked-retrieved-read", "read_file", &read_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "retrieved marker source recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let retrieve_turn = sse(vec![
        ev_function_call(
            "ranked-retrieved-marker-retrieve",
            "lha_input_retrieve",
            &retrieve_args,
        ),
        ev_completed_with_tokens("r3", 10),
    ]);
    let retrieve_follow_up = sse(vec![
        ev_assistant_message("m2", "retrieved marker inspected"),
        ev_completed_with_tokens("r4", 10),
    ]);

    let mut later_events = Vec::new();
    for index in 0..13 {
        let prefix = format!("ranked-retrieval-boost-ordinary-{index}");
        let file = test.cwd.path().join(format!("ranked-boost-{index}.txt"));
        fs::write(&file, read_file_fixture_contents(&prefix, line_count)).unwrap();
        let call_id = format!("ranked-retrieval-boost-output-{index}");
        let args = json!({
            "file_path": file.to_string_lossy(),
            "offset": 1,
            "limit": line_count,
        })
        .to_string();
        later_events.push(ev_function_call(&call_id, "read_file", &args));
    }
    later_events.push(ev_completed_with_tokens("r5", 1_000));
    let later_tool_turn = sse(later_events);
    let compact_turn = sse(vec![
        ev_assistant_message("m3", "Ranked compact summary after retrieval boost."),
        ev_completed_with_tokens("r6", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r7", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_read_turn,
            first_follow_up,
            retrieve_turn,
            retrieve_follow_up,
            later_tool_turn,
            compact_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    test.submit_turn("read marker that will be retrieved")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn("retrieve the earlier marker")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn("read more marker candidates")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        7,
        "expected retrieval, ranked compact, and post-compact follow-up"
    );
    let follow_up_body = requests[6].body_json().to_string();
    let ranked_hashes = ranked_marker_section_hashes_for_test(&follow_up_body);
    assert!(
        ranked_hashes.contains(&retrieved_hash),
        "retrieved marker should be retained despite older occurrence"
    );
    assert!(
        ranked_hashes.len() <= 12,
        "retrieved marker retention should still respect top-k budget"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranked_marker_compact_does_not_append_raw_original_text() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RankedMarkerCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(12_000);
        });
    let test = builder.build(&server).await.unwrap();

    let prefix = "ranked-hidden-raw-original";
    let line_count = 5_000;
    let hidden_raw_line = format!("L2500: {prefix}-line-2500");
    let file = test.cwd.path().join("ranked-hidden-raw-original.txt");
    fs::write(&file, read_file_fixture_contents(prefix, line_count)).unwrap();
    let args = json!({
        "file_path": file.to_string_lossy(),
        "offset": 1,
        "limit": line_count,
    })
    .to_string();

    let read_turn = sse(vec![
        ev_function_call("ranked-hidden-raw-output", "read_file", &args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let compact_turn = sse(vec![
        ev_assistant_message("m1", "Ranked compact summary without raw sentinel."),
        ev_completed_with_tokens("r2", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m2", FINAL_REPLY),
        ev_completed_with_tokens("r3", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![read_turn, compact_turn, post_compact_follow_up],
    )
    .await;

    test.submit_turn("read hidden raw ranked marker file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 3);
    let follow_up_body = requests[2].body_json().to_string();
    assert!(
        follow_up_body.contains("Ranked retrievable markers:"),
        "ranked compact should append retrievable marker references"
    );
    assert!(
        follow_up_body.contains("<<lha-input:"),
        "ranked compact should retain marker references"
    );
    assert!(
        !follow_up_body.contains(&hidden_raw_line),
        "ranked marker section should not append raw original text"
    );
    assert!(
        !follow_up_body.contains("Original input for <<lha-input:"),
        "ranked marker section should not include retrieval payloads"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_aware_raw_fallback_preserves_existing_durable_marker() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let first_call_id = "retrieval-aware-raw-fallback-output-1";
    let second_call_id = "retrieval-aware-raw-fallback-output-2";
    let first_args = json!({
        "command": "for i in $(seq 1 1500); do echo retrieval-aware-raw-fallback-sentinel-1-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();
    let second_args = json!({
        "command": "for i in $(seq 1 5000); do echo retrieval-aware-raw-fallback-sentinel-2-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(first_call_id, "shell_command", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first raw fallback output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_tool_turn = sse(vec![
        ev_function_call(second_call_id, "shell_command", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let retrieval_summary =
        "Current task and decisions\n- Compact with unresolved raw-fallback evidence.";
    let retrieval_aware_compact_turn = sse(vec![
        ev_assistant_message("m2", retrieval_summary),
        ev_completed_with_tokens("r4", 10),
    ]);
    let post_retrieval_compact_follow_up = sse(vec![
        ev_assistant_message("m3", "post retrieval-aware compact response"),
        ev_completed_with_tokens("r5", 10),
    ]);
    let raw_fallback_summary = "Raw fallback compact summary omitted durable markers.";
    let raw_fallback_compact_turn = sse(vec![
        ev_assistant_message("m4", raw_fallback_summary),
        ev_completed_with_tokens("r6", 10),
    ]);
    let after_raw_fallback = sse(vec![
        ev_assistant_message("m5", FINAL_REPLY),
        ev_completed_with_tokens("r7", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            second_tool_turn,
            retrieval_aware_compact_turn,
            post_retrieval_compact_follow_up,
            raw_fallback_compact_turn,
            after_raw_fallback,
        ],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy(
        "make retrieval-aware raw fallback output one",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn_with_policy(
        "make retrieval-aware raw fallback output two",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected retrieval-aware compact before the raw fallback trigger"
    );
    let post_retrieval_body = requests[4].body_json().to_string();
    let durable_hashes = input_slimming_hashes_from_text_for_test(&post_retrieval_body);
    assert!(
        !durable_hashes.is_empty(),
        "post retrieval-aware compact history should contain durable markers"
    );

    let oversized_user = "raw-fallback-pressure ".repeat(5_000);
    test.submit_turn(&oversized_user).await.unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        7,
        "expected raw fallback compact followed by the retried user request"
    );
    let raw_compact_body = requests[5].body_json().to_string();
    assert!(
        body_contains_text(&raw_compact_body, SUMMARIZATION_PROMPT),
        "sixth request should be the raw compact fallback"
    );
    assert!(
        !raw_compact_body.contains("lha_input_retrieve"),
        "raw fallback compact should not advertise retrieval tools"
    );

    let follow_up_body = requests[6].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, raw_fallback_summary),
        "post-raw-fallback request should include the raw compact summary"
    );
    assert!(
        follow_up_body.contains("Unresolved retrievable markers:"),
        "raw fallback summary should be guarded with unresolved markers"
    );
    for hash in &durable_hashes {
        assert!(
            follow_up_body.contains(&format!("<<lha-input:{hash}>>")),
            "durable marker {hash} should remain retrievable after raw fallback"
        );
    }

    let rollout_path = test.session_configured.rollout_path.expect("rollout path");
    let rollout_text = tokio::fs::read_to_string(rollout_path)
        .await
        .expect("read rollout");
    let sidecar_hashes = input_slimming_sidecar_hashes_from_rollout(&rollout_text);
    assert!(
        durable_hashes.is_subset(&sidecar_hashes),
        "preserved markers should all have input_slimming_stored_input sidecars"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_preserves_existing_durable_input_slimming_marker() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let first_call_id = "remote-preserve-marker-output-1";
    let second_call_id = "remote-preserve-marker-output-2";
    let first_args = json!({
        "command": "for i in $(seq 1 1500); do echo remote-preserve-marker-sentinel-1-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();
    let second_args = json!({
        "command": "for i in $(seq 1 5000); do echo remote-preserve-marker-sentinel-2-line-$i; done",
        "timeout_ms": 5_000,
    })
    .to_string();

    let first_tool_turn = sse(vec![
        ev_function_call(first_call_id, "shell_command", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first remote preserve output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_tool_turn = sse(vec![
        ev_function_call(second_call_id, "shell_command", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let retrieval_summary =
        "Current task and decisions\n- Compact with unresolved remote-compact evidence.";
    let retrieval_aware_compact_turn = sse(vec![
        ev_assistant_message("m2", retrieval_summary),
        ev_completed_with_tokens("r4", 10),
    ]);
    let post_retrieval_compact_follow_up = sse(vec![
        ev_assistant_message("m3", "post retrieval-aware remote response"),
        ev_completed_with_tokens("r5", 10),
    ]);
    let after_remote_compact = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r6", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_tool_turn,
            first_follow_up,
            second_tool_turn,
            retrieval_aware_compact_turn,
            post_retrieval_compact_follow_up,
            after_remote_compact,
        ],
    )
    .await;

    let fake_hash = "ffffffffffffffffffffffff";
    let remote_summary = format!("REMOTE_COMPACT_SUMMARY_WITH_FAKE <<lha-input:{fake_hash}>>");
    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: remote_summary.clone(),
        }],
        end_turn: None,
    }];
    let compact_mock =
        mount_compact_json_once(&server, serde_json::json!({ "output": compacted_history })).await;

    let model_provider = openai_model_provider(&server);
    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_model("gpt-5.1")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.features.enable(Feature::RemoteCompaction);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn_with_policy(
        "make remote compact preserve marker output one",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn_with_policy(
        "make remote compact preserve marker output two",
        SandboxPolicy::DangerFullAccess,
    )
    .await
    .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected retrieval-aware compact before manual remote compact"
    );
    let post_retrieval_body = requests[4].body_json().to_string();
    let durable_hashes = input_slimming_hashes_from_text_for_test(&post_retrieval_body);
    assert!(
        !durable_hashes.is_empty(),
        "post retrieval-aware compact history should contain durable markers"
    );

    test.codex
        .submit(Op::Compact)
        .await
        .expect("trigger compact");
    wait_for_event(&test.codex, |ev| {
        matches!(
            ev,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::ContextCompaction(_),
                ..
            })
        )
    })
    .await;

    let compact_requests = compact_mock.requests();
    assert_eq!(
        compact_requests.len(),
        1,
        "manual compact should use the remote compact endpoint"
    );
    assert_eq!(
        compact_requests[0].path(),
        "/v1/responses/compact",
        "manual compact should hit the compact endpoint"
    );

    test.submit_turn("after remote compact").await.unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        6,
        "expected one follow-up after manual remote compact"
    );
    let follow_up_body = requests[5].body_json().to_string();
    assert!(
        follow_up_body.contains("REMOTE_COMPACT_SUMMARY_WITH_FAKE"),
        "post-remote-compact request should include the remote compact summary"
    );
    assert!(
        !follow_up_body.contains(&format!("<<lha-input:{fake_hash}>>")),
        "remote compact fake marker should be neutralized"
    );
    assert!(
        follow_up_body.contains(&format!("input slimming marker {fake_hash} omitted")),
        "post-remote-compact request should explain why the fake marker was omitted"
    );
    assert!(
        follow_up_body.contains("Unresolved retrievable markers:"),
        "remote compact output should be guarded with unresolved markers"
    );
    for hash in &durable_hashes {
        let marker = format!("<<lha-input:{hash}>>");
        assert!(
            follow_up_body.contains(&marker),
            "durable marker {hash} should remain retrievable after remote compact"
        );
        assert_eq!(
            follow_up_body.matches(&marker).count(),
            1,
            "remote compact marker {hash} should not be duplicated"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_aware_compact_executes_retrieve_tool() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    let first_prefix = "retrieval-aware-read-sentinel-1";
    let second_prefix = "retrieval-aware-read-sentinel-2";
    let first_line_count = 1_500;
    let second_line_count = 5_000;
    let first_file = test.cwd.path().join("retrieval-aware-read-1.txt");
    let second_file = test.cwd.path().join("retrieval-aware-read-2.txt");
    fs::write(
        &first_file,
        read_file_fixture_contents(first_prefix, first_line_count),
    )
    .unwrap();
    fs::write(
        &second_file,
        read_file_fixture_contents(second_prefix, second_line_count),
    )
    .unwrap();

    let first_call_id = "retrieval-aware-read-output-1";
    let second_call_id = "retrieval-aware-read-output-2";
    let retrieve_call_id = "retrieval-aware-compact-retrieve-call";
    let first_args = json!({
        "file_path": first_file.to_string_lossy(),
        "offset": 1,
        "limit": first_line_count,
    })
    .to_string();
    let second_args = json!({
        "file_path": second_file.to_string_lossy(),
        "offset": 1,
        "limit": second_line_count,
    })
    .to_string();
    let second_output = read_file_tool_output(second_prefix, second_line_count);
    let second_hash = input_slimming_hash_for_test(&second_output);
    let retrieved_sentinel = format!("{second_prefix}-line-{second_line_count}");
    let retrieve_args = json!({
        "hash": second_hash,
        "query": retrieved_sentinel,
    })
    .to_string();

    let first_read_turn = sse(vec![
        ev_function_call(first_call_id, "read_file", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first deterministic read output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_read_turn = sse(vec![
        ev_function_call(second_call_id, "read_file", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let retrieval_call_turn = sse(vec![
        ev_function_call(retrieve_call_id, "lha_input_retrieve", &retrieve_args),
        ev_completed_with_tokens("r4", 10),
    ]);
    let retrieved_summary = format!(
        "Current task and decisions\n- Retrieved deterministic evidence.\n\nEvidence retrieved\n- {retrieved_sentinel}\n\nUnresolved retrievable markers\n- None relevant.\n\nNext steps\n- Continue."
    );
    let retrieved_summary_turn = sse(vec![
        ev_assistant_message("m2", &retrieved_summary),
        ev_completed_with_tokens("r5", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r6", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_read_turn,
            first_follow_up,
            second_read_turn,
            retrieval_call_turn,
            retrieved_summary_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    test.submit_turn("read the first deterministic retrieval file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn("read the second deterministic retrieval file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        6,
        "expected compact retrieval call, compact follow-up, and post-compact follow-up"
    );

    let compact_body = requests[3].body_json();
    let compact_body_text = compact_body.to_string();
    assert!(
        compact_body_text.contains("<<lha-input:"),
        "retrieval-aware compact should send slimmed markers before retrieving"
    );
    assert!(
        !compact_body_text.contains(&format!("L{second_line_count}: {retrieved_sentinel}")),
        "retrieval-aware compact should not send the raw matching line before retrieval"
    );
    let tools = compact_body["tools"]
        .as_array()
        .expect("retrieval-aware compact tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].get("name").and_then(|value| value.as_str()),
        Some("lha_input_retrieve")
    );

    let (retrieved_output, success) = requests[4]
        .function_call_output_content_and_success(retrieve_call_id)
        .expect("retrieval tool output should be sent back to the compact model");
    assert_eq!(success, Some(true));
    assert!(
        retrieved_output
            .as_deref()
            .is_some_and(|output| output.contains(&retrieved_sentinel)),
        "retrieval tool output should include the requested original line"
    );

    let follow_up_body = requests[5].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, &retrieved_summary),
        "post-compact follow-up should use the retrieved compact summary"
    );
    assert!(
        !follow_up_body.contains(retrieve_call_id),
        "transient retrieve call should not enter replacement history"
    );
    assert!(
        !follow_up_body.contains("Original input for <<lha-input:"),
        "transient retrieve output should not enter replacement history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_aware_compact_query_miss_keeps_unresolved_marker() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    let first_prefix = "retrieval-aware-miss-sentinel-1";
    let second_prefix = "retrieval-aware-miss-sentinel-2";
    let first_line_count = 1_500;
    let second_line_count = 5_000;
    let first_file = test.cwd.path().join("retrieval-aware-miss-1.txt");
    let second_file = test.cwd.path().join("retrieval-aware-miss-2.txt");
    fs::write(
        &first_file,
        read_file_fixture_contents(first_prefix, first_line_count),
    )
    .unwrap();
    fs::write(
        &second_file,
        read_file_fixture_contents(second_prefix, second_line_count),
    )
    .unwrap();

    let first_call_id = "retrieval-aware-miss-output-1";
    let second_call_id = "retrieval-aware-miss-output-2";
    let retrieve_call_id = "retrieval-aware-compact-query-miss-call";
    let first_args = json!({
        "file_path": first_file.to_string_lossy(),
        "offset": 1,
        "limit": first_line_count,
    })
    .to_string();
    let second_args = json!({
        "file_path": second_file.to_string_lossy(),
        "offset": 1,
        "limit": second_line_count,
    })
    .to_string();
    let second_output = read_file_tool_output(second_prefix, second_line_count);
    let second_hash = input_slimming_hash_for_test(&second_output);
    let missing_query = "definitely-not-in-retrieval-aware-miss-output";
    let retrieve_args = json!({
        "hash": second_hash,
        "query": missing_query,
    })
    .to_string();

    let first_read_turn = sse(vec![
        ev_function_call(first_call_id, "read_file", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first query-miss read output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_read_turn = sse(vec![
        ev_function_call(second_call_id, "read_file", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let retrieval_call_turn = sse(vec![
        ev_function_call(retrieve_call_id, "lha_input_retrieve", &retrieve_args),
        ev_completed_with_tokens("r4", 10),
    ]);
    let summary_without_marker = "Current task and decisions\n- Query miss happened, but the compact summary omitted the marker.\n\nEvidence retrieved\n- No matching lines were found.\n\nNext steps\n- Continue.";
    let summary_turn = sse(vec![
        ev_assistant_message("m2", summary_without_marker),
        ev_completed_with_tokens("r5", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r6", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_read_turn,
            first_follow_up,
            second_read_turn,
            retrieval_call_turn,
            summary_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    test.submit_turn("read the first query miss retrieval file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn("read the second query miss retrieval file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        6,
        "expected compact query miss, compact summary, and post-compact follow-up"
    );

    let (miss_output, success) = requests[4]
        .function_call_output_content_and_success(retrieve_call_id)
        .expect("query miss output should be sent back to the compact model");
    assert_eq!(success, Some(true));
    assert!(
        miss_output
            .as_deref()
            .is_some_and(|output| output.contains("did not match any lines or sections")),
        "query miss should return an explicit no-match result"
    );

    let follow_up_body = requests[5].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, summary_without_marker),
        "post-compact follow-up should include the compact model summary"
    );
    assert!(
        follow_up_body.contains("Unresolved retrievable markers:"),
        "query-missed markers omitted by the model should be restored by the guard"
    );
    assert!(
        follow_up_body.contains(&format!("<<lha-input:{second_hash}>>")),
        "the query-missed marker should remain retrievable after compaction"
    );
    assert!(
        !follow_up_body.contains(&format!(
            "L{second_line_count}: {second_prefix}-line-{second_line_count}"
        )),
        "post-compact follow-up should not contain the raw original output"
    );

    let rollout_path = test.session_configured.rollout_path.expect("rollout path");
    let rollout_text = tokio::fs::read_to_string(rollout_path)
        .await
        .expect("read rollout");
    let sidecar_hashes = input_slimming_sidecar_hashes_from_rollout(&rollout_text);
    assert!(
        sidecar_hashes.contains(&second_hash),
        "query-missed unresolved marker should have a persisted sidecar"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_aware_compact_large_full_retrieve_keeps_unresolved_marker() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex()
        .with_model("test-gpt-5")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.features.enable(Feature::InputSlimmingLiveZone);
            config.features.enable(Feature::RetrievalAwareCompact);
            config.tool_output_token_limit = Some(200_000);
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(20_000);
        });
    let test = builder.build(&server).await.unwrap();

    let first_prefix = "retrieval-aware-large-sentinel-1";
    let second_prefix = "retrieval-aware-large-sentinel-2";
    let first_line_count = 1_500;
    let second_line_count = 5_000;
    let first_file = test.cwd.path().join("retrieval-aware-large-1.txt");
    let second_file = test.cwd.path().join("retrieval-aware-large-2.txt");
    fs::write(
        &first_file,
        read_file_fixture_contents(first_prefix, first_line_count),
    )
    .unwrap();
    fs::write(
        &second_file,
        read_file_fixture_contents(second_prefix, second_line_count),
    )
    .unwrap();

    let first_call_id = "retrieval-aware-large-output-1";
    let second_call_id = "retrieval-aware-large-output-2";
    let retrieve_call_id = "retrieval-aware-compact-large-full-call";
    let first_args = json!({
        "file_path": first_file.to_string_lossy(),
        "offset": 1,
        "limit": first_line_count,
    })
    .to_string();
    let second_args = json!({
        "file_path": second_file.to_string_lossy(),
        "offset": 1,
        "limit": second_line_count,
    })
    .to_string();
    let second_output = read_file_tool_output(second_prefix, second_line_count);
    let second_hash = input_slimming_hash_for_test(&second_output);
    let retrieve_args = json!({
        "hash": second_hash,
    })
    .to_string();

    let first_read_turn = sse(vec![
        ev_function_call(first_call_id, "read_file", &first_args),
        ev_completed_with_tokens("r1", 1_000),
    ]);
    let first_follow_up = sse(vec![
        ev_assistant_message("m1", "first large full-retrieve output recorded"),
        ev_completed_with_tokens("r2", 1_000),
    ]);
    let second_read_turn = sse(vec![
        ev_function_call(second_call_id, "read_file", &second_args),
        ev_completed_with_tokens("r3", 1_000),
    ]);
    let retrieval_call_turn = sse(vec![
        ev_function_call(retrieve_call_id, "lha_input_retrieve", &retrieve_args),
        ev_completed_with_tokens("r4", 10),
    ]);
    let summary_without_marker = "Current task and decisions\n- Large full retrieval returned only a head/tail view, but the compact summary omitted the marker.\n\nEvidence retrieved\n- Head/tail only.\n\nNext steps\n- Continue.";
    let summary_turn = sse(vec![
        ev_assistant_message("m2", summary_without_marker),
        ev_completed_with_tokens("r5", 10),
    ]);
    let post_compact_follow_up = sse(vec![
        ev_assistant_message("m3", FINAL_REPLY),
        ev_completed_with_tokens("r6", 10),
    ]);
    let request_log = mount_sse_sequence(
        &server,
        vec![
            first_read_turn,
            first_follow_up,
            second_read_turn,
            retrieval_call_turn,
            summary_turn,
            post_compact_follow_up,
        ],
    )
    .await;

    test.submit_turn("read the first large full retrieval file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.submit_turn("read the second large full retrieval file")
        .await
        .unwrap();
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        6,
        "expected compact full retrieval, compact summary, and post-compact follow-up"
    );

    let (retrieve_output, success) = requests[4]
        .function_call_output_content_and_success(retrieve_call_id)
        .expect("full retrieval output should be sent back to the compact model");
    assert_eq!(success, Some(true));
    assert!(
        retrieve_output
            .as_deref()
            .is_some_and(|output| output.contains("larger than the retrieval budget")),
        "large no-query retrieval should return an explicit head/tail view"
    );

    let follow_up_body = requests[5].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, summary_without_marker),
        "post-compact follow-up should include the compact model summary"
    );
    assert!(
        follow_up_body.contains("Unresolved retrievable markers:"),
        "head/tail-only retrieval should leave the omitted marker unresolved"
    );
    assert!(
        follow_up_body.contains(&format!("<<lha-input:{second_hash}>>")),
        "the partially retrieved marker should remain retrievable after compaction"
    );
    assert!(
        !follow_up_body.contains(&format!(
            "L{}: {second_prefix}-line-{}",
            second_line_count / 2,
            second_line_count / 2
        )),
        "post-compact follow-up should not contain the raw middle of the large output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_auto_compact_runs_before_initial_request_when_input_exceeds_effective_context_window()
 {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let auto_summary_payload = auto_summary(AUTO_SUMMARY_TEXT);
    let preflight_compact_turn = sse(vec![
        ev_assistant_message("m1", &auto_summary_payload),
        ev_completed_with_tokens("r1", 10),
    ]);
    let post_compact_turn = sse(vec![
        ev_assistant_message("m2", FINAL_REPLY),
        ev_completed_with_tokens("r2", 10),
    ]);
    let request_log =
        mount_sse_sequence(&server, vec![preflight_compact_turn, post_compact_turn]).await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_context_window = Some(400);
    });
    let test = builder.build(&server).await.unwrap();

    let large_user_message = "preflight-overflow ".repeat(200);
    test.submit_turn(&large_user_message).await.unwrap();

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        2,
        "expected preflight compact request followed by the post-compact request"
    );

    let compact_body = requests[0].body_json().to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "first request should be the preflight compact request"
    );
    assert!(
        body_contains_text(&compact_body, &large_user_message),
        "preflight compact should include the oversized user message in history"
    );

    let follow_up_body = requests[1].body_json().to_string();
    assert!(
        body_contains_text(&follow_up_body, &summary_with_prefix(&auto_summary_payload)),
        "post-compact request should use compacted history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_auto_compact_with_messages_uses_local_compaction() {
    skip_if_no_network!();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(MessagesSeqResponder {
            num_calls: AtomicUsize::new(0),
            bodies: vec![
                messages_sse_text_and_stop_with_tokens("msg_1", AUTO_SUMMARY_TEXT, 200, 20),
                messages_sse_text_and_stop_with_tokens("msg_2", FINAL_REPLY, 120, 15),
            ],
        })
        .expect(2)
        .mount(&server)
        .await;

    let mut model_provider = RuntimeEndpoint::openai();
    model_provider.base_url = Some(format!("{}/v1", server.uri()));
    model_provider.env_key = Some("PATH".into());
    model_provider.set_realtime_turn_streaming_enabled(false);
    model_provider.set_message_turns();

    let test = test_codex()
        .with_config(move |config| {
            config.model = Some("claude-test".to_string());
            config.model_provider = model_provider;
            config.features.enable(Feature::RemoteCompaction);
            set_test_compact_prompt(config);
            config.model_context_window = Some(400);
        })
        .build(&server)
        .await
        .expect("build codex");

    let large_user_message = "preflight-overflow ".repeat(200);
    test.submit_turn(&large_user_message)
        .await
        .expect("submit large turn");

    let requests = server.received_requests().await.expect("capture requests");
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() != "/v1/responses/compact"),
        "Messages compaction should not hit the compact endpoint"
    );
    let message_requests: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/messages")
        .collect();
    assert_eq!(
        message_requests.len(),
        2,
        "expected preflight and follow-up Messages requests"
    );

    let compact_body: serde_json::Value =
        serde_json::from_slice(&message_requests[0].body).expect("decode first Messages request");
    assert!(
        compact_body.get("tools").is_none(),
        "local compact Messages request should omit tools when no tools are declared"
    );
    assert!(
        compact_body.get("tool_choice").is_none(),
        "local compact Messages request should omit tool_choice when no tools are declared"
    );
    let compact_body = compact_body.to_string();
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "first Messages request should be the local compact request"
    );
    assert!(
        body_contains_text(&compact_body, &large_user_message),
        "local compact request should include the oversized user message"
    );

    let follow_up_body: serde_json::Value =
        serde_json::from_slice(&message_requests[1].body).expect("decode second Messages request");
    let follow_up_body = follow_up_body.to_string();
    assert!(
        body_contains_text(
            &follow_up_body,
            &summary_with_prefix(&auto_summary(AUTO_SUMMARY_TEXT)),
        ),
        "follow-up Messages request should use compacted local history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_auto_compact_skips_request_that_fits_effective_context_window() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let request_log = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("m1", FINAL_REPLY),
            ev_completed_with_tokens("r1", 10),
        ])],
    )
    .await;

    let model_provider = non_openai_model_provider(&server);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider = model_provider;
        set_test_compact_prompt(config);
        config.model_context_window = Some(100_000);
    });
    let test = builder.build(&server).await.unwrap();

    test.submit_turn("small preflight-safe prompt")
        .await
        .unwrap();

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        1,
        "request under the effective context window should not preflight compact"
    );
    let first_input = requests[0].input();
    assert!(
        !contains_user_text(&first_input, SUMMARIZATION_PROMPT),
        "first request should be the normal turn request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_counts_encrypted_reasoning_before_last_user() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let first_user = "COUNT_PRE_LAST_REASONING";
    let second_user = "TRIGGER_COMPACT_AT_LIMIT";
    let third_user = "AFTER_REMOTE_COMPACT";

    let pre_last_reasoning_content = "a".repeat(2_400);
    let post_last_reasoning_content = "b".repeat(4_000);

    let first_turn = sse(vec![
        ev_reasoning_item("pre-reasoning", &["pre"], &[&pre_last_reasoning_content]),
        ev_completed_with_tokens("r1", 10),
    ]);
    let second_turn = sse(vec![
        ev_reasoning_item("post-reasoning", &["post"], &[&post_last_reasoning_content]),
        ev_completed_with_tokens("r2", 80),
    ]);
    let third_turn = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r4", 1),
    ]);

    let request_log = mount_sse_sequence(
        &server,
        vec![
            // Turn 1: reasoning before last user (should count).
            first_turn,
            // Turn 2: reasoning after last user (should be ignored for compaction).
            second_turn,
            // Turn 3: next user turn after remote compaction.
            third_turn,
        ],
    )
    .await;

    let compacted_history = vec![
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "REMOTE_COMPACT_SUMMARY".to_string(),
            }],
            end_turn: None,
        },
        TranscriptItem::Reasoning {
            id: "compact-summary".to_string(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("ENCRYPTED_COMPACTION_SUMMARY".to_string()),
        },
    ];
    let compact_mock =
        mount_compact_json_once(&server, serde_json::json!({ "output": compacted_history })).await;

    let codex = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_config(|config| {
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(300);
            config.features.enable(Feature::RemoteCompaction);
        })
        .build(&server)
        .await
        .expect("build codex")
        .codex;

    for (idx, user) in [first_user, second_user, third_user]
        .into_iter()
        .enumerate()
    {
        codex
            .submit(Op::UserInput {
                items: vec![UserInput::Text {
                    text: user.into(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .await
            .unwrap();
        wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

        if idx < 2 {
            assert!(
                compact_mock.requests().is_empty(),
                "remote compaction should not run before the next user turn"
            );
        }
    }

    let compact_requests = compact_mock.requests();
    assert_eq!(
        compact_requests.len(),
        1,
        "remote compaction should run once after the second turn"
    );
    assert_eq!(
        compact_requests[0].path(),
        "/v1/responses/compact",
        "remote compaction should hit the compact endpoint"
    );

    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        3,
        "conversation should include three user turns"
    );
    let second_request_body = requests[1].body_json().to_string();
    assert!(
        !second_request_body.contains("REMOTE_COMPACT_SUMMARY"),
        "second turn should not include compacted history"
    );
    let third_request_body = requests[2].body_json().to_string();
    assert!(
        third_request_body.contains("REMOTE_COMPACT_SUMMARY")
            || third_request_body.contains(FINAL_REPLY),
        "third turn should include compacted history"
    );
    assert!(
        third_request_body.contains("ENCRYPTED_COMPACTION_SUMMARY"),
        "third turn should include compaction summary item"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_runs_when_reasoning_header_clears_between_turns() {
    skip_if_no_network!();

    let server = start_mock_server().await;

    let first_user = "SERVER_INCLUDED_FIRST";
    let second_user = "SERVER_INCLUDED_SECOND";
    let third_user = "SERVER_INCLUDED_THIRD";

    let pre_last_reasoning_content = "a".repeat(2_400);
    let post_last_reasoning_content = "b".repeat(4_000);

    let first_turn = sse(vec![
        ev_reasoning_item("pre-reasoning", &["pre"], &[&pre_last_reasoning_content]),
        ev_completed_with_tokens("r1", 10),
    ]);
    let second_turn = sse(vec![
        ev_reasoning_item("post-reasoning", &["post"], &[&post_last_reasoning_content]),
        ev_completed_with_tokens("r2", 80),
    ]);
    let third_turn = sse(vec![
        ev_assistant_message("m4", FINAL_REPLY),
        ev_completed_with_tokens("r4", 1),
    ]);

    let responses = vec![
        sse_response(first_turn).insert_header("X-Reasoning-Included", "true"),
        sse_response(second_turn),
        sse_response(third_turn),
    ];
    mount_response_sequence(&server, responses).await;

    let compacted_history = vec![
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "REMOTE_COMPACT_SUMMARY".to_string(),
            }],
            end_turn: None,
        },
        TranscriptItem::Reasoning {
            id: "compact-summary".to_string(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("ENCRYPTED_COMPACTION_SUMMARY".to_string()),
        },
    ];
    let compact_mock =
        mount_compact_json_once(&server, serde_json::json!({ "output": compacted_history })).await;

    let codex = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_config(|config| {
            set_test_compact_prompt(config);
            config.model_auto_compact_token_limit = Some(300);
            config.features.enable(Feature::RemoteCompaction);
        })
        .build(&server)
        .await
        .expect("build codex")
        .codex;

    for user in [first_user, second_user, third_user] {
        codex
            .submit(Op::UserInput {
                items: vec![UserInput::Text {
                    text: user.into(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .await
            .unwrap();
        wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
    }

    let compact_requests = compact_mock.requests();
    assert_eq!(
        compact_requests.len(),
        1,
        "remote compaction should run once after the reasoning header clears"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_chat_model_upgrades_context_window_after_large_success() {
    skip_if_no_network!();

    let server = MockServer::start().await;
    let chat_seq = ChatSeqResponder {
        num_calls: AtomicUsize::new(0),
        bodies: vec![
            chat_sse_delta_and_stop("first reply"),
            chat_sse_delta_and_stop("second reply"),
        ],
    };
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_seq)
        .expect(2)
        .mount(&server)
        .await;

    let model_provider = unknown_chat_model_provider(&server);
    let large_input = "a".repeat(100_000);

    let codex = test_codex()
        .with_config(move |config| {
            config.model = Some("unknown-chat-model".to_string());
            config.model_provider = model_provider;
        })
        .build(&server)
        .await
        .expect("build codex")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: large_input,
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit large turn");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "follow up".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit follow-up turn");

    let model_context_window = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::TurnStarted(event) => Some(event.model_context_window),
        _ => None,
    })
    .await;
    assert_eq!(model_context_window, Some(60_800));

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_chat_model_upgrades_context_window_from_response_usage() {
    skip_if_no_network!();

    let server = MockServer::start().await;
    let chat_seq = ChatSeqResponder {
        num_calls: AtomicUsize::new(0),
        bodies: vec![
            chat_sse_delta_and_stop_with_tokens("first reply", 40_000),
            chat_sse_delta_and_stop("second reply"),
        ],
    };
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_seq)
        .expect(2)
        .mount(&server)
        .await;

    let model_provider = unknown_chat_model_provider(&server);

    let codex = test_codex()
        .with_config(move |config| {
            config.model = Some("unknown-chat-model".to_string());
            config.model_provider = model_provider;
        })
        .build(&server)
        .await
        .expect("build codex")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "small request".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit initial turn");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "follow up".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit follow-up turn");

    let model_context_window = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::TurnStarted(event) => Some(event.model_context_window),
        _ => None,
    })
    .await;
    assert_eq!(model_context_window, Some(60_800));

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_chat_model_adjacent_probe_failure_persists_learned_window() {
    skip_if_no_network!();

    let server = MockServer::start().await;
    let chat_seq = ChatSeqResponder {
        num_calls: AtomicUsize::new(0),
        bodies: vec![
            chat_sse_delta_and_stop(FINAL_REPLY),
            chat_sse_length(),
            chat_sse_delta_and_stop(AUTO_SUMMARY_TEXT),
            chat_sse_delta_and_stop(FINAL_REPLY),
            chat_sse_delta_and_stop(AUTO_SUMMARY_TEXT),
            chat_sse_delta_and_stop(FINAL_REPLY),
        ],
    };
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_seq)
        .expect(6)
        .mount(&server)
        .await;

    let model_provider = unknown_chat_model_provider(&server);
    let first_probe_input = "b".repeat(40_000);
    let second_probe_input = "c".repeat(80_000);
    let locked_follow_up_input = "d".repeat(40_000);

    let test = test_codex()
        .with_config(move |config| {
            config.model = Some("unknown-chat-model".to_string());
            config.model_provider = model_provider;
            set_test_compact_prompt(config);
        })
        .build(&server)
        .await
        .expect("build codex");
    let codex = test.codex.clone();

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: first_probe_input.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit first probe turn");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: second_probe_input.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit second probe turn");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: locked_follow_up_input.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submit locked follow-up turn");
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = server.received_requests().await.expect("capture requests");
    let chat_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/chat/completions")
        .collect::<Vec<_>>();
    assert_eq!(
        chat_requests.len(),
        6,
        "expected one successful probe, one failed adjacent probe with compact retry, then preflight compact after locking"
    );

    let first_probe_body = String::from_utf8_lossy(&chat_requests[0].body);
    assert!(
        body_contains_text(&first_probe_body, &first_probe_input),
        "first request should probe optimistically at the initial step"
    );
    assert!(
        !body_contains_text(&first_probe_body, SUMMARIZATION_PROMPT),
        "first request should not preflight compact"
    );

    let second_probe_body = String::from_utf8_lossy(&chat_requests[1].body);
    assert!(
        body_contains_text(&second_probe_body, &second_probe_input),
        "second request should probe the adjacent next step"
    );
    assert!(
        !body_contains_text(&second_probe_body, SUMMARIZATION_PROMPT),
        "second request should still be sent before compacting"
    );

    let compact_body = String::from_utf8_lossy(&chat_requests[2].body);
    assert!(
        body_contains_text(&compact_body, SUMMARIZATION_PROMPT),
        "failed adjacent probe should trigger compaction"
    );

    let retry_body = String::from_utf8_lossy(&chat_requests[3].body);
    assert!(
        body_contains_text(&retry_body, AUTO_SUMMARY_TEXT),
        "retry request should include the compacted summary"
    );

    let second_compact_body = String::from_utf8_lossy(&chat_requests[4].body);
    assert!(
        body_contains_text(&second_compact_body, SUMMARIZATION_PROMPT),
        "future large turns should preflight compact after the learned window is locked"
    );
    assert!(
        body_contains_text(&second_compact_body, &locked_follow_up_input),
        "preflight compact should include the follow-up input once the learned window is known"
    );

    let second_retry_body = String::from_utf8_lossy(&chat_requests[5].body);
    assert!(
        body_contains_text(&second_retry_body, AUTO_SUMMARY_TEXT),
        "post-preflight retry should include the compacted summary"
    );

    let raw_models = tokio::fs::read_to_string(test.lha_home_path().join("models.json"))
        .await
        .expect("read models.json");
    assert!(
        raw_models.contains("32000"),
        "learned context window should be persisted to models.json"
    );
}
