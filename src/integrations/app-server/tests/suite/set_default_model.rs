use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use core_test_support::responses;
use lha_app_server_protocol::JSONRPCError;
use lha_app_server_protocol::JSONRPCResponse;
use lha_app_server_protocol::RequestId;
use lha_app_server_protocol::SetDefaultModelParams;
use lha_app_server_protocol::SetDefaultModelResponse;
use lha_app_server_protocol::ThreadStartParams;
use lha_app_server_protocol::ThreadStartResponse;
use lha_app_server_protocol::TurnStartParams;
use lha_app_server_protocol::TurnStartResponse;
use lha_app_server_protocol::UserInput as V2UserInput;
use lha_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::sleep;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_persists_overrides() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let params = SetDefaultModelParams {
        model: Some("gpt-4.1".to_string()),
        model_provider: None,
        reasoning_effort: None,
    };

    let request_id = mcp.send_set_default_model_request(params).await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let _: SetDefaultModelResponse = to_response(resp)?;

    assert_eq!(
        state_model_ref(codex_home.path()).await?,
        "openai.main:gpt-4.1"
    );
    assert_eq!(state_reasoning_effort(codex_home.path()).await?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_persists_reasoning_effort() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_set_default_model_request(SetDefaultModelParams {
            model: Some("gpt-4.1".to_string()),
            model_provider: None,
            reasoning_effort: Some(ReasoningEffort::High),
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let _: SetDefaultModelResponse = to_response(resp)?;

    assert_eq!(
        state_model_ref(codex_home.path()).await?,
        "openai.main:gpt-4.1"
    );
    assert_eq!(
        state_reasoning_effort(codex_home.path()).await?,
        Some(ReasoningEffort::High)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_persists_explicit_provider() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml_with_custom_provider(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let params = SetDefaultModelParams {
        model: Some("deepseek-v3".to_string()),
        model_provider: Some("iie".to_string()),
        reasoning_effort: None,
    };

    let request_id = mcp.send_set_default_model_request(params).await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let _: SetDefaultModelResponse = to_response(resp)?;

    assert_eq!(
        state_model_ref(codex_home.path()).await?,
        "iie.main:deepseek-v3"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_infers_provider_from_models_json() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_models_json_mapped_config_toml(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let params = SetDefaultModelParams {
        model: Some("deepseek-v3".to_string()),
        model_provider: None,
        reasoning_effort: None,
    };

    let request_id = mcp.send_set_default_model_request(params).await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let _: SetDefaultModelResponse = to_response(resp)?;

    assert_eq!(
        state_model_ref(codex_home.path()).await?,
        "provider_b.main:deepseek-v3"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_rejects_ambiguous_provider_mapping() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_ambiguous_models_json_config_toml(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_set_default_model_request(SetDefaultModelParams {
            model: Some("deepseek-v3".to_string()),
            model_provider: None,
            reasoning_effort: None,
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error
            .error
            .message
            .contains("model `deepseek-v3` is configured for multiple providers"),
        "unexpected error: {error:?}"
    );
    assert!(error.error.message.contains("chatanywhere"));
    assert!(error.error.message.contains("iie"));
    assert_eq!(
        state_model_ref(codex_home.path()).await?,
        "iie.main:fallback-model"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_rejects_unknown_explicit_provider() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let params = SetDefaultModelParams {
        model: Some("gpt-4.1".to_string()),
        model_provider: Some("missing-provider".to_string()),
        reasoning_effort: None,
    };

    let request_id = mcp.send_set_default_model_request(params).await?;

    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(
        error.error.message,
        "model provider `missing-provider` was not found"
    );

    assert!(!tokio::fs::try_exists(codex_home.path().join("state.json")).await?);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_switches_loaded_default_thread_to_messages_runtime() -> Result<()> {
    let server = create_mock_responses_and_messages_server().await;
    let codex_home = TempDir::new()?;
    create_variant_switching_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_thread(&mut mcp, ThreadStartParams::default()).await?;

    let request_id = mcp
        .send_set_default_model_request(SetDefaultModelParams {
            model: Some("glm-5.1".to_string()),
            model_provider: None,
            reasoning_effort: None,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: SetDefaultModelResponse = to_response(resp)?;

    start_turn(
        &mut mcp,
        TurnStartParams {
            thread_id,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        },
    )
    .await?;
    wait_for_request_count(&server, 1).await?;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    let paths = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .map(|request| request.url.path().to_string())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["/v1/messages"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_switches_loaded_implicit_default_thread_to_messages_runtime()
-> Result<()> {
    let server = create_mock_responses_and_messages_server().await;
    let codex_home = TempDir::new()?;
    create_implicit_default_variant_switching_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_thread(&mut mcp, ThreadStartParams::default()).await?;
    write_variant_switching_models_json(codex_home.path(), &server.uri())?;
    app_test_support::write_state_json(codex_home.path(), "i9vc.responses:gpt-5.4")?;

    let request_id = mcp
        .send_set_default_model_request(SetDefaultModelParams {
            model: Some("glm-5.1".to_string()),
            model_provider: None,
            reasoning_effort: None,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: SetDefaultModelResponse = to_response(resp)?;

    start_turn(
        &mut mcp,
        TurnStartParams {
            thread_id,
            input: vec![V2UserInput::Text {
                text: "Hello from implicit default thread".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        },
    )
    .await?;
    wait_for_request_count(&server, 1).await?;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    let paths = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .map(|request| request.url.path().to_string())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["/v1/messages"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_switches_loaded_default_thread_for_provider_only_update() -> Result<()> {
    let server = create_mock_responses_and_messages_server().await;
    let codex_home = TempDir::new()?;
    create_variant_switching_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_thread(&mut mcp, ThreadStartParams::default()).await?;

    let request_id = mcp
        .send_set_default_model_request(SetDefaultModelParams {
            model: None,
            model_provider: Some("i9vc.messages".to_string()),
            reasoning_effort: None,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: SetDefaultModelResponse = to_response(resp)?;

    start_turn(
        &mut mcp,
        TurnStartParams {
            thread_id,
            input: vec![V2UserInput::Text {
                text: "Hello after provider-only switch".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        },
    )
    .await?;
    wait_for_request_count(&server, 1).await?;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    let paths = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .map(|request| request.url.path().to_string())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["/v1/messages"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_does_not_switch_loaded_threads_with_explicit_provider() -> Result<()> {
    let server = create_mock_responses_and_messages_server().await;
    let codex_home = TempDir::new()?;
    create_variant_switching_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let default_thread_id = start_thread(&mut mcp, ThreadStartParams::default()).await?;

    let explicit_thread_id = start_thread(
        &mut mcp,
        ThreadStartParams {
            model: Some("gpt-4.1".to_string()),
            model_provider: Some("other_provider".to_string()),
            ..Default::default()
        },
    )
    .await?;

    let request_id = mcp
        .send_set_default_model_request(SetDefaultModelParams {
            model: Some("glm-5.1".to_string()),
            model_provider: None,
            reasoning_effort: None,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: SetDefaultModelResponse = to_response(resp)?;

    start_turn(
        &mut mcp,
        TurnStartParams {
            thread_id: default_thread_id,
            input: vec![V2UserInput::Text {
                text: "Hello from default thread".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        },
    )
    .await?;
    wait_for_request_count(&server, 1).await?;

    start_turn(
        &mut mcp,
        TurnStartParams {
            thread_id: explicit_thread_id,
            input: vec![V2UserInput::Text {
                text: "Hello from explicit thread".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        },
    )
    .await?;
    wait_for_request_count(&server, 2).await?;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    let paths = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .map(|request| request.url.path().to_string())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["/v1/messages", "/v1/responses"]);

    Ok(())
}

async fn state_model_ref(codex_home: &Path) -> Result<String> {
    let state_path = codex_home.join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&tokio::fs::read_to_string(state_path).await?)?;
    state["last_selected_model"]["model_ref"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("state should include model ref"))
}

async fn state_reasoning_effort(codex_home: &Path) -> Result<Option<ReasoningEffort>> {
    let state_path = codex_home.join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&tokio::fs::read_to_string(state_path).await?)?;
    Ok(serde_json::from_value(
        state
            .get("last_reasoning_effort")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )?)
}

// Helper to create a config.toml; mirrors create_conversation.rs
fn create_config_toml(codex_home: &Path) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    std::fs::write(config_toml, "")
}

fn create_models_json_mapped_config_toml(codex_home: &Path) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("models.json"),
        r#"{
  "providers": {
    "provider_a": { "endpoints": { "main": { "base_url": "https://example.com/a", "dialect": "chat", "bearer_token": "sk-a", "models": { "glm-5": {} } } } },
    "provider_b": { "endpoints": { "main": { "base_url": "https://example.com/b", "dialect": "chat", "bearer_token": "sk-b", "models": { "deepseek-v3": {} } } } }
  }
}
"#,
    )?;
    app_test_support::write_state_json(codex_home, "provider_a.main:glm-5")?;
    std::fs::write(codex_home.join("config.toml"), "")
}

fn create_config_toml_with_custom_provider(codex_home: &Path) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("models.json"),
        r#"{
  "providers": {
    "iie": { "endpoints": { "main": { "base_url": "https://example.com/iie", "dialect": "responses", "bearer_token": "sk-test", "models": { "deepseek-v3": {} } } } }
  }
}
"#,
    )?;
    std::fs::write(codex_home.join("config.toml"), "")
}

fn create_ambiguous_models_json_config_toml(codex_home: &Path) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("models.json"),
        r#"{
  "providers": {
    "iie": { "endpoints": { "main": { "base_url": "https://example.com/iie", "dialect": "responses", "bearer_token": "sk-iie", "models": { "fallback-model": {}, "deepseek-v3": {} } } } },
    "chatanywhere": { "endpoints": { "main": { "base_url": "https://example.com/chatanywhere", "dialect": "responses", "bearer_token": "sk-chatanywhere", "models": { "deepseek-v3": {} } } } }
  }
}
"#,
    )?;
    app_test_support::write_state_json(codex_home, "iie.main:fallback-model")?;
    std::fs::write(codex_home.join("config.toml"), "")
}

fn create_variant_switching_config_toml(
    codex_home: &Path,
    server_uri: &str,
) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("models.json"),
        format!(
            r#"{{
  "providers": {{
    "i9vc": {{
      "name": "i9vc",
      "endpoints": {{
        "responses": {{
          "name": "i9vc",
          "base_url": "{server_uri}/v1",
          "dialect": "responses",
          "bearer_token": "sk-responses",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{ "gpt-5.4": {{}} }}
        }},
        "messages": {{
          "name": "i9vc",
          "base_url": "{server_uri}/v1",
          "dialect": "messages",
          "bearer_token": "sk-messages",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{ "glm-5.1": {{}} }}
        }}
      }}
    }},
    "other_provider": {{
      "name": "other_provider",
      "endpoints": {{
        "main": {{
          "name": "other_provider",
          "base_url": "{server_uri}/v1",
          "dialect": "responses",
          "bearer_token": "sk-other",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{ "gpt-4.1": {{}} }}
        }}
      }}
    }}
  }}
}}
"#,
        ),
    )?;
    std::fs::write(
        codex_home.join("state.json"),
        r#"{
  "last_selected_model": { "model_ref": "i9vc.responses:gpt-5.4", "selected_at": null },
  "last_reasoning_effort": null,
  "last_model_verbosity": null,
  "last_selected_identity": null
}
"#,
    )?;
    std::fs::write(
        codex_home.join("config.toml"),
        r#"
approval_policy = "never"
sandbox_mode = "read-only"
"#,
    )
}

fn create_implicit_default_variant_switching_config_toml(
    codex_home: &Path,
    server_uri: &str,
) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("models.json"),
        format!(
            r#"{{
  "providers": {{
    "i9vc": {{
      "name": "i9vc",
      "endpoints": {{
        "responses": {{
          "name": "i9vc",
          "base_url": "{server_uri}/v1",
          "dialect": "responses",
          "bearer_token": "sk-responses",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{ "gpt-5.4": {{}} }}
        }}
      }}
    }}
  }}
}}
"#,
        ),
    )?;
    std::fs::write(
        codex_home.join("config.toml"),
        r#"
approval_policy = "never"
sandbox_mode = "read-only"
"#,
    )
}

fn write_variant_switching_models_json(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("models.json"),
        format!(
            r#"{{
  "providers": {{
    "i9vc": {{
      "name": "i9vc",
      "endpoints": {{
        "responses": {{
          "name": "i9vc",
          "base_url": "{server_uri}/v1",
          "dialect": "responses",
          "bearer_token": "sk-responses",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{ "gpt-5.4": {{}} }}
        }},
        "messages": {{
          "name": "i9vc",
          "base_url": "{server_uri}/v1",
          "dialect": "messages",
          "bearer_token": "sk-messages",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{ "glm-5.1": {{}} }}
        }}
      }}
    }}
  }}
}}
"#,
        ),
    )
}

async fn create_mock_responses_and_messages_server() -> MockServer {
    let server = responses::start_mock_server().await;

    let responses_body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(responses::sse_response(responses_body))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(messages_sse_response("Done"))
        .mount(&server)
        .await;

    server
}

fn messages_sse_response(message: &str) -> ResponseTemplate {
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":12}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\""
    )
    .to_string()
        + message
        + "\"}}\n\n"
        + "event: message_stop\n"
        + "data: {\"type\":\"message_stop\"}\n\n";

    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn start_thread(mcp: &mut McpProcess, params: ThreadStartParams) -> Result<String> {
    let request_id = mcp.send_thread_start_request(params).await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(resp)?;
    Ok(thread.id)
}

async fn start_turn(mcp: &mut McpProcess, params: TurnStartParams) -> Result<()> {
    let request_id = mcp.send_turn_start_request(params).await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: TurnStartResponse = to_response(resp)?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(())
}

async fn wait_for_request_count(server: &MockServer, expected: usize) -> Result<()> {
    timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let requests = server.received_requests().await.unwrap_or_default();
            if requests.len() >= expected {
                return;
            }
            sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await?;
    Ok(())
}
