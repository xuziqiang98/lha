use crate::product::agent::agent_jobs::AgentJobExecConfig;
use crate::product::agent::agent_jobs::AgentJobSpawnOptions;
use crate::product::agent::agent_jobs::AgentJobStatus;
use crate::product::agent::agent_jobs::AgentJobType;
use crate::product::agent::codex::Session;
use crate::product::agent::codex::TurnContext;
use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::handlers::parse_arguments;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;
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
            "wait" => wait(session, turn, arguments).await,
            "close_agent" => close_agent(session, turn, arguments).await,
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
}

#[derive(Debug, Serialize)]
struct SpawnAgentResult {
    id: String,
    agent_type: AgentJobType,
    name: Option<String>,
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
    let agent_type = parse_agent_job_type(args.agent_type.as_deref())
        .map_err(FunctionCallError::RespondToModel)?;
    let exec_config = AgentJobExecConfig::from_runtime(
        &turn.runtime,
        &turn.runtime.get_model(),
        turn.sandbox_policy.clone(),
        turn.windows_sandbox_level,
    );
    let snapshot = session
        .services
        .agent_jobs
        .spawn(
            session.conversation_id,
            agent_type,
            args.message,
            turn.cwd.clone(),
            exec_config,
            AgentJobSpawnOptions::log_only(),
        )
        .await
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
    session.send_event(&turn, snapshot.status_event()).await;
    let content = serde_json::to_string(&SpawnAgentResult {
        id: snapshot.id,
        agent_type: snapshot.agent_type,
        name: snapshot.name,
        status: snapshot.status,
    })
    .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize spawn result: {err}")))?;
    Ok(ToolOutput::Function {
        content,
        success: Some(true),
        content_items: None,
    })
}

fn parse_agent_job_type(agent_type: Option<&str>) -> Result<AgentJobType, String> {
    match agent_type.unwrap_or("explorer") {
        "explorer" => Ok(AgentJobType::Explorer),
        other => Err(format!(
            "unsupported agent_type '{other}'; only 'explorer' is available"
        )),
    }
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
    name: Option<String>,
    status: AgentJobStatus,
}

async fn wait(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    arguments: String,
) -> Result<ToolOutput, FunctionCallError> {
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
    let mut jobs = BTreeMap::new();
    for snapshot in snapshots {
        session.send_event(&turn, snapshot.status_event()).await;
        jobs.insert(
            snapshot.id,
            WaitJobResult {
                agent_type: snapshot.agent_type,
                name: snapshot.name,
                status: snapshot.status,
            },
        );
    }
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
    name: Option<String>,
    status: AgentJobStatus,
}

async fn close_agent(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    arguments: String,
) -> Result<ToolOutput, FunctionCallError> {
    let args: CloseAgentArgs = parse_arguments(&arguments)?;
    let snapshot = session.services.agent_jobs.close(&args.id).await;
    session.send_event(&turn, snapshot.status_event()).await;
    let content = serde_json::to_string(&CloseAgentResult {
        id: snapshot.id,
        agent_type: snapshot.agent_type,
        name: snapshot.name,
        status: snapshot.status,
    })
    .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize close result: {err}")))?;
    Ok(ToolOutput::Function {
        content,
        success: Some(true),
        content_items: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn parse_agent_job_type_defaults_to_explorer() {
        assert_eq!(parse_agent_job_type(None), Ok(AgentJobType::Explorer));
    }

    #[test]
    fn parse_agent_job_type_accepts_explorer() {
        assert_eq!(
            parse_agent_job_type(Some("explorer")),
            Ok(AgentJobType::Explorer)
        );
    }

    #[test]
    fn parse_agent_job_type_rejects_reviewer() {
        let err = parse_agent_job_type(Some("reviewer")).expect_err("reviewer is unsupported");

        assert!(err.contains("reviewer"));
        assert!(err.contains("only 'explorer' is available"));
    }

    #[test]
    fn parse_agent_job_type_rejects_unknown_values() {
        let err = parse_agent_job_type(Some("planner")).expect_err("planner is unsupported");

        assert!(err.contains("planner"));
        assert!(err.contains("only 'explorer' is available"));
    }

    #[test]
    fn spawn_agent_result_serializes_name() {
        let result = SpawnAgentResult {
            id: "agent-job-1".to_string(),
            agent_type: AgentJobType::Explorer,
            name: Some("Boyle".to_string()),
            status: AgentJobStatus::Running,
        };

        assert_eq!(
            serde_json::to_value(result).expect("serialize spawn result"),
            json!({
                "id": "agent-job-1",
                "agent_type": "explorer",
                "name": "Boyle",
                "status": {
                    "status": "running"
                }
            })
        );
    }

    #[test]
    fn wait_result_serializes_job_names() {
        let mut jobs = BTreeMap::new();
        jobs.insert(
            "agent-job-1".to_string(),
            WaitJobResult {
                agent_type: AgentJobType::Explorer,
                name: Some("Boyle".to_string()),
                status: AgentJobStatus::Completed {
                    result: "done".to_string(),
                    exit_code: Some(0),
                },
            },
        );
        let result = WaitResult {
            jobs,
            timed_out: false,
        };

        assert_eq!(
            serde_json::to_value(result).expect("serialize wait result"),
            json!({
                "jobs": {
                    "agent-job-1": {
                        "agent_type": "explorer",
                        "name": "Boyle",
                        "status": {
                            "status": "completed",
                            "result": "done",
                            "exit_code": 0
                        }
                    }
                },
                "timed_out": false
            })
        );
    }

    #[test]
    fn close_agent_result_serializes_name() {
        let result = CloseAgentResult {
            id: "agent-job-1".to_string(),
            agent_type: AgentJobType::Explorer,
            name: Some("Boyle".to_string()),
            status: AgentJobStatus::Cancelled,
        };

        assert_eq!(
            serde_json::to_value(result).expect("serialize close result"),
            json!({
                "id": "agent-job-1",
                "agent_type": "explorer",
                "name": "Boyle",
                "status": {
                    "status": "cancelled"
                }
            })
        );
    }
}
