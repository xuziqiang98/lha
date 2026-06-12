#![cfg(not(target_os = "windows"))]

use crate::product::agent::features::Feature;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ItemCompletedEvent;
use crate::product::agent::protocol::ItemStartedEvent;
use crate::product::agent::protocol::Op;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::items::AgentMessageContent;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::memory_citation::MemoryCitation;
use crate::product::protocol::memory_citation::MemoryCitationEntry;
use crate::product::protocol::models::WebSearchAction;
use crate::product::protocol::protocol::RolloutItem;
use crate::product::protocol::protocol::RolloutLine;
use crate::product::protocol::user_input::ByteRange;
use crate::product::protocol::user_input::TextElement;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_message_item_added;
use crate::test_support::core::responses::ev_output_text_delta;
use crate::test_support::core::responses::ev_reasoning_item;
use crate::test_support::core::responses::ev_reasoning_item_added;
use crate::test_support::core::responses::ev_reasoning_summary_text_delta;
use crate::test_support::core::responses::ev_reasoning_text_delta;
use crate::test_support::core::responses::ev_response_created;
use crate::test_support::core::responses::ev_web_search_call_added_partial;
use crate::test_support::core::responses::ev_web_search_call_done;
use crate::test_support::core::responses::mount_sse_once;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::start_mock_server;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::TestCodex;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use crate::test_support::core::wait_for_event_match;
use anyhow::Ok;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;

const MEMORY_CITATION_ROLLOUT_ID: &str = "00000000-0000-0000-0000-000000000001";

fn memory_citation_block(note: &str) -> String {
    format!(
        "<oai-mem-citation>\n<citation_entries>\nMEMORY.md:1-2|note=[{note}]\n</citation_entries>\n<rollout_ids>\n{MEMORY_CITATION_ROLLOUT_ID}\n</rollout_ids>\n</oai-mem-citation>"
    )
}

fn expected_memory_citation(note: &str) -> MemoryCitation {
    MemoryCitation {
        entries: vec![MemoryCitationEntry {
            path: "MEMORY.md".into(),
            line_start: 1,
            line_end: 2,
            note: note.into(),
        }],
        rollout_ids: vec![MEMORY_CITATION_ROLLOUT_ID.into()],
    }
}

fn agent_message_text(item: &crate::product::protocol::items::AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

fn write_memory_summary(lha_home: &std::path::Path) {
    let memory_root = lha_home.join("memories");
    fs::create_dir_all(&memory_root)
        .unwrap_or_else(|err| panic!("failed to create memory dir: {err}"));
    fs::write(
        memory_root.join("memory_summary.md"),
        "Planner memory citations are available.",
    )
    .unwrap_or_else(|err| panic!("failed to write memory summary: {err}"));
}

async fn read_rollout_items(path: &Path) -> anyhow::Result<Vec<RolloutItem>> {
    let contents = tokio::fs::read_to_string(path).await?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<RolloutLine>(line)
                .map(|rollout_line| rollout_line.item)
                .map_err(Into::into)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_message_item_is_emitted() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex { codex, .. } = test_codex().build(&server).await?;

    let first_response = sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]);
    mount_sse_once(&server, first_response).await;

    let text_elements = vec![TextElement::new(
        ByteRange { start: 0, end: 6 },
        Some("<file>".into()),
    )];
    let expected_input = UserInput::Text {
        text: "please inspect sample.txt".into(),
        text_elements: text_elements.clone(),
    };

    codex
        .submit(Op::UserInput {
            items: vec![expected_input.clone()],
            final_output_json_schema: None,
        })
        .await?;

    let started_item = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::UserMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    let completed_item = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::UserMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    assert_eq!(started_item.id, completed_item.id);
    assert_eq!(started_item.content, vec![expected_input.clone()]);
    assert_eq!(completed_item.content, vec![expected_input]);

    let legacy_message = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::UserMessage(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    assert_eq!(legacy_message.message, "please inspect sample.txt");
    assert_eq!(legacy_message.text_elements, text_elements);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn assistant_message_item_is_emitted() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex { codex, .. } = test_codex().build(&server).await?;

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "all done"),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, first_response).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "please summarize results".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let started = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    let completed = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    assert_eq!(started.id, completed.id);
    let Some(crate::product::protocol::items::AgentMessageContent::Text { text }) =
        completed.content.first()
    else {
        panic!("expected agent message text content");
    };
    assert_eq!(text, "all done");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_item_is_emitted() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex { codex, .. } = test_codex().build(&server).await?;

    let reasoning_item = ev_reasoning_item(
        "reasoning-1",
        &["Consider inputs", "Compute output"],
        &["Detailed reasoning trace"],
    );

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        reasoning_item,
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, first_response).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "explain your reasoning".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let started = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::Reasoning(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    let completed = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::Reasoning(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    assert_eq!(started.id, completed.id);
    assert_eq!(
        completed.summary_text,
        vec!["Consider inputs".to_string(), "Compute output".to_string()]
    );
    assert_eq!(
        completed.raw_content,
        vec!["Detailed reasoning trace".to_string()]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_item_is_emitted() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex { codex, .. } = test_codex().build(&server).await?;

    let web_search_added = ev_web_search_call_added_partial("web-search-1", "in_progress");
    let web_search_done = ev_web_search_call_done("web-search-1", "completed", "weather seattle");

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        web_search_added,
        web_search_done,
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, first_response).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "find the weather".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let begin = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::WebSearchBegin(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let completed = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::WebSearch(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    assert_eq!(begin.call_id, "web-search-1");
    assert_eq!(completed.id, begin.call_id);
    assert_eq!(
        completed.action,
        WebSearchAction::Search {
            query: Some("weather seattle".to_string()),
            queries: None,
        }
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_message_content_delta_has_item_metadata() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta("streamed response"),
        ev_assistant_message("msg-1", "streamed response"),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "please stream text".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let (started_turn_id, started_item) = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            turn_id,
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some((turn_id.clone(), item.clone())),
        _ => None,
    })
    .await;

    let delta_event = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::AgentMessageContentDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let legacy_delta = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::AgentMessageDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let completed_item = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    let session_id = session_configured.session_id.to_string();
    assert_eq!(delta_event.thread_id, session_id);
    assert_eq!(delta_event.turn_id, started_turn_id);
    assert_eq!(delta_event.item_id, started_item.id);
    assert_eq!(delta_event.delta, "streamed response");
    assert_eq!(legacy_delta.delta, "streamed response");
    assert_eq!(completed_item.id, started_item.id);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_emits_plan_item_from_proposed_plan_block() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let plan_block = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>\n";
    let full_message = format!("Intro\n{plan_block}Outro");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let plan_delta = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::PlanDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;

    let plan_completed = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::Plan(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    assert_eq!(
        plan_delta.thread_id,
        session_configured.session_id.to_string()
    );
    assert_eq!(plan_delta.delta, "- Step 1\n- Step 2\n");
    assert_eq!(plan_completed.text, "- Step 1\n- Step 2\n");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_preserves_literal_plan_tags_inside_fenced_code_blocks() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let plan_text = concat!(
        "# Plan\n",
        "```text\n",
        "<proposed_plan>\n",
        "- Example Step\n",
        "</proposed_plan>\n",
        "```\n",
        "After code fence\n",
    );
    let full_message = format!("Intro\n<proposed_plan>\n{plan_text}</proposed_plan>\nOutro");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut plan_deltas = Vec::new();
    let mut plan_items = Vec::new();
    let mut legacy_agent_messages = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::AgentMessage(event) => {
                legacy_agent_messages.push(event.message);
            }
            EventMsg::PlanDelta(event) => {
                plan_deltas.push(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(agent_deltas.concat(), "Intro\nOutro");
    assert_eq!(plan_deltas.concat(), plan_text);
    assert_eq!(
        plan_items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec![plan_text]
    );
    assert_eq!(legacy_agent_messages, Vec::<String>::new());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_preserves_indented_literal_plan_tags_inside_plan() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let plan_text = concat!(
        "# Plan\n",
        "\n",
        "1. Case\n",
        "\n",
        "     ```text\n",
        "     Intro<proposed_plan>\n",
        "     # Inner\n",
        "     </proposed_plan>\n",
        "     ```\n",
        "\n",
        "## Tests\n",
        "- still inside plan\n",
    );
    let full_message = format!("<proposed_plan>\n{plan_text}</proposed_plan>\n");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut plan_items = Vec::new();
    let mut agent_messages = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item.text);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_messages.push(agent_message_text(&item));
            }
            EventMsg::AgentMessage(event) => {
                agent_messages.push(event.message);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(plan_items, vec![plan_text.to_string()]);
    assert!(
        plan_items[0].contains("## Tests\n- still inside plan\n"),
        "expected trailing plan content to stay inside plan: {:?}",
        plan_items[0]
    );
    assert!(
        plan_items[0].contains("     </proposed_plan>\n"),
        "expected indented literal close tag to stay inside plan: {:?}",
        plan_items[0]
    );
    assert_eq!(agent_messages, Vec::<String>::new());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_preserves_nested_literal_plan_tags_inside_plan() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let plan_text = concat!(
        "# Plan\n",
        "<proposed_plan>\n",
        "# Inner\n",
        "</proposed_plan>\n",
        "## Tests\n",
        "- still inside plan\n",
    );
    let full_message = format!("<proposed_plan>\n{plan_text}</proposed_plan>\n");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut plan_items = Vec::new();
    let mut agent_messages = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item.text);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_messages.push(agent_message_text(&item));
            }
            EventMsg::AgentMessage(event) => {
                agent_messages.push(event.message);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(plan_items, vec![plan_text.to_string()]);
    assert!(
        plan_items[0].contains("## Tests\n- still inside plan\n"),
        "expected trailing plan content to stay inside plan: {:?}",
        plan_items[0]
    );
    assert!(
        plan_items[0].contains("<proposed_plan>\n# Inner\n</proposed_plan>\n"),
        "expected nested literal plan tags to stay inside plan: {:?}",
        plan_items[0]
    );
    assert_eq!(agent_messages, Vec::<String>::new());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_accepts_deeply_indented_outer_plan_tags() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let plan_text = concat!("# Plan\n", "- still a plan\n");
    let full_message =
        format!("Intro\n    <proposed_plan>\n{plan_text}    </proposed_plan>\nOutro");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut plan_items = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item.text);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(agent_deltas.concat(), "Intro\nOutro");
    assert_eq!(plan_items, vec![plan_text.to_string()]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_strips_plan_from_agent_messages() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;
    let rollout_path = session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let plan_block = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>\n";
    let full_message = format!("Intro\n{plan_block}Outro");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut plan_delta = None;
    let mut agent_item = None;
    let mut plan_item = None;
    let mut legacy_agent_messages = Vec::new();

    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::AgentMessage(event) => {
                legacy_agent_messages.push(event.message);
            }
            EventMsg::PlanDelta(event) => {
                plan_delta = Some(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_item = Some(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_item = Some(item);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    let agent_text = agent_deltas.concat();
    assert_eq!(agent_text, "Intro\nOutro");
    assert_eq!(plan_delta.unwrap(), "- Step 1\n- Step 2\n");
    assert_eq!(plan_item.unwrap().text, "- Step 1\n- Step 2\n");
    assert_eq!(legacy_agent_messages, Vec::<String>::new());
    let agent_text_from_item: String = agent_item
        .unwrap()
        .content
        .iter()
        .map(|entry| match entry {
            AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect();
    assert_eq!(agent_text_from_item, "Intro\nOutro");
    let persisted_agent_messages = read_rollout_items(&rollout_path)
        .await?
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::AgentMessage(message)) => Some(message.message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(persisted_agent_messages, vec!["Intro\nOutro".to_string()]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_completes_streamed_intro_before_plan_item() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let full_message = "现在我给完整计划。\n<proposed_plan>\n# Plan\n- Step 1\n</proposed_plan>\n";
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(full_message),
        ev_assistant_message("msg-1", full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut completed_order = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                completed_order.push(("agent", agent_message_text(&item)));
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                completed_order.push(("plan", item.text));
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(
        completed_order,
        vec![
            ("agent", "现在我给完整计划。\n".to_string()),
            ("plan", "# Plan\n- Step 1\n".to_string()),
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_treats_inline_plan_open_tag_as_normal_text() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let full_message = "现在我给完整计划。<proposed_plan>\n# Plan\n</proposed_plan>\n";
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(full_message),
        ev_assistant_message("msg-1", full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut completed_agent_text = None;
    let mut saw_plan_item = false;
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                completed_agent_text = Some(agent_message_text(&item));
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(_),
                ..
            }) => {
                saw_plan_item = true;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(
        completed_agent_text,
        Some(full_message.to_string()),
        "inline proposed_plan tags should remain ordinary text"
    );
    assert!(
        !saw_plan_item,
        "inline proposed_plan tags must not create a plan"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_keeps_non_streamed_agent_message_visible() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let full_message = "Planner note without streaming.";
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_assistant_message("msg-1", full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut agent_items = Vec::new();
    let mut legacy_agent_messages = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::AgentMessage(event) => {
                legacy_agent_messages.push(event.message);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_items.push(item);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(agent_deltas, Vec::<String>::new());
    assert_eq!(legacy_agent_messages, vec![full_message.to_string()]);
    assert_eq!(
        agent_items
            .iter()
            .map(agent_message_text)
            .collect::<Vec<_>>(),
        vec![full_message.to_string()]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_hides_memory_citation_after_proposed_plan() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let citation = memory_citation_block("used plan context");
    let full_message = format!("<proposed_plan>\n- Step 1\n</proposed_plan>\n{citation}");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex()
        .with_pre_build_hook(write_memory_summary)
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
        })
        .build(&server)
        .await?;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut agent_items = Vec::new();
    let mut plan_deltas = Vec::new();
    let mut plan_items = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::PlanDelta(event) => {
                plan_deltas.push(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_items.push(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(plan_deltas, vec!["- Step 1\n".to_string()]);
    assert_eq!(
        plan_items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec!["- Step 1\n"]
    );
    assert_eq!(agent_deltas, Vec::<String>::new());
    assert!(agent_items.is_empty());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_attaches_memory_citation_to_visible_agent_message() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let note = "used plan context";
    let expected_citation = expected_memory_citation(note);
    let citation = memory_citation_block(note);
    let plan_block = "<proposed_plan>\n- Step 1\n</proposed_plan>\n";
    let full_message = format!("Intro\n{plan_block}Outro\n{citation}");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex()
        .with_pre_build_hook(write_memory_summary)
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
        })
        .build(&server)
        .await?;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut agent_items = Vec::new();
    let mut plan_deltas = Vec::new();
    let mut plan_items = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::PlanDelta(event) => {
                plan_deltas.push(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_items.push(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(agent_deltas.concat(), "Intro\nOutro");
    assert_eq!(plan_deltas, vec!["- Step 1\n".to_string()]);
    assert_eq!(
        plan_items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec!["- Step 1\n"]
    );
    assert_eq!(agent_items.len(), 1);
    let agent_item = agent_items.pop().expect("agent message");
    assert_eq!(agent_message_text(&agent_item), "Intro\nOutro");
    assert_eq!(agent_item.memory_citation, Some(expected_citation));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_strips_memory_citation_inside_proposed_plan() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let citation = memory_citation_block("used plan context");
    let full_message = format!("<proposed_plan>\n- Step {citation}\n</proposed_plan>\n");
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-1", &full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex()
        .with_pre_build_hook(write_memory_summary)
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
        })
        .build(&server)
        .await?;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut agent_deltas = Vec::new();
    let mut agent_items = Vec::new();
    let mut plan_deltas = Vec::new();
    let mut plan_items = Vec::new();
    loop {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::AgentMessageContentDelta(event) => {
                agent_deltas.push(event.delta);
            }
            EventMsg::PlanDelta(event) => {
                plan_deltas.push(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_items.push(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_items.push(item);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(plan_deltas, vec!["- Step \n".to_string()]);
    assert_eq!(
        plan_items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec!["- Step \n"]
    );
    assert_eq!(agent_deltas, Vec::<String>::new());
    assert!(agent_items.is_empty());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_handles_missing_plan_close_tag() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex {
        codex,
        session_configured,
        ..
    } = test_codex().build(&server).await?;

    let full_message = "Intro\n<proposed_plan>\n- Step 1\n";
    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_message_item_added("msg-1", ""),
        ev_output_text_delta(full_message),
        ev_assistant_message("msg-1", full_message),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: session_configured.model.clone(),
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
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut plan_delta = None;
    let mut plan_item = None;
    let mut agent_item = None;

    while plan_delta.is_none() || plan_item.is_none() || agent_item.is_none() {
        let ev = wait_for_event(&codex, |_| true).await;
        match ev {
            EventMsg::PlanDelta(event) => {
                plan_delta = Some(event.delta);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => {
                plan_item = Some(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(item),
                ..
            }) => {
                agent_item = Some(item);
            }
            _ => {}
        }
    }

    assert_eq!(plan_delta.unwrap(), "- Step 1\n");
    assert_eq!(plan_item.unwrap().text, "- Step 1\n");
    let agent_text_from_item: String = agent_item
        .unwrap()
        .content
        .iter()
        .map(|entry| match entry {
            AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect();
    assert_eq!(agent_text_from_item, "Intro\n");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_content_delta_has_item_metadata() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex { codex, .. } = test_codex().build(&server).await?;

    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_reasoning_item_added("reasoning-1", &[""]),
        ev_reasoning_summary_text_delta("step one"),
        ev_reasoning_item("reasoning-1", &["step one"], &[]),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "reason through it".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let reasoning_item = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::Reasoning(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    let delta_event = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ReasoningContentDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let legacy_delta = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::AgentReasoningDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;

    assert_eq!(delta_event.item_id, reasoning_item.id);
    assert_eq!(delta_event.delta, "step one");
    assert_eq!(legacy_delta.delta, "step one");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_raw_content_delta_respects_flag() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let TestCodex { codex, .. } = test_codex()
        .with_config(|config| {
            config.show_raw_agent_reasoning = true;
        })
        .build(&server)
        .await?;

    let stream = sse(vec![
        ev_response_created("resp-1"),
        ev_reasoning_item_added("reasoning-raw", &[""]),
        ev_reasoning_text_delta("raw detail"),
        ev_reasoning_item("reasoning-raw", &["complete"], &["raw detail"]),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, stream).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "show raw reasoning".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let reasoning_item = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::Reasoning(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;

    let delta_event = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::ReasoningRawContentDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    let legacy_delta = wait_for_event_match(&codex, |ev| match ev {
        EventMsg::AgentReasoningRawContentDelta(event) => Some(event.clone()),
        _ => None,
    })
    .await;

    assert_eq!(delta_event.item_id, reasoning_item.id);
    assert_eq!(delta_event.delta, "raw detail");
    assert_eq!(legacy_delta.delta, "raw detail");

    Ok(())
}
