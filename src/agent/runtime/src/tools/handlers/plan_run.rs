use crate::codex::Session;
use crate::codex::TurnContext;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::protocol::EventMsg;
use adam_protocol::protocol::ThreadPlanRun;
use adam_protocol::protocol::ThreadPlanRunStatus;
use adam_protocol::protocol::ThreadPlanRunUpdatedEvent;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct PlanRunHandler;

#[derive(Deserialize)]
struct UpdatePlanRunArgs {
    status: String,
}

#[async_trait]
impl ToolHandler for PlanRunHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
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
                    "plan run handler received unsupported payload".to_string(),
                ));
            }
        };

        let content = match tool_name.as_str() {
            "get_plan_run" => get_plan_run(session.as_ref(), turn.as_ref()).await?,
            "update_plan_run" => {
                update_plan_run(session.as_ref(), turn.as_ref(), arguments).await?
            }
            other => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unsupported plan run tool `{other}`"
                )));
            }
        };

        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }
}

async fn get_plan_run(
    session: &Session,
    turn_context: &TurnContext,
) -> Result<String, FunctionCallError> {
    ensure_plan_run_tool_allowed(session, turn_context).await?;
    let state_db = state_db(session)?;
    let plan_run = state_db
        .get_thread_plan_run(session.conversation_id)
        .await
        .map_err(tool_error)?
        .map(|plan_run| {
            let plan_run_id = plan_run.plan_run_id.clone();
            (plan_run_id, protocol_plan_run_from_state(plan_run))
        });
    if let Some((plan_run_id, _)) = &plan_run {
        turn_context
            .plan_run_context
            .set_expected_plan_run_id(plan_run_id.clone())
            .await;
    }
    let plan_run = plan_run.map(|(_, plan_run)| plan_run);
    serde_json::to_string(&json!({ "plan_run": plan_run })).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to serialize plan run: {err}"))
    })
}

async fn update_plan_run(
    session: &Session,
    turn_context: &TurnContext,
    arguments: String,
) -> Result<String, FunctionCallError> {
    ensure_plan_run_tool_allowed(session, turn_context).await?;
    let args: UpdatePlanRunArgs = parse_arguments(&arguments)?;
    let status = match args.status.as_str() {
        "complete" => adam_state::ThreadPlanRunStatus::Complete,
        "blocked" => adam_state::ThreadPlanRunStatus::Blocked,
        _ => {
            return Err(FunctionCallError::RespondToModel(
                "update_plan_run status must be `complete` or `blocked`".to_string(),
            ));
        }
    };
    let state_db = state_db(session)?;
    let expected_plan_run_id = turn_context.plan_run_context.expected_plan_run_id().await;
    let Some(expected_plan_run_id) = expected_plan_run_id else {
        let plan_run = state_db
            .get_thread_plan_run(session.conversation_id)
            .await
            .map_err(tool_error)?;
        let message = if plan_run.is_some() {
            "call get_plan_run before updating the current plan run"
        } else {
            "no plan run is currently set"
        };
        return Err(FunctionCallError::RespondToModel(message.to_string()));
    };
    let plan_run = state_db
        .update_thread_plan_run(
            session.conversation_id,
            adam_state::PlanRunUpdate {
                plan_text: None,
                status: Some(status),
                token_budget: None,
                expected_plan_run_id: Some(expected_plan_run_id),
            },
        )
        .await
        .map_err(tool_error)?
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "plan run changed before this update; call get_plan_run before updating the current plan run"
                    .to_string(),
            )
        })?;
    emit_plan_run_updated(session, turn_context, &plan_run).await;
    serde_json::to_string(&json!({ "plan_run": protocol_plan_run_from_state(plan_run) })).map_err(
        |err| FunctionCallError::RespondToModel(format!("failed to serialize plan run: {err}")),
    )
}

async fn ensure_plan_run_tool_allowed(
    session: &Session,
    turn_context: &TurnContext,
) -> Result<(), FunctionCallError> {
    if turn_context.identity.kind != IdentityKind::Programmer {
        return Err(FunctionCallError::RespondToModel(
            "plan run tools are only available to the programmer identity".to_string(),
        ));
    }
    if !session.enabled(Feature::PlanCompletion) {
        return Err(FunctionCallError::RespondToModel(
            "plan run tools require the plan_completion feature".to_string(),
        ));
    }
    if session.state_db().is_none() {
        return Err(FunctionCallError::RespondToModel(
            "plan run tools require a persisted session".to_string(),
        ));
    }
    Ok(())
}

fn state_db(session: &Session) -> Result<crate::state_db::StateDbHandle, FunctionCallError> {
    session.state_db().ok_or_else(|| {
        FunctionCallError::RespondToModel("plan run tools require a persisted session".to_string())
    })
}

fn tool_error(err: anyhow::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("plan run tool failed: {err}"))
}

async fn emit_plan_run_updated(
    session: &Session,
    turn_context: &TurnContext,
    plan_run: &adam_state::ThreadPlanRun,
) {
    session
        .send_event(
            turn_context,
            EventMsg::ThreadPlanRunUpdated(ThreadPlanRunUpdatedEvent {
                thread_id: session.conversation_id,
                turn_id: Some(turn_context.sub_id.clone()),
                plan_run: protocol_plan_run_from_state(plan_run.clone()),
            }),
        )
        .await;
}

fn protocol_plan_run_from_state(plan_run: adam_state::ThreadPlanRun) -> ThreadPlanRun {
    ThreadPlanRun {
        thread_id: plan_run.thread_id,
        plan_run_id: plan_run.plan_run_id,
        plan_text: plan_run.plan_text,
        status: protocol_plan_run_status_from_state(plan_run.status),
        token_budget: plan_run.token_budget,
        tokens_used: plan_run.tokens_used,
        time_used_seconds: plan_run.time_used_seconds,
        created_at: plan_run.created_at.timestamp(),
        updated_at: plan_run.updated_at.timestamp(),
    }
}

fn protocol_plan_run_status_from_state(
    status: adam_state::ThreadPlanRunStatus,
) -> ThreadPlanRunStatus {
    match status {
        adam_state::ThreadPlanRunStatus::Active => ThreadPlanRunStatus::Active,
        adam_state::ThreadPlanRunStatus::Paused => ThreadPlanRunStatus::Paused,
        adam_state::ThreadPlanRunStatus::Blocked => ThreadPlanRunStatus::Blocked,
        adam_state::ThreadPlanRunStatus::UsageLimited => ThreadPlanRunStatus::UsageLimited,
        adam_state::ThreadPlanRunStatus::BudgetLimited => ThreadPlanRunStatus::BudgetLimited,
        adam_state::ThreadPlanRunStatus::Complete => ThreadPlanRunStatus::Complete,
    }
}
