use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::protocol::EventMsg;
use adam_protocol::protocol::ThreadGoal;
use adam_protocol::protocol::ThreadGoalStatus;
use adam_protocol::protocol::ThreadGoalUpdatedEvent;
use adam_protocol::protocol::validate_thread_goal_objective;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct GoalHandler;

#[derive(Deserialize)]
struct CreateGoalArgs {
    objective: String,
}

#[derive(Deserialize)]
struct UpdateGoalArgs {
    status: String,
}

#[async_trait]
impl ToolHandler for GoalHandler {
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
                    "goal handler received unsupported payload".to_string(),
                ));
            }
        };

        let content = match tool_name.as_str() {
            "get_goal" => get_goal(session.as_ref(), turn.as_ref()).await?,
            "create_goal" => create_goal(session.as_ref(), turn.as_ref(), arguments).await?,
            "update_goal" => update_goal(session.as_ref(), turn.as_ref(), arguments).await?,
            other => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unsupported goal tool `{other}`"
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

async fn get_goal(
    session: &Session,
    turn_context: &TurnContext,
) -> Result<String, FunctionCallError> {
    ensure_goal_tool_allowed(session, turn_context).await?;
    let state_db = state_db(session)?;
    let goal = state_db
        .get_thread_goal(session.conversation_id)
        .await
        .map_err(tool_error)?
        .map(|goal| {
            let goal_id = goal.goal_id.clone();
            (goal_id, protocol_goal_from_state(goal))
        });
    if let Some((goal_id, _)) = &goal {
        turn_context
            .goal_context
            .set_expected_goal_id(goal_id.clone())
            .await;
    }
    let goal = goal.map(|(_, goal)| goal);
    serde_json::to_string(&json!({ "goal": goal })).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to serialize goal: {err}"))
    })
}

async fn create_goal(
    session: &Session,
    turn_context: &TurnContext,
    arguments: String,
) -> Result<String, FunctionCallError> {
    ensure_goal_tool_allowed(session, turn_context).await?;
    let args: CreateGoalArgs = parse_arguments(&arguments)?;
    validate_thread_goal_objective(&args.objective).map_err(FunctionCallError::RespondToModel)?;
    let state_db = state_db(session)?;
    let goal = state_db
        .insert_thread_goal_or_replace_completed(
            session.conversation_id,
            &args.objective,
            adam_state::ThreadGoalStatus::Active,
            None,
        )
        .await
        .map_err(tool_error)?
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "an unfinished goal already exists; update or complete it instead of creating another"
                    .to_string(),
            )
        })?;
    turn_context
        .goal_context
        .set_expected_goal_id(goal.goal_id.clone())
        .await;
    emit_goal_updated(session, turn_context, &goal).await;
    serde_json::to_string(&json!({ "goal": protocol_goal_from_state(goal) })).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to serialize goal: {err}"))
    })
}

async fn update_goal(
    session: &Session,
    turn_context: &TurnContext,
    arguments: String,
) -> Result<String, FunctionCallError> {
    ensure_goal_tool_allowed(session, turn_context).await?;
    let args: UpdateGoalArgs = parse_arguments(&arguments)?;
    let status = match args.status.as_str() {
        "complete" => adam_state::ThreadGoalStatus::Complete,
        "blocked" => adam_state::ThreadGoalStatus::Blocked,
        _ => {
            return Err(FunctionCallError::RespondToModel(
                "update_goal status must be `complete` or `blocked`".to_string(),
            ));
        }
    };
    let state_db = state_db(session)?;
    let expected_goal_id = turn_context.goal_context.expected_goal_id().await;
    let Some(expected_goal_id) = expected_goal_id else {
        let goal = state_db
            .get_thread_goal(session.conversation_id)
            .await
            .map_err(tool_error)?;
        let message = if goal.is_some() {
            "call get_goal before updating the current goal"
        } else {
            "no goal is currently set"
        };
        return Err(FunctionCallError::RespondToModel(message.to_string()));
    };
    let goal = state_db
        .update_thread_goal(
            session.conversation_id,
            adam_state::GoalUpdate {
                objective: None,
                status: Some(status),
                token_budget: None,
                expected_goal_id: Some(expected_goal_id),
            },
        )
        .await
        .map_err(tool_error)?
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "goal changed before this update; call get_goal before updating the current goal"
                    .to_string(),
            )
        })?;
    emit_goal_updated(session, turn_context, &goal).await;
    serde_json::to_string(&json!({ "goal": protocol_goal_from_state(goal) })).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to serialize goal: {err}"))
    })
}

async fn ensure_goal_tool_allowed(
    session: &Session,
    turn_context: &TurnContext,
) -> Result<(), FunctionCallError> {
    if turn_context.identity.kind != IdentityKind::Programmer {
        return Err(FunctionCallError::RespondToModel(
            "goal tools are only available to the programmer identity".to_string(),
        ));
    }
    if session.state_db().is_none() {
        return Err(FunctionCallError::RespondToModel(
            "goal tools require a persisted session".to_string(),
        ));
    }
    Ok(())
}

fn state_db(session: &Session) -> Result<crate::state_db::StateDbHandle, FunctionCallError> {
    session.state_db().ok_or_else(|| {
        FunctionCallError::RespondToModel("goal tools require a persisted session".to_string())
    })
}

fn tool_error(err: anyhow::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("goal tool failed: {err}"))
}

async fn emit_goal_updated(
    session: &Session,
    turn_context: &TurnContext,
    goal: &adam_state::ThreadGoal,
) {
    session
        .send_event(
            turn_context,
            EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: session.conversation_id,
                turn_id: Some(turn_context.sub_id.clone()),
                goal: protocol_goal_from_state(goal.clone()),
            }),
        )
        .await;
}

fn protocol_goal_from_state(goal: adam_state::ThreadGoal) -> ThreadGoal {
    ThreadGoal {
        thread_id: goal.thread_id,
        goal_id: goal.goal_id,
        objective: goal.objective,
        status: protocol_goal_status_from_state(goal.status),
        token_budget: goal.token_budget,
        tokens_used: goal.tokens_used,
        time_used_seconds: goal.time_used_seconds,
        created_at: goal.created_at.timestamp(),
        updated_at: goal.updated_at.timestamp(),
    }
}

fn protocol_goal_status_from_state(status: adam_state::ThreadGoalStatus) -> ThreadGoalStatus {
    match status {
        adam_state::ThreadGoalStatus::Active => ThreadGoalStatus::Active,
        adam_state::ThreadGoalStatus::Paused => ThreadGoalStatus::Paused,
        adam_state::ThreadGoalStatus::Blocked => ThreadGoalStatus::Blocked,
        adam_state::ThreadGoalStatus::UsageLimited => ThreadGoalStatus::UsageLimited,
        adam_state::ThreadGoalStatus::BudgetLimited => ThreadGoalStatus::BudgetLimited,
        adam_state::ThreadGoalStatus::Complete => ThreadGoalStatus::Complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use pretty_assertions::assert_eq;

    async fn make_goal_tool_context() -> (
        Session,
        TurnContext,
        crate::state_db::StateDbHandle,
        tempfile::TempDir,
    ) {
        let (mut session, mut turn_context) = make_session_and_context().await;
        let state_home = tempfile::tempdir().expect("create state temp dir");
        let state_db = adam_state::StateRuntime::init(
            state_home.path().to_path_buf(),
            "test".to_string(),
            None,
        )
        .await
        .expect("state runtime should initialize");
        session.services.state_db = Some(std::sync::Arc::clone(&state_db));
        turn_context.identity.kind = IdentityKind::Programmer;
        (session, turn_context, state_db, state_home)
    }

    fn response_goal(content: &str) -> ThreadGoal {
        let value: serde_json::Value =
            serde_json::from_str(content).expect("tool output should be JSON");
        serde_json::from_value(value["goal"].clone()).expect("goal should deserialize")
    }

    #[tokio::test]
    async fn create_goal_rejects_unfinished_goal() {
        let (session, turn_context, state_db, _state_home) = make_goal_tool_context().await;

        let first_goal = response_goal(
            &create_goal(
                &session,
                &turn_context,
                json!({ "objective": "finish current work" }).to_string(),
            )
            .await
            .expect("first goal should be created"),
        );
        let err = create_goal(
            &session,
            &turn_context,
            json!({ "objective": "start new work" }).to_string(),
        )
        .await
        .expect_err("unfinished goal should block creation");

        assert_eq!(
            FunctionCallError::RespondToModel(
                "an unfinished goal already exists; update or complete it instead of creating another"
                    .to_string()
            ),
            err
        );
        let stored = state_db
            .get_thread_goal(session.conversation_id)
            .await
            .expect("goal read should succeed")
            .expect("goal should exist");
        assert_eq!(first_goal, protocol_goal_from_state(stored));
    }

    #[tokio::test]
    async fn create_goal_replaces_completed_goal() {
        let (session, turn_context, state_db, _state_home) = make_goal_tool_context().await;

        let first_goal = response_goal(
            &create_goal(
                &session,
                &turn_context,
                json!({ "objective": "finish current work" }).to_string(),
            )
            .await
            .expect("first goal should be created"),
        );
        let completed_goal = response_goal(
            &update_goal(
                &session,
                &turn_context,
                json!({ "status": "complete" }).to_string(),
            )
            .await
            .expect("goal should be completed"),
        );
        assert_eq!(ThreadGoalStatus::Complete, completed_goal.status);

        let second_goal = response_goal(
            &create_goal(
                &session,
                &turn_context,
                json!({ "objective": "start new work" }).to_string(),
            )
            .await
            .expect("completed goal should allow new creation"),
        );

        assert_ne!(first_goal.goal_id, second_goal.goal_id);
        assert_eq!("start new work", second_goal.objective);
        assert_eq!(ThreadGoalStatus::Active, second_goal.status);
        assert_eq!(0, second_goal.tokens_used);
        assert_eq!(0, second_goal.time_used_seconds);
        let stored = state_db
            .get_thread_goal(session.conversation_id)
            .await
            .expect("goal read should succeed")
            .expect("goal should exist");
        assert_eq!(second_goal, protocol_goal_from_state(stored));
        assert_eq!(
            Some(second_goal.goal_id),
            turn_context.goal_context.expected_goal_id().await
        );
    }
}
