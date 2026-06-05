use crate::product::agent::AuthManager;
use crate::product::agent::CodexAuth;
use crate::product::agent::ContentItem;
use crate::product::agent::default_client::originator;
use crate::product::agent::error::CodexErr;
use crate::product::agent::models_manager::manager::ModelsManager;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ItemCompletedEvent;
use crate::product::agent::protocol::ItemStartedEvent;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::SessionSource;
use crate::product::otel::OtelManager;
use crate::product::protocol::ThreadId;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::ReasoningSummary;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::config_types::Verbosity;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::ReasoningItemContent;
use crate::product::protocol::models::ReasoningItemReasoningSummary;
use crate::product::protocol::models::ToolResultPayload;
use crate::product::protocol::models::TranscriptItem;
use crate::product::protocol::models::WebSearchAction;
use crate::product::protocol::openai_models::ReasoningEffort;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::load_default_config_for_test;
use crate::test_support::core::load_sse_fixture_with_id;
use crate::test_support::core::responses::mount_sse_once;
use crate::test_support::core::responses::mount_sse_once_match;
use crate::test_support::core::responses::mount_sse_sequence;
use crate::test_support::core::responses::sse_failed;
use crate::test_support::core::runtime_client::TestRuntimeClient;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::TestCodex;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use crate::test_support::core::wait_for_event_match;
use dunce::canonicalize as normalize_path;
use futures::StreamExt;
use lha_llm::RuntimeEndpoint;
use lha_llm::ToolCallPayload;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;

/// Build minimal SSE stream with completed marker using the JSON fixture.
fn sse_completed(id: &str) -> String {
    load_sse_fixture_with_id("../fixtures/completed_template.json", id)
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
        .unwrap_or_else(|err| panic!("serialize Messages start event: {err}")),
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
        .unwrap_or_else(|err| panic!("serialize Messages text delta: {err}")),
    ));
    body.push_str("event: message_delta\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": output_tokens },
        }))
        .unwrap_or_else(|err| panic!("serialize Messages delta event: {err}")),
    ));
    body.push_str("event: message_stop\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "type": "message_stop",
        }))
        .unwrap_or_else(|err| panic!("serialize Messages stop event: {err}")),
    ));
    body
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

#[allow(clippy::unwrap_used)]
fn assert_message_role(request_body: &serde_json::Value, role: &str) {
    assert_eq!(request_body["role"].as_str().unwrap(), role);
}

#[allow(clippy::expect_used)]
fn assert_message_equals(request_body: &serde_json::Value, text: &str) {
    let content = request_body["content"][0]["text"]
        .as_str()
        .expect("invalid message content");

    assert_eq!(
        content, text,
        "expected message content '{content}' to equal '{text}'"
    );
}

#[allow(clippy::expect_used)]
fn assert_message_starts_with(request_body: &serde_json::Value, text: &str) {
    let content = request_body["content"][0]["text"]
        .as_str()
        .expect("invalid message content");

    assert!(
        content.starts_with(text),
        "expected message content '{content}' to start with '{text}'"
    );
}

#[allow(clippy::expect_used)]
fn assert_message_ends_with(request_body: &serde_json::Value, text: &str) {
    let content = request_body["content"][0]["text"]
        .as_str()
        .expect("invalid message content");

    assert!(
        content.ends_with(text),
        "expected message content '{content}' to end with '{text}'"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_includes_initial_messages_and_sends_prior_items() {
    skip_if_no_network!();

    // Create a fake rollout session file with prior user + system + assistant messages.
    let tmpdir = TempDir::new().unwrap();
    let session_path = tmpdir.path().join("resume-session.jsonl");
    let mut f = std::fs::File::create(&session_path).unwrap();
    let convo_id = Uuid::new_v4();
    writeln!(
        f,
        "{}",
        json!({
            "timestamp": "2024-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": convo_id,
                "timestamp": "2024-01-01T00:00:00Z",
                "instructions": "be nice",
                "cwd": ".",
                "originator": "test_originator",
                "cli_version": "test_version",
                "model_provider": "test-provider"
            }
        })
    )
    .unwrap();

    // Prior item: user message (should be delivered)
    let prior_user = TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![crate::product::protocol::models::ContentItem::InputText {
            text: "resumed user message".to_string(),
        }],
        end_turn: None,
    };
    let prior_user_json = serde_json::to_value(&prior_user).unwrap();
    writeln!(
        f,
        "{}",
        json!({
            "timestamp": "2024-01-01T00:00:01.000Z",
            "type": "transcript_item",
            "payload": prior_user_json
        })
    )
    .unwrap();

    // Prior item: system message (excluded from API history)
    let prior_system = TranscriptItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![crate::product::protocol::models::ContentItem::OutputText {
            text: "resumed system instruction".to_string(),
        }],
        end_turn: None,
    };
    let prior_system_json = serde_json::to_value(&prior_system).unwrap();
    writeln!(
        f,
        "{}",
        json!({
            "timestamp": "2024-01-01T00:00:02.000Z",
            "type": "transcript_item",
            "payload": prior_system_json
        })
    )
    .unwrap();

    // Prior item: assistant message
    let prior_item = TranscriptItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![crate::product::protocol::models::ContentItem::OutputText {
            text: "resumed assistant message".to_string(),
        }],
        end_turn: None,
    };
    let prior_item_json = serde_json::to_value(&prior_item).unwrap();
    writeln!(
        f,
        "{}",
        json!({
            "timestamp": "2024-01-01T00:00:03.000Z",
            "type": "transcript_item",
            "payload": prior_item_json
        })
    )
    .unwrap();
    drop(f);

    // Mock server that will receive the resumed request
    let server = MockServer::start().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;

    // Configure LHA to resume from our file
    let lha_home = Arc::new(TempDir::new().unwrap());
    let mut builder = test_codex()
        .with_home(lha_home.clone())
        .with_config(|config| {
            // Ensure user instructions are NOT delivered on resume.
            config.user_instructions = Some("be nice".to_string());
        });
    let test = builder
        .resume(&server, lha_home, session_path.clone())
        .await
        .expect("resume conversation");
    let codex = test.codex.clone();
    let session_configured = test.session_configured;

    // 1) Assert initial_messages only includes existing EventMsg entries; response items are not converted
    let initial_msgs = session_configured
        .initial_messages
        .clone()
        .expect("expected initial messages option for resumed session");
    let initial_json = serde_json::to_value(&initial_msgs).unwrap();
    let expected_initial_json = json!([]);
    assert_eq!(initial_json, expected_initial_json);

    // 2) Submit new input; the request body must include the prior items, then initial context, then new user input.
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();
    let input = request_body["input"].as_array().expect("input array");
    let messages: Vec<(String, String)> = input
        .iter()
        .filter_map(|item| {
            let role = item.get("role")?.as_str()?;
            let text = item
                .get("content")?
                .as_array()?
                .first()?
                .get("text")?
                .as_str()?;
            Some((role.to_string(), text.to_string()))
        })
        .collect();
    let pos_prior_user = messages
        .iter()
        .position(|(role, text)| role == "user" && text == "resumed user message")
        .expect("prior user message");
    let pos_prior_assistant = messages
        .iter()
        .position(|(role, text)| role == "assistant" && text == "resumed assistant message")
        .expect("prior assistant message");
    let pos_permissions = messages
        .iter()
        .position(|(role, text)| role == "developer" && text.contains("<permissions instructions>"))
        .expect("permissions message");
    let pos_user_instructions = messages
        .iter()
        .position(|(role, text)| {
            role == "user"
                && text.contains("be nice")
                && (text.starts_with("# AGENTS.md instructions for ")
                    || text.starts_with("<user_instructions>"))
        })
        .expect("user instructions");
    let pos_environment = messages
        .iter()
        .position(|(role, text)| role == "user" && text.contains("<environment_context>"))
        .expect("environment context");
    let pos_new_user = messages
        .iter()
        .position(|(role, text)| role == "user" && text == "hello")
        .expect("new user message");

    assert!(pos_prior_user < pos_prior_assistant);
    assert!(pos_prior_assistant < pos_permissions);
    assert!(pos_permissions < pos_user_instructions);
    assert!(pos_user_instructions < pos_environment);
    assert!(pos_environment < pos_new_user);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_conversation_id_and_model_headers_in_request() {
    skip_if_no_network!();

    // Mock server
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;

    let mut builder = test_codex().with_auth(CodexAuth::from_api_key("Test API Key"));
    let test = builder
        .build(&server)
        .await
        .expect("create new conversation");
    let codex = test.codex.clone();
    let session_id = test.session_configured.session_id;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let request_session_id = request.header("session_id").expect("session_id header");
    let request_authorization = request
        .header("authorization")
        .expect("authorization header");
    let request_originator = request.header("originator").expect("originator header");

    assert_eq!(request_session_id, session_id.to_string());
    assert_eq!(request_originator, originator().value);
    assert_eq!(request_authorization, "Bearer Test API Key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_base_instructions_override_in_request() {
    skip_if_no_network!();
    // Mock server
    let server = MockServer::start().await;
    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_config(|config| {
            config.base_instructions = Some("test instructions".to_string());
        });
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert!(
        request_body["instructions"]
            .as_str()
            .unwrap()
            .contains("test instructions")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_user_instructions_message_in_request() {
    skip_if_no_network!();
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_config(|config| {
            config.user_instructions = Some("be nice".to_string());
        });
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert!(
        !request_body["instructions"]
            .as_str()
            .unwrap()
            .contains("be nice")
    );
    assert_message_role(&request_body["input"][0], "developer");
    let permissions_text = request_body["input"][0]["content"][0]["text"]
        .as_str()
        .expect("invalid permissions message content");
    assert!(
        permissions_text.contains("`sandbox_mode`"),
        "expected permissions message to mention sandbox_mode, got {permissions_text:?}"
    );

    assert_message_role(&request_body["input"][1], "user");
    assert_message_starts_with(&request_body["input"][1], "# AGENTS.md instructions for ");
    assert_message_ends_with(&request_body["input"][1], "</INSTRUCTIONS>");
    let ui_text = request_body["input"][1]["content"][0]["text"]
        .as_str()
        .expect("invalid message content");
    assert!(ui_text.contains("<INSTRUCTIONS>"));
    assert!(ui_text.contains("be nice"));
    assert_message_role(&request_body["input"][2], "user");
    assert_message_starts_with(&request_body["input"][2], "<environment_context>");
    assert_message_ends_with(&request_body["input"][2], "</environment_context>");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skills_append_to_instructions() {
    skip_if_no_network!();
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;

    let lha_home = Arc::new(TempDir::new().unwrap());
    let skill_dir = lha_home.path().join("skills/demo");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: build charts\n---\n\n# body\n",
    )
    .expect("write skill");

    let lha_home_path = lha_home.path().to_path_buf();
    let mut builder = test_codex()
        .with_home(lha_home.clone())
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_config(move |config| {
            config.cwd = lha_home_path;
        });
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert_message_role(&request_body["input"][0], "developer");

    assert_message_role(&request_body["input"][1], "user");
    let instructions_text = request_body["input"][1]["content"][0]["text"]
        .as_str()
        .expect("instructions text");
    assert!(
        instructions_text.contains("## Skills"),
        "expected skills section present"
    );
    assert!(
        instructions_text.contains("demo: build charts"),
        "expected skill summary"
    );
    let expected_path = normalize_path(skill_dir.join("SKILL.md")).unwrap();
    let expected_path_str = expected_path.to_string_lossy().replace('\\', "/");
    assert!(
        instructions_text.contains(&expected_path_str),
        "expected path {expected_path_str} in instructions"
    );
    let _lha_home_guard = lha_home;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_configured_effort_in_request() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex()
        .with_model("gpt-5.1-codex")
        .with_config(|config| {
            config.model_reasoning_effort = Some(ReasoningEffort::Medium);
        })
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert_eq!(
        request_body
            .get("reasoning")
            .and_then(|t| t.get("effort"))
            .and_then(|v| v.as_str()),
        Some("medium")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_no_effort_in_request() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex()
        .with_model("gpt-5.1-codex")
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert_eq!(
        request_body
            .get("reasoning")
            .and_then(|t| t.get("effort"))
            .and_then(|v| v.as_str()),
        Some("medium")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_default_reasoning_effort_in_request_when_defined_by_model_info()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex().with_model("gpt-5.1").build(&server).await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert_eq!(
        request_body
            .get("reasoning")
            .and_then(|t| t.get("effort"))
            .and_then(|v| v.as_str()),
        Some("medium")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_identity_overrides_model_and_effort() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex {
        codex,
        config,
        session_configured,
        ..
    } = test_codex()
        .with_model("gpt-5.1-codex")
        .build(&server)
        .await?;

    let identity = Identity {
        kind: IdentityKind::Nobody,
        settings: Settings {
            model: "gpt-5.1".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            cwd: config.cwd.clone(),
            approval_policy: config.approval_policy.value(),
            sandbox_policy: config.sandbox_policy.get().clone(),
            model: session_configured.model.clone(),
            effort: Some(ReasoningEffort::Low),
            summary: config.model_reasoning_summary,
            identity: Some(identity),
            final_output_json_schema: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request_body = resp_mock.single_request().body_json();
    assert_eq!(request_body["model"].as_str(), Some("gpt-5.1"));
    assert_eq!(
        request_body
            .get("reasoning")
            .and_then(|t| t.get("effort"))
            .and_then(|v| v.as_str()),
        Some("high")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_reasoning_summary_is_sent() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex()
        .with_config(|config| {
            config.model_reasoning_summary = ReasoningSummary::Concise;
        })
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    pretty_assertions::assert_eq!(
        request_body
            .get("reasoning")
            .and_then(|reasoning| reasoning.get("summary"))
            .and_then(|value| value.as_str()),
        Some("concise")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_summary_is_omitted_when_disabled() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex()
        .with_config(|config| {
            config.model_reasoning_summary = ReasoningSummary::None;
        })
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    pretty_assertions::assert_eq!(
        request_body
            .get("reasoning")
            .and_then(|reasoning| reasoning.get("summary")),
        None
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_default_verbosity_in_request() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex().with_model("gpt-5.1").build(&server).await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert_eq!(
        request_body
            .get("text")
            .and_then(|t| t.get("verbosity"))
            .and_then(|v| v.as_str()),
        Some("low")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_verbosity_not_sent_for_models_without_support() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex()
        .with_model("gpt-5.1-codex")
        .with_config(|config| {
            config.model_verbosity = Some(Verbosity::High);
        })
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert!(
        request_body
            .get("text")
            .and_then(|t| t.get("verbosity"))
            .is_none()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_verbosity_is_sent() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let TestCodex { codex, .. } = test_codex()
        .with_model("gpt-5.1")
        .with_config(|config| {
            config.model_verbosity = Some(Verbosity::High);
        })
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    assert_eq!(
        request_body
            .get("text")
            .and_then(|t| t.get("verbosity"))
            .and_then(|v| v.as_str()),
        Some("high")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn includes_developer_instructions_message_in_request() {
    skip_if_no_network!();
    let server = MockServer::start().await;

    let resp_mock = mount_sse_once(&server, sse_completed("resp1")).await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::from_api_key("Test API Key"))
        .with_config(|config| {
            config.user_instructions = Some("be nice".to_string());
            config.developer_instructions = Some("be useful".to_string());
        });
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = resp_mock.single_request();
    let request_body = request.body_json();

    let permissions_text = request_body["input"][0]["content"][0]["text"]
        .as_str()
        .expect("invalid permissions message content");

    assert!(
        !request_body["instructions"]
            .as_str()
            .unwrap()
            .contains("be nice")
    );
    assert_message_role(&request_body["input"][0], "developer");
    assert!(
        permissions_text.contains("`sandbox_mode`"),
        "expected permissions message to mention sandbox_mode, got {permissions_text:?}"
    );

    assert_message_role(&request_body["input"][1], "developer");
    assert_message_equals(&request_body["input"][1], "be useful");
    assert_message_role(&request_body["input"][2], "user");
    assert_message_starts_with(&request_body["input"][2], "# AGENTS.md instructions for ");
    assert_message_ends_with(&request_body["input"][2], "</INSTRUCTIONS>");
    let ui_text = request_body["input"][2]["content"][0]["text"]
        .as_str()
        .expect("invalid message content");
    assert!(ui_text.contains("<INSTRUCTIONS>"));
    assert!(ui_text.contains("be nice"));
    assert_message_role(&request_body["input"][3], "user");
    assert_message_starts_with(&request_body["input"][3], "<environment_context>");
    assert_message_ends_with(&request_body["input"][3], "</environment_context>");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_api_folds_developer_messages_into_system() {
    skip_if_no_network!();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(MessagesSeqResponder {
            num_calls: AtomicUsize::new(0),
            bodies: vec![messages_sse_text_and_stop_with_tokens(
                "msg_1",
                "hello back",
                120,
                24,
            )],
        })
        .expect(1)
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
            config.user_instructions = Some("be nice".to_string());
            config.developer_instructions = Some("be useful".to_string());
        })
        .build(&server)
        .await
        .expect("create new conversation");

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = server.received_requests().await.expect("capture requests");
    let request = requests
        .iter()
        .find(|request| request.url.path() == "/v1/messages")
        .expect("messages request");
    let request_body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("messages request should be json");

    let system = request_body["system"]
        .as_str()
        .expect("system should be a string");
    assert!(system.contains("<permissions instructions>"));
    assert!(system.contains("be useful"));

    let messages = request_body["messages"]
        .as_array()
        .expect("messages should be an array");
    assert!(messages.iter().all(|message| {
        matches!(
            message.get("role").and_then(serde_json::Value::as_str),
            Some("user" | "assistant")
        )
    }));

    let message_texts: Vec<(String, String)> = messages
        .iter()
        .filter_map(|message| {
            let role = message.get("role")?.as_str()?.to_string();
            let text = message
                .get("content")?
                .as_array()?
                .first()?
                .get("text")?
                .as_str()?
                .to_string();
            Some((role, text))
        })
        .collect();

    assert!(message_texts.iter().any(|(role, text)| {
        role == "user"
            && text.contains("be nice")
            && (text.starts_with("# AGENTS.md instructions for ")
                || text.starts_with("<user_instructions>"))
    }));
    assert!(
        message_texts
            .iter()
            .any(|(role, text)| role == "user" && text.contains("<environment_context>"))
    );
    assert!(
        message_texts
            .iter()
            .any(|(role, text)| role == "user" && text == "hello")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_api_streamed_agent_message_reuses_item_id() {
    skip_if_no_network!();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(MessagesSeqResponder {
            num_calls: AtomicUsize::new(0),
            bodies: vec![messages_sse_text_and_stop_with_tokens(
                "msg_1",
                "hello back",
                120,
                24,
            )],
        })
        .expect(1)
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
        })
        .build(&server)
        .await
        .expect("create new conversation");

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    let started_item = wait_for_event_match(&test.codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    let delta_event = wait_for_event_match(&test.codex, |ev| match ev {
        EventMsg::AgentMessageContentDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let legacy_delta = wait_for_event_match(&test.codex, |ev| match ev {
        EventMsg::AgentMessageDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let completed_item = wait_for_event_match(&test.codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    assert_eq!(started_item.id, "msg_1");
    assert_eq!(delta_event.item_id, started_item.id);
    assert_eq!(legacy_delta.delta, "hello back");
    assert_eq!(completed_item.id, started_item.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_api_filters_unsupported_freeform_tools() {
    skip_if_no_network!();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(MessagesSeqResponder {
            num_calls: AtomicUsize::new(0),
            bodies: vec![messages_sse_text_and_stop_with_tokens(
                "msg_1",
                "hello back",
                120,
                24,
            )],
        })
        .expect(1)
        .mount(&server)
        .await;

    let mut model_provider = RuntimeEndpoint::openai();
    model_provider.base_url = Some(format!("{}/v1", server.uri()));
    model_provider.env_key = Some("PATH".into());
    model_provider.set_realtime_turn_streaming_enabled(false);
    model_provider.set_message_turns();

    let test = test_codex()
        .with_model("gpt-5.1-codex")
        .with_config(move |config| {
            config.model_provider = model_provider;
            config.web_search_mode =
                Some(crate::product::protocol::config_types::WebSearchMode::Disabled);
        })
        .build(&server)
        .await
        .expect("create new conversation");

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = server.received_requests().await.expect("capture requests");
    let request = requests
        .iter()
        .find(|request| request.url.path() == "/v1/messages")
        .expect("messages request");
    let request_body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("messages request should be json");

    let tools = request_body
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        tools.iter().all(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && tool.get("input_schema").is_some()
        }),
        "messages request should only include function-tool payloads"
    );
    assert!(
        !tools.iter().any(|tool| {
            tool.get("name").and_then(serde_json::Value::as_str) == Some("apply_patch")
        }),
        "messages request should filter out freeform apply_patch"
    );

    if tools.is_empty() {
        assert!(
            request_body.get("tool_choice").is_none(),
            "messages request without tools should omit tool_choice"
        );
    } else {
        assert_eq!(
            request_body["tool_choice"]["type"],
            serde_json::Value::String("auto".to_string())
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn azure_responses_request_includes_store_and_reasoning_ids() {
    skip_if_no_network!();

    let server = MockServer::start().await;

    let sse_body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n",
    );
    let resp_mock = mount_sse_once(&server, sse_body.to_string()).await;

    let provider =
        RuntimeEndpoint::openai_compatible_responses("azure", format!("{}/openai", server.uri()))
            .with_request_max_retries(Some(0))
            .with_stream_max_retries(Some(0))
            .with_stream_idle_timeout_ms(Some(5_000));

    let lha_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&lha_home).await;
    config.model_provider_id = provider.name.clone();
    config.model_provider = provider.clone();
    let effort = config.model_reasoning_effort;
    let summary = config.model_reasoning_summary;
    let model = ModelsManager::get_model_offline(config.model.as_deref());
    config.model = Some(model.clone());
    let config = Arc::new(config);
    let model_info = ModelsManager::construct_model_info_offline(model.as_str(), &config);
    let conversation_id = ThreadId::new();
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let otel_manager = OtelManager::new(
        conversation_id,
        model.as_str(),
        model_info.slug.as_str(),
        None,
        Some("test@test.com".to_string()),
        auth_manager.get_auth_mode(),
        false,
        "test".to_string(),
        SessionSource::Exec,
    );

    let mut client = TestRuntimeClient::new(
        Arc::clone(&config),
        None,
        model_info,
        otel_manager,
        provider,
        effort,
        summary,
        conversation_id,
        SessionSource::Exec,
    )
    .new_session();

    let mut turn = TurnRequest::default();
    turn.conversation.push(TranscriptItem::Reasoning {
        id: "reasoning-id".into(),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "summary".into(),
        }],
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: "content".into(),
        }]),
        encrypted_content: None,
    });
    turn.conversation.push(TranscriptItem::Message {
        id: Some("message-id".into()),
        role: "assistant".into(),
        content: vec![ContentItem::OutputText {
            text: "message".into(),
        }],
        end_turn: None,
    });
    turn.conversation.push(TranscriptItem::HostedActivity {
        id: Some("web-search-id".into()),
        activity_type: "web_search".into(),
        status: Some("completed".into()),
        payload: serde_json::to_value(WebSearchAction::Search {
            query: Some("weather".into()),
            queries: None,
        })
        .expect("serialize web search action"),
    });
    turn.conversation.push(TranscriptItem::ToolCall {
        id: Some("function-id".into()),
        call_id: "function-call-id".into(),
        tool_name: "do_thing".into(),
        payload: ToolCallPayload::JsonArguments {
            arguments: "{}".into(),
        },
    });
    turn.conversation.push(TranscriptItem::ToolResult {
        call_id: "function-call-id".into(),
        tool_name: "do_thing".into(),
        payload: ToolResultPayload::Structured {
            content: "ok".into(),
            content_items: None,
            success: None,
        },
    });
    turn.conversation.push(TranscriptItem::ToolCall {
        id: Some("local-shell-id".into()),
        call_id: "local-shell-call-id".into(),
        tool_name: "local_shell".into(),
        payload: ToolCallPayload::JsonArguments {
            arguments: serde_json::json!({
                "command": ["echo", "hello"],
            })
            .to_string(),
        },
    });
    turn.conversation.push(TranscriptItem::ToolCall {
        id: Some("custom-tool-id".into()),
        call_id: "custom-tool-call-id".into(),
        tool_name: "custom_tool".into(),
        payload: ToolCallPayload::TextInput { input: "{}".into() },
    });
    turn.conversation.push(TranscriptItem::ToolResult {
        call_id: "custom-tool-call-id".into(),
        tool_name: "custom_tool".into(),
        payload: ToolResultPayload::Text {
            output: "ok".into(),
        },
    });

    let mut stream = client
        .run_turn(&turn)
        .await
        .expect("responses stream to start");

    while let Some(event) = stream.next().await {
        if let Ok(TurnEvent::Completed { .. }) = event {
            break;
        }
    }

    let request = resp_mock.single_request();
    assert_eq!(request.path(), "/openai/responses");
    let body = request.body_json();

    assert_eq!(body["store"], serde_json::Value::Bool(true));
    assert_eq!(body["stream"], serde_json::Value::Bool(true));
    assert_eq!(body["input"].as_array().map(Vec::len), Some(8));
    assert_eq!(body["input"][0]["id"].as_str(), Some("reasoning-id"));
    assert_eq!(body["input"][1]["id"].as_str(), Some("message-id"));
    assert_eq!(body["input"][2]["id"].as_str(), Some("web-search-id"));
    assert_eq!(body["input"][3]["id"].as_str(), Some("function-id"));
    assert_eq!(
        body["input"][4]["call_id"].as_str(),
        Some("function-call-id")
    );
    assert_eq!(body["input"][5]["id"].as_str(), Some("local-shell-id"));
    assert_eq!(body["input"][6]["id"].as_str(), Some("custom-tool-id"));
    assert_eq!(
        body["input"][7]["call_id"].as_str(),
        Some("custom-tool-call-id")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_error_emits_error_event() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    let response = ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "limit reached",
                "resets_at": 1704067242
            }
    }));

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;

    let mut builder = test_codex();
    let codex_fixture = builder.build(&server).await?;
    let codex = codex_fixture.codex.clone();

    let submission_id = codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .expect("submission should succeed while emitting usage limit error events");

    let error_event = wait_for_event(&codex, |msg| matches!(msg, EventMsg::Error(_))).await;
    let EventMsg::Error(error_event) = error_event else {
        unreachable!();
    };
    assert!(
        error_event.message.to_lowercase().contains("usage limit"),
        "unexpected error message for submission {submission_id}: {}",
        error_event.message
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_window_error_sets_total_tokens_to_model_window() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    const EFFECTIVE_CONTEXT_WINDOW: i64 = (272_000 * 95) / 100;

    mount_sse_once_match(
        &server,
        body_string_contains("trigger context window"),
        sse_failed(
            "resp_context_window",
            "context_length_exceeded",
            "Your input exceeds the context window of this model. Please adjust your input and try again.",
        ),
    )
    .await;

    mount_sse_once_match(
        &server,
        body_string_contains("seed turn"),
        sse_completed("resp_seed"),
    )
    .await;

    let TestCodex { codex, .. } = test_codex()
        .with_config(|config| {
            config.model = Some("gpt-5.1".to_string());
            config.model_context_window = Some(272_000);
        })
        .build(&server)
        .await?;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "seed turn".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "trigger context window".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let token_event = wait_for_event(&codex, |event| {
        matches!(
            event,
            EventMsg::TokenCount(payload)
                if payload.info.as_ref().is_some_and(|info| {
                    info.model_context_window == Some(info.total_token_usage.total_tokens)
                        && info.total_token_usage.total_tokens > 0
                })
        )
    })
    .await;

    let EventMsg::TokenCount(token_payload) = token_event else {
        unreachable!("wait_for_event returned unexpected event");
    };

    let info = token_payload
        .info
        .expect("token usage info present when context window is exceeded");

    assert_eq!(info.model_context_window, Some(EFFECTIVE_CONTEXT_WINDOW));
    assert_eq!(
        info.total_token_usage.total_tokens,
        EFFECTIVE_CONTEXT_WINDOW
    );

    let error_event = wait_for_event(&codex, |ev| matches!(ev, EventMsg::Error(_))).await;
    let expected_context_window_message = CodexErr::ContextWindowExceeded.to_string();
    assert!(
        matches!(
            error_event,
            EventMsg::Error(ref err) if err.message == expected_context_window_message
        ),
        "expected context window error; got {error_event:?}"
    );

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn azure_overrides_assign_properties_used_for_responses_url() {
    skip_if_no_network!();
    let existing_env_var_with_random_value = if cfg!(windows) { "USERNAME" } else { "USER" };

    // Mock server
    let server = MockServer::start().await;

    // First request – must NOT include `previous_response_id`.
    let first = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse_completed("resp1"), "text/event-stream");

    // Expect POST to /openai/responses with api-version query param
    Mock::given(method("POST"))
        .and(path("/openai/responses"))
        .and(query_param("api-version", "2025-04-01-preview"))
        .and(header_regex("Custom-Header", "Value"))
        .and(header_regex(
            "Authorization",
            format!(
                "Bearer {}",
                std::env::var(existing_env_var_with_random_value).unwrap()
            )
            .as_str(),
        ))
        .respond_with(first)
        .expect(1)
        .mount(&server)
        .await;

    let provider =
        RuntimeEndpoint::openai_compatible_responses("custom", format!("{}/openai", server.uri()))
            .with_env_key(Some(existing_env_var_with_random_value.to_string()))
            .with_query_params(Some(std::collections::HashMap::from([(
                "api-version".to_string(),
                "2025-04-01-preview".to_string(),
            )])))
            .with_http_headers(Some(std::collections::HashMap::from([(
                "Custom-Header".to_string(),
                "Value".to_string(),
            )])));

    // Init session
    let mut builder = test_codex()
        .with_auth(create_dummy_codex_auth())
        .with_config(move |config| {
            config.model_provider = provider;
        });
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_var_overrides_loaded_auth() {
    skip_if_no_network!();
    let existing_env_var_with_random_value = if cfg!(windows) { "USERNAME" } else { "USER" };

    // Mock server
    let server = MockServer::start().await;

    // First request – must NOT include `previous_response_id`.
    let first = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse_completed("resp1"), "text/event-stream");

    // Expect POST to /openai/responses with api-version query param
    Mock::given(method("POST"))
        .and(path("/openai/responses"))
        .and(query_param("api-version", "2025-04-01-preview"))
        .and(header_regex("Custom-Header", "Value"))
        .and(header_regex(
            "Authorization",
            format!(
                "Bearer {}",
                std::env::var(existing_env_var_with_random_value).unwrap()
            )
            .as_str(),
        ))
        .respond_with(first)
        .expect(1)
        .mount(&server)
        .await;

    let provider =
        RuntimeEndpoint::openai_compatible_responses("custom", format!("{}/openai", server.uri()))
            .with_env_key(Some(existing_env_var_with_random_value.to_string()))
            .with_query_params(Some(std::collections::HashMap::from([(
                "api-version".to_string(),
                "2025-04-01-preview".to_string(),
            )])))
            .with_http_headers(Some(std::collections::HashMap::from([(
                "Custom-Header".to_string(),
                "Value".to_string(),
            )])));

    // Init session
    let mut builder = test_codex()
        .with_auth(create_dummy_codex_auth())
        .with_config(move |config| {
            config.model_provider = provider;
        });
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

fn create_dummy_codex_auth() -> CodexAuth {
    CodexAuth::from_api_key("Test API Key")
}

/// Scenario:
/// - Turn 1: user sends U1; model streams deltas then a final assistant message A.
/// - Turn 2: user sends U2; model streams a delta then the same final assistant message A.
/// - Turn 3: user sends U3; model responds (same SSE again, not important).
///
/// We assert that the `input` sent on each turn contains the expected conversation history
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_dedupes_streamed_and_final_messages_across_turns() {
    // Skip under LHA sandbox network restrictions (mirrors other tests).
    skip_if_no_network!();

    // Mock server that will receive three sequential requests and return the same SSE stream
    // each time: a few deltas, then a final assistant message, then completed.
    let server = MockServer::start().await;

    // Build a small SSE stream with deltas and a final assistant message.
    // We emit the same body for all 3 turns; ids vary but are unused by assertions.
    let sse_raw = r##"[
        {"type":"response.output_item.added", "item":{
            "type":"message", "role":"assistant",
            "content":[{"type":"output_text","text":""}]
        }},
        {"type":"response.output_text.delta", "delta":"Hey "},
        {"type":"response.output_text.delta", "delta":"there"},
        {"type":"response.output_text.delta", "delta":"!\n"},
        {"type":"response.output_item.done", "item":{
            "type":"message", "role":"assistant",
            "content":[{"type":"output_text","text":"Hey there!\n"}]
        }},
        {"type":"response.completed", "response": {"id": "__ID__"}}
    ]"##;
    let sse1 = crate::test_support::core::load_sse_fixture_with_id_from_str(sse_raw, "resp1");

    let request_log = mount_sse_sequence(&server, vec![sse1.clone(), sse1.clone(), sse1]).await;

    let mut builder = test_codex().with_auth(CodexAuth::from_api_key("Test API Key"));
    let codex = builder
        .build(&server)
        .await
        .expect("create new conversation")
        .codex;

    // Turn 1: user sends U1; wait for completion.
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "U1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Turn 2: user sends U2; wait for completion.
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "U2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Turn 3: user sends U3; wait for completion.
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "U3".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Inspect the three captured requests.
    let requests = request_log.requests();
    assert_eq!(requests.len(), 3, "expected 3 requests (one per turn)");
    for request in &requests {
        assert_eq!(request.path(), "/v1/responses");
    }

    // Replace full-array compare with tail-only raw JSON compare using a single hard-coded value.
    let r3_tail_expected = json!([
        {
            "type": "message",
            "role": "user",
            "content": [{"type":"input_text","text":"U1"}]
        },
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type":"output_text","text":"Hey there!\n"}]
        },
        {
            "type": "message",
            "role": "user",
            "content": [{"type":"input_text","text":"U2"}]
        },
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type":"output_text","text":"Hey there!\n"}]
        },
        {
            "type": "message",
            "role": "user",
            "content": [{"type":"input_text","text":"U3"}]
        }
    ]);

    let r3_input_array = requests[2]
        .body_json()
        .get("input")
        .and_then(|v| v.as_array())
        .cloned()
        .expect("r3 missing input array");
    // skipping earlier context and developer messages
    let tail_len = r3_tail_expected.as_array().unwrap().len();
    let actual_tail = &r3_input_array[r3_input_array.len() - tail_len..];
    assert_eq!(
        serde_json::Value::Array(actual_tail.to_vec()),
        r3_tail_expected,
        "request 3 tail mismatch",
    );
}
