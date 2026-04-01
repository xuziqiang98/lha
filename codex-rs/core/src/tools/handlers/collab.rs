use crate::agent::AgentStatus;
use crate::agent::SpawnAgentOptions;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::apply_role_to_config;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::config::Config;
use crate::error::CodexErr;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::ThreadId;
use codex_protocol::models::BaseInstructions;
use codex_protocol::protocol::CollabAgentInteractionBeginEvent;
use codex_protocol::protocol::CollabAgentInteractionEndEvent;
use codex_protocol::protocol::CollabAgentRef;
use codex_protocol::protocol::CollabAgentSpawnBeginEvent;
use codex_protocol::protocol::CollabAgentSpawnEndEvent;
use codex_protocol::protocol::CollabAgentStatusEntry;
use codex_protocol::protocol::CollabCloseBeginEvent;
use codex_protocol::protocol::CollabCloseEndEvent;
use codex_protocol::protocol::CollabResumeBeginEvent;
use codex_protocol::protocol::CollabResumeEndEvent;
use codex_protocol::protocol::CollabWaitingBeginEvent;
use codex_protocol::protocol::CollabWaitingEndEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;

pub struct CollabHandler;

/// Minimum wait timeout to prevent tight polling loops from burning CPU.
pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;

#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    id: String,
}

#[async_trait]
impl ToolHandler for CollabHandler {
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
            call_id,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "collab handler received unsupported payload".to_string(),
                ));
            }
        };

        match tool_name.as_str() {
            "spawn_agent" => spawn::handle(session, turn, call_id, arguments).await,
            "send_input" => send_input::handle(session, turn, call_id, arguments).await,
            "resume_agent" => resume_agent::handle(session, turn, call_id, arguments).await,
            "wait" => wait::handle(session, turn, call_id, arguments).await,
            "close_agent" => close_agent::handle(session, turn, call_id, arguments).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported collab tool {other}"
            ))),
        }
    }
}

mod spawn {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct SpawnAgentArgs {
        message: Option<String>,
        items: Option<Vec<UserInput>>,
        agent_type: Option<String>,
        #[serde(default)]
        fork_context: bool,
    }

    #[derive(Debug, Serialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SpawnAgentArgs = parse_arguments(&arguments)?;
        let role_name = args
            .agent_type
            .as_deref()
            .map(str::trim)
            .filter(|role| !role.is_empty());
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let session_source = turn.client.get_session_source();
        let child_depth = next_thread_spawn_depth(&session_source);
        if exceeds_thread_spawn_depth_limit(child_depth, turn.client.config().agent_max_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }
        let mut config =
            build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
        apply_role_to_config(&mut config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        apply_spawn_agent_overrides(&mut config, child_depth);
        let model = config.model.clone().unwrap_or_default();
        let reasoning_effort = config.model_reasoning_effort.unwrap_or_default();
        session
            .send_event(
                &turn,
                CollabAgentSpawnBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    prompt: prompt.clone(),
                    model: model.clone(),
                    reasoning_effort,
                }
                .into(),
            )
            .await;

        let result = session
            .services
            .agent_control
            .spawn_agent_with_options(
                config,
                input_items,
                Some(thread_spawn_source(
                    session.conversation_id,
                    child_depth,
                    role_name,
                )),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: args.fork_context.then(|| call_id.clone()),
                },
            )
            .await
            .map_err(collab_spawn_error);
        let (new_thread_id, status) = match &result {
            Ok(thread_id) => (
                Some(*thread_id),
                session.services.agent_control.get_status(*thread_id).await,
            ),
            Err(_) => (None, AgentStatus::NotFound),
        };
        let (new_agent_nickname, new_agent_role) = match new_thread_id {
            Some(thread_id) => session
                .services
                .agent_control
                .get_agent_nickname_and_role(thread_id)
                .await
                .unwrap_or((None, None)),
            None => (None, None),
        };
        let nickname = new_agent_nickname.clone();
        session
            .send_event(
                &turn,
                CollabAgentSpawnEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    new_thread_id,
                    new_agent_nickname,
                    new_agent_role,
                    prompt,
                    model,
                    reasoning_effort,
                    status,
                }
                .into(),
            )
            .await;
        let new_thread_id = result?;
        let _role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);

        let content = serde_json::to_string(&SpawnAgentResult {
            agent_id: new_thread_id.to_string(),
            nickname,
        })
        .map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize spawn_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            success: Some(true),
            content_items: None,
        })
    }
}

mod send_input {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct SendInputArgs {
        id: String,
        message: Option<String>,
        items: Option<Vec<UserInput>>,
        #[serde(default)]
        interrupt: bool,
    }

    #[derive(Debug, Serialize)]
    struct SendInputResult {
        submission_id: String,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SendInputArgs = parse_arguments(&arguments)?;
        let receiver_thread_id = agent_id(&args.id)?;
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_thread_id)
            .await
            .unwrap_or((None, None));
        if args.interrupt {
            session
                .services
                .agent_control
                .interrupt_agent(receiver_thread_id)
                .await
                .map_err(|err| collab_agent_error(receiver_thread_id, err))?;
        }
        session
            .send_event(
                &turn,
                CollabAgentInteractionBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    prompt: prompt.clone(),
                }
                .into(),
            )
            .await;
        let result = session
            .services
            .agent_control
            .send_input(receiver_thread_id, input_items)
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err));
        let status = session
            .services
            .agent_control
            .get_status(receiver_thread_id)
            .await;
        session
            .send_event(
                &turn,
                CollabAgentInteractionEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    prompt,
                    status,
                }
                .into(),
            )
            .await;
        let submission_id = result?;

        let content = serde_json::to_string(&SendInputResult { submission_id }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize send_input result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            success: Some(true),
            content_items: None,
        })
    }
}

mod resume_agent {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct ResumeAgentArgs {
        id: String,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub(super) struct ResumeAgentResult {
        pub(super) status: AgentStatus,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: ResumeAgentArgs = parse_arguments(&arguments)?;
        let receiver_thread_id = agent_id(&args.id)?;
        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_thread_id)
            .await
            .unwrap_or((None, None));
        let session_source = turn.client.get_session_source();
        let child_depth = next_thread_spawn_depth(&session_source);
        if exceeds_thread_spawn_depth_limit(child_depth, turn.client.config().agent_max_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }

        session
            .send_event(
                &turn,
                CollabResumeBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    receiver_agent_nickname: receiver_agent_nickname.clone(),
                    receiver_agent_role: receiver_agent_role.clone(),
                }
                .into(),
            )
            .await;

        let mut status = session
            .services
            .agent_control
            .get_status(receiver_thread_id)
            .await;
        let error = if matches!(status, AgentStatus::NotFound) {
            match try_resume_closed_agent(&session, &turn, receiver_thread_id, child_depth).await {
                Ok(resumed_status) => {
                    status = resumed_status;
                    None
                }
                Err(err) => {
                    status = session
                        .services
                        .agent_control
                        .get_status(receiver_thread_id)
                        .await;
                    Some(err)
                }
            }
        } else {
            None
        };

        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_thread_id)
            .await
            .unwrap_or((receiver_agent_nickname, receiver_agent_role));
        session
            .send_event(
                &turn,
                CollabResumeEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    status: status.clone(),
                }
                .into(),
            )
            .await;

        if let Some(err) = error {
            return Err(err);
        }

        let content = serde_json::to_string(&ResumeAgentResult { status }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize resume_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            success: Some(true),
            content_items: None,
        })
    }

    async fn try_resume_closed_agent(
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        receiver_thread_id: ThreadId,
        child_depth: i32,
    ) -> Result<AgentStatus, FunctionCallError> {
        let config = build_agent_resume_config(turn.as_ref(), child_depth)?;
        let resumed_thread_id = session
            .services
            .agent_control
            .resume_agent_from_rollout(
                config,
                receiver_thread_id,
                thread_spawn_source(session.conversation_id, child_depth, None),
            )
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err))?;

        Ok(session
            .services
            .agent_control
            .get_status(resumed_thread_id)
            .await)
    }
}

pub(crate) mod wait {
    use super::*;
    use crate::agent::status::is_final;
    use futures::StreamExt;
    use futures::stream::FuturesUnordered;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::watch::Receiver;
    use tokio::time::Instant;

    use tokio::time::timeout_at;

    #[derive(Debug, Deserialize)]
    struct WaitArgs {
        ids: Vec<String>,
        timeout_ms: Option<i64>,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub(crate) struct WaitResult {
        pub(crate) status: HashMap<ThreadId, AgentStatus>,
        pub(crate) timed_out: bool,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: WaitArgs = parse_arguments(&arguments)?;
        if args.ids.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "ids must be non-empty".to_owned(),
            ));
        }
        let receiver_thread_ids = args
            .ids
            .iter()
            .map(|id| agent_id(id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut receiver_agents = Vec::with_capacity(receiver_thread_ids.len());
        for receiver_thread_id in &receiver_thread_ids {
            let (agent_nickname, agent_role) = session
                .services
                .agent_control
                .get_agent_nickname_and_role(*receiver_thread_id)
                .await
                .unwrap_or((None, None));
            receiver_agents.push(CollabAgentRef {
                thread_id: *receiver_thread_id,
                agent_nickname,
                agent_role,
            });
        }

        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let timeout_ms = match timeout_ms {
            ms if ms <= 0 => {
                return Err(FunctionCallError::RespondToModel(
                    "timeout_ms must be greater than zero".to_owned(),
                ));
            }
            ms => ms.clamp(MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS),
        };

        session
            .send_event(
                &turn,
                CollabWaitingBeginEvent {
                    sender_thread_id: session.conversation_id,
                    receiver_thread_ids: receiver_thread_ids.clone(),
                    receiver_agents: receiver_agents.clone(),
                    call_id: call_id.clone(),
                }
                .into(),
            )
            .await;

        let expected_ids = receiver_thread_ids.iter().copied().collect::<HashSet<_>>();
        let mut status_rxs = Vec::with_capacity(receiver_thread_ids.len());
        let mut statuses_map = HashMap::with_capacity(receiver_thread_ids.len());
        for id in &receiver_thread_ids {
            match session.services.agent_control.subscribe_status(*id).await {
                Ok(rx) => {
                    let status = rx.borrow().clone();
                    if is_final(&status) {
                        statuses_map.insert(*id, status);
                    } else {
                        status_rxs.push((*id, rx));
                    }
                }
                Err(CodexErr::ThreadNotFound(_)) => {
                    statuses_map.insert(*id, session.services.agent_control.get_status(*id).await);
                }
                Err(err) => {
                    let mut statuses = HashMap::with_capacity(1);
                    statuses.insert(*id, session.services.agent_control.get_status(*id).await);
                    session
                        .send_event(
                            &turn,
                            CollabWaitingEndEvent {
                                sender_thread_id: session.conversation_id,
                                call_id: call_id.clone(),
                                agent_statuses: build_wait_agent_statuses(
                                    &statuses,
                                    &receiver_agents,
                                ),
                                statuses,
                            }
                            .into(),
                        )
                        .await;
                    return Err(collab_agent_error(*id, err));
                }
            }
        }

        let result = WaitResult {
            status: statuses_map.clone(),
            timed_out: false,
        };
        let mut result = result;

        if statuses_map.len() < expected_ids.len() {
            let mut futures = FuturesUnordered::new();
            for (id, rx) in status_rxs {
                let session = session.clone();
                futures.push(wait_for_final_status(session, id, rx));
            }
            let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
            while statuses_map.len() < expected_ids.len() {
                match timeout_at(deadline, futures.next()).await {
                    Ok(Some(Some((id, status)))) => {
                        statuses_map.insert(id, status);
                    }
                    Ok(Some(None)) => continue,
                    Ok(None) | Err(_) => {
                        result.timed_out = statuses_map.len() < expected_ids.len();
                        break;
                    }
                }
            }
        }
        result.status = statuses_map.clone();

        session
            .send_event(
                &turn,
                CollabWaitingEndEvent {
                    sender_thread_id: session.conversation_id,
                    call_id,
                    agent_statuses: build_wait_agent_statuses(&statuses_map, &receiver_agents),
                    statuses: statuses_map,
                }
                .into(),
            )
            .await;

        let content = serde_json::to_string(&result).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize wait result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            success: None,
            content_items: None,
        })
    }

    pub(super) async fn wait_for_final_status(
        session: Arc<Session>,
        thread_id: ThreadId,
        mut status_rx: Receiver<AgentStatus>,
    ) -> Option<(ThreadId, AgentStatus)> {
        let mut status = status_rx.borrow().clone();
        if is_final(&status) {
            return Some((thread_id, status));
        }

        loop {
            if status_rx.changed().await.is_err() {
                let latest = session.services.agent_control.get_status(thread_id).await;
                return is_final(&latest).then_some((thread_id, latest));
            }
            status = status_rx.borrow().clone();
            if is_final(&status) {
                return Some((thread_id, status));
            }
        }
    }
}

pub mod close_agent {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize, Serialize)]
    pub(super) struct CloseAgentResult {
        pub(super) status: AgentStatus,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: CloseAgentArgs = parse_arguments(&arguments)?;
        let agent_id = agent_id(&args.id)?;
        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(agent_id)
            .await
            .unwrap_or((None, None));
        session
            .send_event(
                &turn,
                CollabCloseBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id: agent_id,
                }
                .into(),
            )
            .await;
        let status = match session
            .services
            .agent_control
            .subscribe_status(agent_id)
            .await
        {
            Ok(mut status_rx) => status_rx.borrow_and_update().clone(),
            Err(err) => {
                let status = session.services.agent_control.get_status(agent_id).await;
                session
                    .send_event(
                        &turn,
                        CollabCloseEndEvent {
                            call_id: call_id.clone(),
                            sender_thread_id: session.conversation_id,
                            receiver_thread_id: agent_id,
                            receiver_agent_nickname: receiver_agent_nickname.clone(),
                            receiver_agent_role: receiver_agent_role.clone(),
                            status,
                        }
                        .into(),
                    )
                    .await;
                return Err(collab_agent_error(agent_id, err));
            }
        };
        let result = if !matches!(status, AgentStatus::Shutdown) {
            session
                .services
                .agent_control
                .shutdown_agent(agent_id)
                .await
                .map_err(|err| collab_agent_error(agent_id, err))
                .map(|_| ())
        } else {
            Ok(())
        };
        session
            .send_event(
                &turn,
                CollabCloseEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id: agent_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    status: status.clone(),
                }
                .into(),
            )
            .await;
        result?;

        let content = serde_json::to_string(&CloseAgentResult { status }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize close_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            success: Some(true),
            content_items: None,
        })
    }
}

fn agent_id(id: &str) -> Result<ThreadId, FunctionCallError> {
    ThreadId::from_string(id)
        .map_err(|e| FunctionCallError::RespondToModel(format!("invalid agent id {id}: {e:?}")))
}

fn collab_spawn_error(err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        err => FunctionCallError::RespondToModel(format!("collab spawn failed: {err}")),
    }
}

fn collab_agent_error(agent_id: ThreadId, err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::ThreadNotFound(id) => {
            FunctionCallError::RespondToModel(format!("agent with id {id} not found"))
        }
        CodexErr::InternalAgentDied => {
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} is closed"))
        }
        CodexErr::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        err => FunctionCallError::RespondToModel(format!("collab tool failed: {err}")),
    }
}

pub(crate) fn build_agent_spawn_config(
    base_instructions: &BaseInstructions,
    turn: &TurnContext,
) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn)?;
    config.base_instructions = Some(base_instructions.text.clone());
    Ok(config)
}

fn build_agent_resume_config(
    turn: &TurnContext,
    child_depth: i32,
) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn)?;
    apply_spawn_agent_overrides(&mut config, child_depth);
    config.base_instructions = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, FunctionCallError> {
    let base_config = turn.client.config();
    let mut config = (*base_config).clone();
    config.model = Some(turn.client.get_model());
    config.model_provider = turn.client.get_provider();
    config.model_reasoning_effort = turn.client.get_reasoning_effort();
    config.model_reasoning_summary = turn.client.get_reasoning_summary();
    config.developer_instructions = turn.developer_instructions.clone();
    config.compact_prompt = turn.compact_prompt.clone();
    config.shell_environment_policy = turn.shell_environment_policy.clone();
    config.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
    config.cwd = turn.cwd.clone();
    config
        .approval_policy
        .set(turn.approval_policy)
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("approval_policy is invalid: {err}"))
        })?;
    config
        .sandbox_policy
        .set(turn.sandbox_policy.clone())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("sandbox_policy is invalid: {err}"))
        })?;
    Ok(config)
}

fn apply_spawn_agent_overrides(config: &mut Config, child_depth: i32) {
    if child_depth >= config.agent_max_depth {
        config.features.disable(Feature::Collab);
    }
}

fn thread_spawn_source(
    parent_thread_id: ThreadId,
    depth: i32,
    agent_role: Option<&str>,
) -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth,
        agent_nickname: None,
        agent_role: agent_role.map(str::to_string),
    })
}

fn parse_collab_input(
    message: Option<String>,
    items: Option<Vec<UserInput>>,
) -> Result<Vec<UserInput>, FunctionCallError> {
    match (message, items) {
        (Some(_), Some(_)) => Err(FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string(),
        )),
        (None, None) => Err(FunctionCallError::RespondToModel(
            "Provide one of: message or items".to_string(),
        )),
        (Some(message), None) => {
            if message.trim().is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Empty message can't be sent to an agent".to_string(),
                ));
            }
            Ok(vec![UserInput::Text {
                text: message,
                text_elements: Vec::new(),
            }])
        }
        (None, Some(items)) => {
            if items.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Empty items can't be sent to an agent".to_string(),
                ));
            }
            Ok(items)
        }
    }
}

fn input_preview(items: &[UserInput]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => text.clone(),
            UserInput::Image { .. } => "[image]".to_string(),
            UserInput::LocalImage { path } => format!("[local_image:{}]", path.display()),
            UserInput::Skill { name, path } => {
                format!("[skill:{name}]({})", path.display())
            }
            UserInput::Mention { name, path } => format!("[mention:{name}]({path})"),
            _ => "[input]".to_string(),
        })
        .collect();

    parts.join("\n")
}

fn build_wait_agent_statuses(
    statuses: &std::collections::HashMap<ThreadId, AgentStatus>,
    receiver_agents: &[CollabAgentRef],
) -> Vec<CollabAgentStatusEntry> {
    receiver_agents
        .iter()
        .filter_map(|agent| {
            statuses
                .get(&agent.thread_id)
                .cloned()
                .map(|status| CollabAgentStatusEntry {
                    agent: agent.clone(),
                    status,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CodexAuth;
    use crate::ThreadManager;
    use crate::built_in_model_providers;
    use crate::client::ModelClient;
    use crate::codex::make_session_and_context;
    use crate::config::types::ShellEnvironmentPolicy;
    use crate::function_tool::FunctionCallError;
    use crate::protocol::AskForApproval;
    use crate::protocol::Op;
    use crate::protocol::SandboxPolicy;
    use crate::protocol::SessionSource;
    use crate::protocol::SubAgentSource;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio::time::sleep;
    use tokio::time::timeout;

    fn invocation(
        session: Arc<crate::codex::Session>,
        turn: Arc<TurnContext>,
        tool_name: &str,
        payload: ToolPayload,
    ) -> ToolInvocation {
        ToolInvocation {
            session,
            turn,
            tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
            call_id: "call-1".to_string(),
            tool_name: tool_name.to_string(),
            payload,
        }
    }

    fn function_payload(args: serde_json::Value) -> ToolPayload {
        ToolPayload::Function {
            arguments: args.to_string(),
        }
    }

    fn thread_manager() -> ThreadManager {
        ThreadManager::with_models_provider(
            CodexAuth::from_api_key("dummy"),
            built_in_model_providers()["openai"].clone(),
        )
    }

    async fn wait_for_agent_status(
        agent_control: &crate::agent::AgentControl,
        thread_id: ThreadId,
        expected: AgentStatus,
    ) {
        timeout(Duration::from_secs(5), async {
            loop {
                if agent_control.get_status(thread_id).await == expected {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("agent status should reach expected value");
    }

    #[tokio::test]
    async fn handler_rejects_non_function_payloads() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            ToolPayload::Custom {
                input: "hello".to_string(),
            },
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("payload should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "collab handler received unsupported payload".to_string()
            )
        );
    }

    #[tokio::test]
    async fn handler_rejects_unknown_tool() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "unknown_tool",
            function_payload(json!({})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("tool should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("unsupported collab tool unknown_tool".to_string())
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_empty_message() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "   "})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("empty message should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Empty message can't be sent to an agent".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_when_message_and_items_are_both_set() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "hello",
                "items": [{"type": "text", "text": "hello"}]
            })),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("duplicate input should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Provide either message or items, but not both".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_errors_when_manager_dropped() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("spawn should fail without a manager");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_when_depth_limit_exceeded() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let max_depth = turn.client.config().agent_max_depth;

        let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: session.conversation_id,
            depth: max_depth,
            agent_nickname: None,
            agent_role: None,
        });
        turn.client = ModelClient::new_with_dynamic_context_window(
            turn.client.config(),
            Some(session.services.auth_manager.clone()),
            turn.client.get_model_info(),
            turn.client.dynamic_context_window(),
            turn.client.chat_role_compatibility(),
            turn.client.get_otel_manager(),
            turn.client.get_provider(),
            turn.client.get_reasoning_effort(),
            turn.client.get_reasoning_summary(),
            session.conversation_id,
            session_source,
            turn.client.transport_manager(),
        );

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("spawn should fail when depth limit exceeded");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string()
            )
        );
    }

    #[tokio::test]
    async fn send_input_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": "not-a-uuid", "message": "hi"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id not-a-uuid:"));
    }

    #[tokio::test]
    async fn send_input_reports_missing_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let agent_id = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": agent_id.to_string(), "message": "hi"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("missing agent should be reported");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
        );
    }

    #[tokio::test]
    async fn send_input_interrupts_before_prompt() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.client.config().as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "message": "hi",
                "interrupt": true
            })),
        );
        CollabHandler
            .handle(invocation)
            .await
            .expect("send_input should succeed");

        let ops = manager.captured_ops();
        let ops_for_agent: Vec<&Op> = ops
            .iter()
            .filter_map(|(id, op)| (*id == agent_id).then_some(op))
            .collect();
        assert_eq!(ops_for_agent.len(), 2);
        assert!(matches!(ops_for_agent[0], Op::Interrupt));
        assert!(matches!(ops_for_agent[1], Op::UserInput { .. }));

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct WaitResult {
        status: HashMap<ThreadId, AgentStatus>,
        timed_out: bool,
    }

    #[tokio::test]
    async fn wait_rejects_non_positive_timeout() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [ThreadId::new().to_string()],
                "timeout_ms": 0
            })),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("non-positive timeout should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("timeout_ms must be greater than zero".to_string())
        );
    }

    #[tokio::test]
    async fn wait_returns_not_found_for_missing_agents() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let id_a = ThreadId::new();
        let id_b = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [id_a.to_string(), id_b.to_string()],
                "timeout_ms": 1000
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            content, success, ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::from([
                    (id_a, AgentStatus::NotFound),
                    (id_b, AgentStatus::NotFound),
                ]),
                timed_out: false
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_times_out_when_status_is_not_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.client.config().as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": MIN_WAIT_TIMEOUT_MS
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            content, success, ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::new(),
                timed_out: true
            }
        );
        assert_eq!(success, None);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn wait_for_final_status_returns_interrupted_status() {
        let (session, _turn) = make_session_and_context().await;
        let session = Arc::new(session);
        let thread_id = ThreadId::new();
        let (status_tx, status_rx) = tokio::sync::watch::channel(AgentStatus::Running);

        tokio::spawn(async move {
            tokio::task::yield_now().await;
            status_tx
                .send(AgentStatus::Interrupted)
                .expect("status send should succeed");
        });

        let result = timeout(
            Duration::from_secs(1),
            wait::wait_for_final_status(session, thread_id, status_rx),
        )
        .await
        .expect("wait should finish")
        .expect("wait should return a final status");

        assert_eq!(result, (thread_id, AgentStatus::Interrupted));
    }

    #[tokio::test]
    async fn wait_returns_retained_shutdown_for_detached_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.client.config().as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let detached = manager
            .remove_thread(&agent_id)
            .await
            .expect("thread should detach");

        tokio::spawn(async move {
            tokio::task::yield_now().await;
            detached
                .submit(Op::Shutdown {})
                .await
                .expect("shutdown should submit");
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": MIN_WAIT_TIMEOUT_MS
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            content, success, ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::from([(agent_id, AgentStatus::Shutdown)]),
                timed_out: false,
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_collects_initial_and_later_final_statuses() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.client.config().as_ref().clone();
        let first = manager
            .start_thread(config.clone())
            .await
            .expect("start first");
        let second = manager.start_thread(config).await.expect("start second");
        let first_id = first.thread_id;
        let second_id = second.thread_id;

        first
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("first shutdown should submit");
        wait_for_agent_status(
            &session.services.agent_control,
            first_id,
            AgentStatus::Shutdown,
        )
        .await;

        let second_thread = second.thread.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            second_thread
                .submit(Op::Shutdown {})
                .await
                .expect("second shutdown should submit");
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [first_id.to_string(), second_id.to_string()],
                "timeout_ms": MIN_WAIT_TIMEOUT_MS
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            content, success, ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::from([
                    (first_id, AgentStatus::Shutdown),
                    (second_id, AgentStatus::Shutdown),
                ]),
                timed_out: false,
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_clamps_short_timeouts_to_minimum() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.client.config().as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": 10
            })),
        );

        let early = timeout(Duration::from_millis(50), CollabHandler.handle(invocation)).await;
        assert!(
            early.is_err(),
            "wait should not return before the minimum timeout clamp"
        );

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn resume_agent_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": "invalid"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id invalid:"));
    }

    #[tokio::test]
    async fn close_agent_submits_shutdown_and_returns_status() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.client.config().as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let status_before = manager.agent_control().get_status(agent_id).await;

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("close_agent should succeed");
        let ToolOutput::Function {
            content, success, ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: close_agent::CloseAgentResult =
            serde_json::from_str(&content).expect("close_agent result should be json");
        assert_eq!(result.status, status_before);
        assert_eq!(success, Some(true));

        let ops = manager.captured_ops();
        let submitted_shutdown = ops
            .iter()
            .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
        assert_eq!(submitted_shutdown, true);

        let agent_control = manager.agent_control();
        wait_for_agent_status(&agent_control, agent_id, AgentStatus::Shutdown).await;
    }

    #[tokio::test]
    async fn build_agent_spawn_config_uses_turn_context_values() {
        let (_session, mut turn) = make_session_and_context().await;
        let base_instructions = BaseInstructions {
            text: "base".to_string(),
        };
        turn.developer_instructions = Some("dev".to_string());
        turn.compact_prompt = Some("compact".to_string());
        turn.shell_environment_policy = ShellEnvironmentPolicy {
            use_profile: true,
            ..ShellEnvironmentPolicy::default()
        };
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        turn.codex_linux_sandbox_exe = Some(PathBuf::from("/bin/echo"));
        turn.approval_policy = AskForApproval::Never;
        turn.sandbox_policy = SandboxPolicy::DangerFullAccess;

        let config = build_agent_spawn_config(&base_instructions, &turn).expect("spawn config");
        let mut expected = (*turn.client.config()).clone();
        expected.base_instructions = Some(base_instructions.text);
        expected.model = Some(turn.client.get_model());
        expected.model_provider = turn.client.get_provider();
        expected.model_reasoning_effort = turn.client.get_reasoning_effort();
        expected.model_reasoning_summary = turn.client.get_reasoning_summary();
        expected.developer_instructions = turn.developer_instructions.clone();
        expected.compact_prompt = turn.compact_prompt.clone();
        expected.shell_environment_policy = turn.shell_environment_policy.clone();
        expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
        expected.cwd = turn.cwd.clone();
        expected
            .approval_policy
            .set(turn.approval_policy)
            .expect("approval policy set");
        expected
            .sandbox_policy
            .set(turn.sandbox_policy)
            .expect("sandbox policy set");
        assert_eq!(config, expected);
    }
}
