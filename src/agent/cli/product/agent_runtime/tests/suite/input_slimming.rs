#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::product::agent::features::Feature;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::SandboxPolicy;
use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_function_call;
use crate::test_support::core::responses::ev_response_created;
use crate::test_support::core::responses::mount_sse_once;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::start_mock_server;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event_match;
use anyhow::Result;
use serde_json::Value;
use serde_json::json;

fn body_text(body: &Value) -> String {
    serde_json::to_string(body).expect("request body serializes")
}

fn tool_identifiers(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .or_else(|| tool.get("type").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect()
}

fn extract_input_slimming_hash(text: &str) -> String {
    let prefix = "<<lha-input:";
    let start = text.find(prefix).expect("marker should exist") + prefix.len();
    let end = text[start..].find(">>").expect("marker should close") + start;
    text[start..end].to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_slimming_slims_live_and_historical_tool_outputs() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "large-output";
    let command = "for i in $(seq 1 5000); do echo old-tool-line-$i; done";
    let args = json!({
        "command": command,
        "timeout_ms": 5_000,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let same_turn_follow_up = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "first turn done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;
    let second_turn = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-2", "second turn done"),
            ev_completed("resp-3"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.1").with_config(|config| {
        config.features.enable(Feature::InputSlimming);
        config.tool_output_token_limit = Some(100_000);
    });
    let test = builder.build(&server).await?;

    test.submit_turn_with_policy("make a large shell output", SandboxPolicy::DangerFullAccess)
        .await?;
    let same_turn_slimming = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::InputSlimming(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    assert!(same_turn_slimming.last.tokens_before > same_turn_slimming.last.tokens_after);
    assert_eq!(
        same_turn_slimming.last.tokens_saved,
        same_turn_slimming
            .last
            .tokens_before
            .saturating_sub(same_turn_slimming.last.tokens_after)
    );
    assert!(same_turn_slimming.last.replacements > 0);
    assert_eq!(
        same_turn_slimming.total.tokens_saved,
        same_turn_slimming.last.tokens_saved
    );

    let same_turn_body = same_turn_follow_up.single_request().body_json();
    let same_turn_text = body_text(&same_turn_body);
    assert!(same_turn_text.contains("<<lha-input:"));
    assert!(same_turn_text.contains("Input Slimming"));
    assert!(
        tool_identifiers(&same_turn_body)
            .iter()
            .any(|tool| tool == "lha_input_retrieve")
    );
    assert!(!same_turn_text.contains("old-tool-line-2500"));

    test.submit_turn("now inspect the previous output").await?;
    let no_new_slimming_event =
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            wait_for_event_match(&test.codex, |event| match event {
                EventMsg::InputSlimming(event) => Some(event.clone()),
                _ => None,
            })
            .await
        })
        .await;
    assert!(
        no_new_slimming_event.is_err(),
        "reusing the same occurrence should not emit a second context saved event"
    );

    let second_turn_body = second_turn.single_request().body_json();
    let second_turn_text = body_text(&second_turn_body);

    assert!(second_turn_text.contains("<<lha-input:"));
    assert!(second_turn_text.contains("Input Slimming"));
    assert!(
        tool_identifiers(&second_turn_body)
            .iter()
            .any(|tool| tool == "lha_input_retrieve")
    );
    assert!(!second_turn_text.contains("old-tool-line-2500"));

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let rollout = tokio::fs::read_to_string(&rollout_path).await?;
    assert!(rollout.contains("old-tool-line-2500"));
    assert!(!rollout.contains("<<lha-input:"));
    assert!(rollout.contains("input_slimming_stored_input"));

    let hash = extract_input_slimming_hash(&second_turn_text);
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-4"),
            ev_function_call(
                "retrieve-call",
                "lha_input_retrieve",
                &serde_json::to_string(&json!({
                    "hash": hash.clone(),
                    "query": "old-tool-line-2500",
                }))?,
            ),
            ev_completed("resp-4"),
        ]),
    )
    .await;
    let after_retrieve = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-3", "retrieved original line"),
            ev_completed("resp-5"),
        ]),
    )
    .await;

    test.submit_turn("retrieve old-tool-line-2500").await?;
    let retrieve_follow_up = after_retrieve.single_request().body_json();
    let retrieve_follow_up_text = body_text(&retrieve_follow_up);
    assert!(retrieve_follow_up_text.contains("old-tool-line-2500"));
    assert!(!retrieve_follow_up_text.contains("store miss"));

    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-4", "marker noted"),
            ev_completed("resp-6"),
        ]),
    )
    .await;
    test.submit_turn(&format!("remember marker <<lha-input:{hash}>>"))
        .await?;

    let resumed_retrieve = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-7"),
            ev_function_call(
                "resume-retrieve-call",
                "lha_input_retrieve",
                &serde_json::to_string(&json!({
                    "hash": hash.clone(),
                    "query": "old-tool-line-2500",
                }))?,
            ),
            ev_completed("resp-7"),
        ]),
    )
    .await;
    let resumed_after_retrieve = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-5", "resumed retrieval worked"),
            ev_completed("resp-8"),
        ]),
    )
    .await;
    let mut resume_builder = test_codex().with_model("gpt-5.1").with_config(|config| {
        config.features.enable(Feature::InputSlimming);
        config.tool_output_token_limit = Some(64);
    });
    let resumed = resume_builder
        .resume(&server, test.home.clone(), rollout_path)
        .await?;

    resumed
        .submit_turn("retrieve the remembered marker")
        .await?;
    let resumed_body = resumed_retrieve.single_request().body_json();
    let resumed_text = body_text(&resumed_body);
    assert!(resumed_text.contains(&format!("<<lha-input:{hash}>>")));
    assert!(
        tool_identifiers(&resumed_body)
            .iter()
            .any(|tool| tool == "lha_input_retrieve")
    );
    let resumed_follow_up = resumed_after_retrieve.single_request().body_json();
    let resumed_follow_up_text = body_text(&resumed_follow_up);
    assert!(resumed_follow_up_text.contains("old-tool-line-2500"));
    assert!(!resumed_follow_up_text.contains("store miss"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_slimming_feature_disabled_keeps_old_tool_output_unmodified() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "large-output-disabled";
    let command = "for i in $(seq 1 5000); do echo disabled-tool-line-$i; done";
    let args = json!({
        "command": command,
        "timeout_ms": 5_000,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "first turn done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;
    let second_turn = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-2", "second turn done"),
            ev_completed("resp-3"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.1").with_config(|config| {
        config.features.disable(Feature::InputSlimming);
        config.tool_output_token_limit = Some(100_000);
    });
    let test = builder.build(&server).await?;

    test.submit_turn_with_policy("make a large shell output", SandboxPolicy::DangerFullAccess)
        .await?;
    test.submit_turn("now inspect the previous output").await?;

    let second_turn_body = second_turn.single_request().body_json();
    let second_turn_text = body_text(&second_turn_body);

    assert!(second_turn_text.contains("disabled-tool-line-2500"));
    assert!(!second_turn_text.contains("<<lha-input:"));
    assert!(
        !tool_identifiers(&second_turn_body)
            .iter()
            .any(|tool| tool == "lha_input_retrieve")
    );
    assert!(!Feature::InputSlimming.default_enabled());

    Ok(())
}
