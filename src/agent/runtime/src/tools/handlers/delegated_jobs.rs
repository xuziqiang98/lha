use crate::agent_jobs::AgentJobExecConfig;
use crate::agent_jobs::AgentJobStatus;
use crate::agent_jobs::AgentJobType;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub struct DelegatedJobHandler;

pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;

#[async_trait]
impl ToolHandler for DelegatedJobHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            ..
        } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "delegated job handler received unsupported payload".to_string(),
                ));
            }
        };
        match tool_name.as_str() {
            "spawn_agent" => spawn_agent(session, turn, arguments).await,
            "wait" => wait(session, arguments).await,
            "close_agent" => close_agent(session, arguments).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported delegated job tool {other}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SpawnAgentArgs {
    message: String,
    agent_type: Option<String>,
    max_runtime_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SpawnAgentResult {
    id: String,
    agent_type: AgentJobType,
    status: AgentJobStatus,
}

async fn spawn_agent(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    arguments: String,
) -> Result<ToolOutput, FunctionCallError> {
    let args: SpawnAgentArgs = parse_arguments(&arguments)?;
    if args.message.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "message must be non-empty".to_string(),
        ));
    }
    let agent_type = match args.agent_type.as_deref().unwrap_or("explorer") {
        "explorer" => AgentJobType::Explorer,
        other => {
            return Err(FunctionCallError::RespondToModel(format!(
                "unsupported agent_type '{other}'; only 'explorer' is available"
            )));
        }
    };
    let exec_config = AgentJobExecConfig::from_runtime(&turn.runtime, &turn.runtime.get_model());
    let snapshot = session
        .services
        .agent_jobs
        .spawn(
            session.conversation_id,
            agent_type,
            args.message,
            turn.cwd.clone(),
            exec_config,
            args.max_runtime_seconds,
        )
        .await
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
    let content = serde_json::to_string(&SpawnAgentResult {
        id: snapshot.id,
        agent_type: snapshot.agent_type,
        status: snapshot.status,
    })
    .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize spawn result: {err}")))?;
    Ok(ToolOutput::Function {
        content,
        success: Some(true),
        content_items: None,
    })
}

#[derive(Debug, Deserialize)]
struct WaitArgs {
    ids: Vec<String>,
    timeout_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
struct WaitResult {
    jobs: BTreeMap<String, WaitJobResult>,
    timed_out: bool,
}

#[derive(Debug, Serialize)]
struct WaitJobResult {
    agent_type: AgentJobType,
    status: AgentJobStatus,
}

async fn wait(session: Arc<Session>, arguments: String) -> Result<ToolOutput, FunctionCallError> {
    let args: WaitArgs = parse_arguments(&arguments)?;
    if args.ids.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "ids must be non-empty".to_string(),
        ));
    }
    let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
    let timeout_ms = match timeout_ms {
        ms if ms <= 0 => {
            return Err(FunctionCallError::RespondToModel(
                "timeout_ms must be greater than zero".to_string(),
            ));
        }
        ms => ms.clamp(MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS),
    };
    let snapshots = session
        .services
        .agent_jobs
        .wait(&args.ids, Duration::from_millis(timeout_ms as u64))
        .await;
    let timed_out = snapshots.iter().any(|snapshot| !snapshot.status.is_final());
    let jobs = snapshots
        .into_iter()
        .map(|snapshot| {
            (
                snapshot.id,
                WaitJobResult {
                    agent_type: snapshot.agent_type,
                    status: snapshot.status,
                },
            )
        })
        .collect();
    let content = serde_json::to_string(&WaitResult { jobs, timed_out }).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize wait result: {err}"))
    })?;
    Ok(ToolOutput::Function {
        content,
        success: None,
        content_items: None,
    })
}

#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    id: String,
}

#[derive(Debug, Serialize)]
struct CloseAgentResult {
    id: String,
    agent_type: AgentJobType,
    status: AgentJobStatus,
}

async fn close_agent(
    session: Arc<Session>,
    arguments: String,
) -> Result<ToolOutput, FunctionCallError> {
    let args: CloseAgentArgs = parse_arguments(&arguments)?;
    let snapshot = session.services.agent_jobs.close(&args.id).await;
    let content = serde_json::to_string(&CloseAgentResult {
        id: snapshot.id,
        agent_type: snapshot.agent_type,
        status: snapshot.status,
    })
    .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize close result: {err}")))?;
    Ok(ToolOutput::Function {
        content,
        success: Some(true),
        content_items: None,
    })
}
