//! Verifies that the agent retries when the SSE stream terminates before
//! delivering a `response.completed` event.

use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::Op;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::load_sse_fixture;
use crate::test_support::core::load_sse_fixture_with_id;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::TestCodex;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use lha_llm::RuntimeEndpoint;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn sse_incomplete() -> String {
    load_sse_fixture("tests/fixtures/incomplete_sse.json")
}

fn sse_completed(id: &str) -> String {
    load_sse_fixture_with_id("../fixtures/completed_template.json", id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_on_early_close() {
    skip_if_no_network!();

    let server = MockServer::start().await;

    struct SeqResponder;
    impl Respond for SeqResponder {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            use std::sync::atomic::AtomicUsize;
            use std::sync::atomic::Ordering;
            static CALLS: AtomicUsize = AtomicUsize::new(0);
            let n = CALLS.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_raw(sse_incomplete(), "text/event-stream")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_raw(sse_completed("resp_ok"), "text/event-stream")
            }
        }
    }

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(SeqResponder {})
        .expect(2)
        .mount(&server)
        .await;

    // Configure retry behavior explicitly to avoid mutating process-wide
    // environment variables.

    let model_provider =
        RuntimeEndpoint::openai_compatible_responses("openai", format!("{}/v1", server.uri()))
            .with_env_key(Some("PATH".into()))
            .with_request_max_retries(Some(0))
            .with_stream_max_retries(Some(1))
            .with_stream_idle_timeout_ms(Some(2000));

    let TestCodex { codex, .. } = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
        })
        .build(&server)
        .await
        .unwrap();

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

    // Wait until TurnComplete (should succeed after retry).
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
}
