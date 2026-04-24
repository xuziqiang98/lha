use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::find_thread_path_by_id_str;
use crate::rollout::RolloutRecorder;
use crate::subagents::AgentStatus;
use crate::subagents::guards::Guards;
use crate::subagents::role::DEFAULT_ROLE_NAME;
use crate::subagents::role::resolve_role_config;
use crate::subagents::status::is_final;
use crate::thread_manager::ThreadManagerState;
use codex_llm::ToolResultItem;
use codex_llm::ToolResultPayload;
use codex_protocol::ThreadId;
use codex_protocol::models::TranscriptItem;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde_json::json;
use std::sync::Arc;
use std::sync::Weak;
use tokio::sync::watch;

const AGENT_NAMES: &str = include_str!("agent_names.txt");
const FORKED_SPAWN_AGENT_OUTPUT_MESSAGE: &str = "You are the newly spawned agent. The prior conversation history was forked from your parent agent. Treat the next user message as your new task, and use the forked history only as background context.";

#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnAgentOptions {
    pub(crate) fork_parent_spawn_call_id: Option<String>,
}

fn format_subagent_notification_message(agent_id: &str, status: &AgentStatus) -> String {
    format!(
        "<subagent_notification>{}</subagent_notification>",
        json!({
            "agent_id": agent_id,
            "status": status,
        })
    )
}

fn format_subagent_context_line(agent_id: &str, agent_nickname: Option<&str>) -> String {
    match agent_nickname {
        Some(agent_nickname) => format!("- {agent_id}: {agent_nickname}"),
        None => format!("- {agent_id}"),
    }
}

fn default_agent_nickname_list() -> Vec<&'static str> {
    AGENT_NAMES
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect()
}

fn agent_nickname_candidates(
    config: &crate::config::Config,
    role_name: Option<&str>,
) -> Vec<String> {
    let role_name = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    if let Some(candidates) =
        resolve_role_config(config, role_name).and_then(|role| role.nickname_candidates.clone())
    {
        return candidates;
    }

    default_agent_nickname_list()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
}

/// Control-plane handle for multi-agent operations.
/// `AgentControl` is held by each session (via `SessionServices`). It provides capability to
/// spawn new agents and the inter-agent communication layer.
/// An `AgentControl` instance is shared per "user session" which means the same `AgentControl`
/// is used for every sub-agent spawned by Codex. By doing so, we make sure the guards are
/// scoped to a user session.
#[derive(Clone, Default)]
pub(crate) struct AgentControl {
    /// Weak handle back to the global thread registry/state.
    /// This is `Weak` to avoid reference cycles and shadow persistence of the form
    /// `ThreadManagerState -> CodexThread -> Session -> SessionServices -> ThreadManagerState`.
    manager: Weak<ThreadManagerState>,
    state: Arc<Guards>,
}

impl AgentControl {
    /// Construct a new `AgentControl` that can spawn/message agents via the given manager state.
    pub(crate) fn new(manager: Weak<ThreadManagerState>) -> Self {
        Self {
            manager,
            ..Default::default()
        }
    }

    /// Spawn a new agent thread and submit the initial prompt.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn spawn_agent(
        &self,
        config: crate::config::Config,
        items: Vec<UserInput>,
        session_source: Option<SessionSource>,
    ) -> CodexResult<ThreadId> {
        self.spawn_agent_with_options(config, items, session_source, SpawnAgentOptions::default())
            .await
    }

    pub(crate) async fn spawn_agent_with_options(
        &self,
        config: crate::config::Config,
        items: Vec<UserInput>,
        session_source: Option<SessionSource>,
        options: SpawnAgentOptions,
    ) -> CodexResult<ThreadId> {
        let state = self.upgrade()?;
        let mut reservation = self.state.reserve_spawn_slot(config.agent_max_threads)?;
        let session_source =
            normalize_thread_spawn_source(&config, session_source, &mut reservation)?;
        let notification_source = session_source.clone();

        let new_thread = match session_source {
            Some(session_source) => {
                if let Some(call_id) = options.fork_parent_spawn_call_id.as_ref() {
                    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                        parent_thread_id,
                        ..
                    }) = session_source.clone()
                    else {
                        return Err(CodexErr::Fatal(
                            "spawn_agent fork requires a thread-spawn session source".to_string(),
                        ));
                    };

                    let parent_thread = state.get_thread(parent_thread_id).await.ok();
                    if let Some(parent_thread) = parent_thread.as_ref() {
                        parent_thread.flush_rollout().await;
                    }

                    let rollout_path = parent_thread
                        .as_ref()
                        .and_then(|parent_thread| parent_thread.rollout_path())
                        .or(find_thread_path_by_id_str(
                            config.codex_home.as_path(),
                            &parent_thread_id.to_string(),
                        )
                        .await?)
                        .ok_or_else(|| {
                            CodexErr::Fatal(format!(
                                "parent thread rollout unavailable for fork: {parent_thread_id}"
                            ))
                        })?;
                    let mut forked_rollout_items =
                        RolloutRecorder::get_rollout_history(&rollout_path)
                            .await?
                            .get_rollout_items();
                    forked_rollout_items.push(RolloutItem::TranscriptItem(TranscriptItem::from(
                        ToolResultItem {
                            call_id: call_id.clone(),
                            tool_name: "spawn_agent".to_string(),
                            payload: ToolResultPayload::Structured {
                                content: FORKED_SPAWN_AGENT_OUTPUT_MESSAGE.to_string(),
                                content_items: None,
                                success: Some(true),
                            },
                        },
                    )));
                    state
                        .spawn_thread_with_initial_history_and_source(
                            config,
                            InitialHistory::Forked(forked_rollout_items),
                            self.clone(),
                            session_source,
                        )
                        .await?
                } else {
                    state
                        .spawn_new_thread_with_source(config, self.clone(), session_source)
                        .await?
                }
            }
            None => state.spawn_new_thread(config, self.clone()).await?,
        };
        reservation.commit(new_thread.thread_id);

        state.notify_thread_created(new_thread.thread_id);
        self.send_input(new_thread.thread_id, items).await?;
        self.maybe_start_completion_watcher(new_thread.thread_id, notification_source);

        Ok(new_thread.thread_id)
    }

    pub(crate) async fn resume_agent_from_rollout(
        &self,
        config: crate::config::Config,
        thread_id: ThreadId,
        session_source: SessionSource,
    ) -> CodexResult<ThreadId> {
        let state = self.upgrade()?;
        let mut reservation = self.state.reserve_spawn_slot(config.agent_max_threads)?;
        let session_source = restore_thread_spawn_source_for_resume(
            &config,
            thread_id,
            session_source,
            &mut reservation,
        )
        .await?
        .ok_or_else(|| {
            CodexErr::UnsupportedOperation("resume requires a session source".to_string())
        })?;
        let notification_source = Some(session_source.clone());
        let rollout_path =
            find_thread_path_by_id_str(config.codex_home.as_path(), &thread_id.to_string())
                .await?
                .ok_or(CodexErr::ThreadNotFound(thread_id))?;

        let resumed_thread = state
            .resume_thread_from_rollout_with_source(
                config,
                rollout_path,
                self.clone(),
                session_source,
            )
            .await?;
        reservation.commit(resumed_thread.thread_id);
        state.notify_thread_created(resumed_thread.thread_id);
        self.maybe_start_completion_watcher(resumed_thread.thread_id, notification_source);

        Ok(resumed_thread.thread_id)
    }

    /// Send rich user input items to an existing agent thread.
    pub(crate) async fn send_input(
        &self,
        agent_id: ThreadId,
        items: Vec<UserInput>,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        let result = state
            .send_op(
                agent_id,
                Op::UserInput {
                    items,
                    final_output_json_schema: None,
                },
            )
            .await;
        if matches!(result, Err(CodexErr::InternalAgentDied)) {
            let _ = state.remove_thread(&agent_id).await;
            self.state.release_spawned_thread(agent_id);
        }
        result
    }

    /// Send a plain-text prompt to an existing agent thread.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn send_prompt(
        &self,
        agent_id: ThreadId,
        prompt: String,
    ) -> CodexResult<String> {
        self.send_input(
            agent_id,
            vec![UserInput::Text {
                text: prompt,
                text_elements: Vec::new(),
            }],
        )
        .await
    }

    /// Interrupt the current task for an existing agent thread.
    pub(crate) async fn interrupt_agent(&self, agent_id: ThreadId) -> CodexResult<String> {
        let state = self.upgrade()?;
        state.send_op(agent_id, Op::Interrupt).await
    }

    /// Submit a shutdown request to an existing agent thread.
    pub(crate) async fn shutdown_agent(&self, agent_id: ThreadId) -> CodexResult<String> {
        let state = self.upgrade()?;
        let result = state.send_op(agent_id, Op::Shutdown {}).await;
        let _ = state.remove_thread(&agent_id).await;
        self.state.release_spawned_thread(agent_id);
        result
    }

    /// Fetch the last known status for `agent_id`, returning `NotFound` when unavailable.
    pub(crate) async fn get_status(&self, agent_id: ThreadId) -> AgentStatus {
        let Ok(state) = self.upgrade() else {
            return AgentStatus::NotFound;
        };
        state
            .get_status_or_retained(agent_id)
            .await
            .unwrap_or(AgentStatus::NotFound)
    }

    pub(crate) async fn get_agent_nickname_and_role(
        &self,
        agent_id: ThreadId,
    ) -> Option<(Option<String>, Option<String>)> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(thread) = state.get_thread(agent_id).await else {
            return None;
        };
        let session_source = thread.config_snapshot().await.session_source;
        Some((
            session_source.get_nickname(),
            session_source.get_agent_role(),
        ))
    }

    pub(crate) async fn format_environment_context_subagents(
        &self,
        parent_thread_id: ThreadId,
    ) -> String {
        let Ok(state) = self.upgrade() else {
            return String::new();
        };

        let mut agents = Vec::new();
        for thread_id in state.list_thread_ids().await {
            let Ok(thread) = state.get_thread(thread_id).await else {
                continue;
            };
            let snapshot = thread.config_snapshot().await;
            let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: agent_parent_thread_id,
                agent_nickname,
                ..
            }) = snapshot.session_source
            else {
                continue;
            };
            if agent_parent_thread_id != parent_thread_id {
                continue;
            }
            agents.push(format_subagent_context_line(
                &thread_id.to_string(),
                agent_nickname.as_deref(),
            ));
        }
        agents.sort();
        agents.join("\n")
    }

    /// Subscribe to status updates for `agent_id`, yielding the latest value and changes.
    pub(crate) async fn subscribe_status(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<watch::Receiver<AgentStatus>> {
        let state = self.upgrade()?;
        state
            .subscribe_status_or_retained(agent_id)
            .await
            .ok_or(CodexErr::ThreadNotFound(agent_id))
    }

    fn maybe_start_completion_watcher(
        &self,
        child_thread_id: ThreadId,
        session_source: Option<SessionSource>,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return;
        };
        let control = self.clone();
        tokio::spawn(async move {
            let status = match control.subscribe_status(child_thread_id).await {
                Ok(mut status_rx) => {
                    let mut status = status_rx.borrow().clone();
                    while !is_final(&status) {
                        if status_rx.changed().await.is_err() {
                            status = control.get_status(child_thread_id).await;
                            break;
                        }
                        status = status_rx.borrow().clone();
                    }
                    status
                }
                Err(_) => control.get_status(child_thread_id).await,
            };
            if !is_final(&status) {
                return;
            }

            let Ok(state) = control.upgrade() else {
                return;
            };
            let Ok(parent_thread) = state.get_thread(parent_thread_id).await else {
                return;
            };
            parent_thread
                .inject_user_message_without_turn(format_subagent_notification_message(
                    &child_thread_id.to_string(),
                    &status,
                ))
                .await;
        });
    }

    fn upgrade(&self) -> CodexResult<Arc<ThreadManagerState>> {
        self.manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))
    }
}

fn normalize_thread_spawn_source(
    config: &crate::config::Config,
    session_source: Option<SessionSource>,
    reservation: &mut crate::subagents::guards::SpawnReservation,
) -> CodexResult<Option<SessionSource>> {
    Ok(match session_source {
        Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_nickname,
            agent_role,
        })) => {
            let candidate_names = agent_nickname_candidates(config, agent_role.as_deref());
            let candidate_name_refs: Vec<&str> =
                candidate_names.iter().map(String::as_str).collect();
            let reserved_nickname = reservation.reserve_agent_nickname_with_preference(
                &candidate_name_refs,
                agent_nickname.as_deref(),
            )?;
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                agent_nickname: Some(reserved_nickname),
                agent_role,
            }))
        }
        other => other,
    })
}

async fn restore_thread_spawn_source_for_resume(
    config: &crate::config::Config,
    thread_id: ThreadId,
    session_source: SessionSource,
    reservation: &mut crate::subagents::guards::SpawnReservation,
) -> CodexResult<Option<SessionSource>> {
    let session_source = match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth,
            agent_nickname,
            agent_role,
        }) => {
            let (stored_agent_nickname, stored_agent_role) =
                restored_thread_spawn_metadata(config, thread_id).await;
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                agent_nickname: stored_agent_nickname.or(agent_nickname),
                agent_role: stored_agent_role.or(agent_role),
            }))
        }
        other => Some(other),
    };
    normalize_thread_spawn_source(config, session_source, reservation)
}

async fn restored_thread_spawn_metadata(
    config: &crate::config::Config,
    thread_id: ThreadId,
) -> (Option<String>, Option<String>) {
    let Some(state_db_ctx) = crate::state_db::get_state_db(config, None).await else {
        return (None, None);
    };

    let Ok(Some(metadata)) = state_db_ctx.get_thread(thread_id).await else {
        return (None, None);
    };

    let Ok(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        agent_nickname,
        agent_role,
        ..
    })) = serde_json::from_str::<SessionSource>(&metadata.source)
    else {
        return (None, None);
    };

    (agent_nickname, agent_role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CodexAuth;
    use crate::CodexThread;
    use crate::ThreadManager;
    use crate::config::Config;
    use crate::config::ConfigBuilder;
    use crate::features::Feature;
    use crate::subagents::agent_status_from_event;
    use assert_matches::assert_matches;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::protocol::ErrorEvent;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::TurnAbortReason;
    use codex_protocol::protocol::TurnAbortedEvent;
    use codex_protocol::protocol::TurnCompleteEvent;
    use codex_protocol::protocol::TurnStartedEvent;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tokio::time::Duration;
    use tokio::time::sleep;
    use tokio::time::timeout;
    use toml::Value as TomlValue;

    async fn test_config_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> (TempDir, Config) {
        let home = TempDir::new().expect("create temp dir");
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .cli_overrides(cli_overrides)
            .build()
            .await
            .expect("load default test config");
        (home, config)
    }

    async fn test_config() -> (TempDir, Config) {
        test_config_with_cli_overrides(Vec::new()).await
    }

    struct AgentControlHarness {
        _home: TempDir,
        config: Config,
        manager: ThreadManager,
        control: AgentControl,
    }

    impl AgentControlHarness {
        async fn new() -> Self {
            let (home, config) = test_config().await;
            let manager = ThreadManager::with_models_provider_and_home(
                CodexAuth::from_api_key("dummy"),
                config.model_provider_id.as_str(),
                config.model_provider.clone(),
                config.codex_home.clone(),
            );
            let control = manager.agent_control();
            Self {
                _home: home,
                config,
                manager,
                control,
            }
        }

        async fn start_thread(&self) -> (ThreadId, Arc<CodexThread>) {
            let new_thread = self
                .manager
                .start_thread(self.config.clone())
                .await
                .expect("start thread");
            (new_thread.thread_id, new_thread.thread)
        }
    }

    async fn wait_for_agent_status(
        control: &AgentControl,
        thread_id: ThreadId,
        expected: AgentStatus,
    ) {
        timeout(Duration::from_secs(5), async {
            loop {
                if control.get_status(thread_id).await == expected {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("agent status should reach expected value");
    }

    #[tokio::test]
    async fn send_prompt_errors_when_manager_dropped() {
        let control = AgentControl::default();
        let err = control
            .send_prompt(ThreadId::new(), "hello".to_string())
            .await
            .expect_err("send_prompt should fail without a manager");
        assert_eq!(
            err.to_string(),
            "unsupported operation: thread manager dropped"
        );
    }

    #[tokio::test]
    async fn get_status_returns_not_found_without_manager() {
        let control = AgentControl::default();
        let got = control.get_status(ThreadId::new()).await;
        assert_eq!(got, AgentStatus::NotFound);
    }

    #[tokio::test]
    async fn on_event_updates_status_from_task_started() {
        let status = agent_status_from_event(&EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Custom,
        }));
        assert_eq!(status, Some(AgentStatus::Running));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_task_complete() {
        let status = agent_status_from_event(&EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: Some("done".to_string()),
        }));
        let expected = AgentStatus::Completed(Some("done".to_string()));
        assert_eq!(status, Some(expected));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_error() {
        let status = agent_status_from_event(&EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            codex_error_info: None,
        }));

        let expected = AgentStatus::Errored("boom".to_string());
        assert_eq!(status, Some(expected));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_turn_aborted() {
        let status = agent_status_from_event(&EventMsg::TurnAborted(TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }));

        let expected = AgentStatus::Interrupted;
        assert_eq!(status, Some(expected));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_shutdown_complete() {
        let status = agent_status_from_event(&EventMsg::ShutdownComplete);
        assert_eq!(status, Some(AgentStatus::Shutdown));
    }

    #[tokio::test]
    async fn spawn_agent_errors_when_manager_dropped() {
        let control = AgentControl::default();
        let (_home, config) = test_config().await;
        let err = control
            .spawn_agent(
                config,
                vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect_err("spawn_agent should fail without a manager");
        assert_eq!(
            err.to_string(),
            "unsupported operation: thread manager dropped"
        );
    }

    #[tokio::test]
    async fn send_prompt_errors_when_thread_missing() {
        let harness = AgentControlHarness::new().await;
        let thread_id = ThreadId::new();
        let err = harness
            .control
            .send_prompt(thread_id, "hello".to_string())
            .await
            .expect_err("send_prompt should fail for missing thread");
        assert_matches!(err, CodexErr::ThreadNotFound(id) if id == thread_id);
    }

    #[tokio::test]
    async fn get_status_returns_not_found_for_missing_thread() {
        let harness = AgentControlHarness::new().await;
        let status = harness.control.get_status(ThreadId::new()).await;
        assert_eq!(status, AgentStatus::NotFound);
    }

    #[tokio::test]
    async fn get_status_returns_pending_init_for_new_thread() {
        let harness = AgentControlHarness::new().await;
        let (thread_id, _) = harness.start_thread().await;
        let status = harness.control.get_status(thread_id).await;
        assert_eq!(status, AgentStatus::PendingInit);
    }

    #[tokio::test]
    async fn subscribe_status_errors_for_missing_thread() {
        let harness = AgentControlHarness::new().await;
        let thread_id = ThreadId::new();
        let err = harness
            .control
            .subscribe_status(thread_id)
            .await
            .expect_err("subscribe_status should fail for missing thread");
        assert_matches!(err, CodexErr::ThreadNotFound(id) if id == thread_id);
    }

    #[tokio::test]
    async fn subscribe_status_updates_on_shutdown() {
        let harness = AgentControlHarness::new().await;
        let (thread_id, thread) = harness.start_thread().await;
        let mut status_rx = harness
            .control
            .subscribe_status(thread_id)
            .await
            .expect("subscribe_status should succeed");
        assert_eq!(status_rx.borrow().clone(), AgentStatus::PendingInit);

        let _ = thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");

        let _ = status_rx.changed().await;
        assert_eq!(status_rx.borrow().clone(), AgentStatus::Shutdown);
    }

    #[tokio::test]
    async fn detached_thread_retains_final_status_after_shutdown() {
        let harness = AgentControlHarness::new().await;
        let (thread_id, _thread) = harness.start_thread().await;
        let detached = harness
            .manager
            .remove_thread(&thread_id)
            .await
            .expect("thread should detach");

        detached
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");

        wait_for_agent_status(&harness.control, thread_id, AgentStatus::Shutdown).await;

        let status_rx = harness
            .control
            .subscribe_status(thread_id)
            .await
            .expect("retained status subscription should succeed");
        assert_eq!(status_rx.borrow().clone(), AgentStatus::Shutdown);
    }

    #[tokio::test]
    async fn send_input_submits_user_message() {
        let harness = AgentControlHarness::new().await;
        let (thread_id, _thread) = harness.start_thread().await;

        let items = vec![UserInput::Text {
            text: "hello from tests".to_string(),
            text_elements: Vec::new(),
        }];
        let submission_id = harness
            .control
            .send_input(thread_id, items.clone())
            .await
            .expect("send_input should succeed");
        assert!(!submission_id.is_empty());
        let expected = (
            thread_id,
            Op::UserInput {
                items,
                final_output_json_schema: None,
            },
        );
        let captured = harness
            .manager
            .captured_ops()
            .into_iter()
            .find(|entry| *entry == expected);
        assert_eq!(captured, Some(expected));
    }

    #[tokio::test]
    async fn spawn_agent_creates_thread_and_sends_prompt() {
        let harness = AgentControlHarness::new().await;
        let items = vec![UserInput::Text {
            text: "spawned".to_string(),
            text_elements: Vec::new(),
        }];
        let thread_id = harness
            .control
            .spawn_agent(harness.config.clone(), items.clone(), None)
            .await
            .expect("spawn_agent should succeed");
        let _thread = harness
            .manager
            .get_thread(thread_id)
            .await
            .expect("thread should be registered");
        let expected = (
            thread_id,
            Op::UserInput {
                items,
                final_output_json_schema: None,
            },
        );
        let captured = harness
            .manager
            .captured_ops()
            .into_iter()
            .find(|entry| *entry == expected);
        assert_eq!(captured, Some(expected));
    }

    #[tokio::test]
    async fn spawn_agent_assigns_nickname_for_thread_spawn_sources() {
        let harness = AgentControlHarness::new().await;
        let thread_id = harness
            .control
            .spawn_agent(
                harness.config.clone(),
                vec![UserInput::Text {
                    text: "spawned".to_string(),
                    text_elements: Vec::new(),
                }],
                Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id: ThreadId::new(),
                    depth: 1,
                    agent_nickname: None,
                    agent_role: Some("worker".to_string()),
                })),
            )
            .await
            .expect("spawn_agent should succeed");

        let metadata = harness
            .control
            .get_agent_nickname_and_role(thread_id)
            .await
            .expect("agent metadata");
        assert!(metadata.0.is_some());
        assert_eq!(metadata.1, Some("worker".to_string()));
    }

    #[tokio::test]
    async fn resume_agent_restores_stored_role_for_thread_spawn_sources() {
        let (home, mut config) = test_config().await;
        config.features.enable(Feature::Sqlite);
        let manager = ThreadManager::with_models_provider_and_home(
            CodexAuth::from_api_key("dummy"),
            config.model_provider_id.as_str(),
            config.model_provider.clone(),
            config.codex_home.clone(),
        );
        let control = manager.agent_control();
        let harness = AgentControlHarness {
            _home: home,
            config,
            manager,
            control,
        };
        let (parent_thread_id, _parent_thread) = harness.start_thread().await;

        let child_thread_id = harness
            .control
            .spawn_agent(
                harness.config.clone(),
                vec![UserInput::Text {
                    text: "hello child".to_string(),
                    text_elements: Vec::new(),
                }],
                Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id,
                    depth: 1,
                    agent_nickname: None,
                    agent_role: Some("worker".to_string()),
                })),
            )
            .await
            .expect("child spawn should succeed");

        let child_thread = harness
            .manager
            .get_thread(child_thread_id)
            .await
            .expect("child thread should exist");
        let mut status_rx = harness
            .control
            .subscribe_status(child_thread_id)
            .await
            .expect("status subscription should succeed");
        if matches!(status_rx.borrow().clone(), AgentStatus::PendingInit) {
            timeout(Duration::from_secs(5), async {
                loop {
                    status_rx
                        .changed()
                        .await
                        .expect("child status should advance past pending init");
                    if !matches!(status_rx.borrow().clone(), AgentStatus::PendingInit) {
                        break;
                    }
                }
            })
            .await
            .expect("child should initialize before shutdown");
        }

        let original_snapshot = child_thread.config_snapshot().await;
        let original_nickname = original_snapshot
            .session_source
            .get_nickname()
            .expect("spawned sub-agent should have a nickname");
        let state_db = child_thread
            .state_db()
            .expect("sqlite state db should be available for resume test");
        timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(Some(metadata)) = state_db.get_thread(child_thread_id).await
                    && let Ok(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                        agent_nickname,
                        agent_role,
                        ..
                    })) = serde_json::from_str::<SessionSource>(&metadata.source)
                    && agent_nickname.is_some()
                    && agent_role.as_deref() == Some("worker")
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child thread metadata should be persisted before shutdown");

        let _ = harness
            .control
            .shutdown_agent(child_thread_id)
            .await
            .expect("child shutdown should submit");

        let resumed_thread_id = harness
            .control
            .resume_agent_from_rollout(
                harness.config.clone(),
                child_thread_id,
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id,
                    depth: 1,
                    agent_nickname: None,
                    agent_role: None,
                }),
            )
            .await
            .expect("resume should succeed");
        assert_eq!(resumed_thread_id, child_thread_id);

        let resumed_snapshot = harness
            .manager
            .get_thread(resumed_thread_id)
            .await
            .expect("resumed child thread should exist")
            .config_snapshot()
            .await;
        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: resumed_parent_thread_id,
            depth: resumed_depth,
            agent_nickname: resumed_nickname,
            agent_role: resumed_role,
        }) = resumed_snapshot.session_source
        else {
            panic!("expected thread-spawn sub-agent source");
        };
        assert_eq!(resumed_parent_thread_id, parent_thread_id);
        assert_eq!(resumed_depth, 1);
        assert_eq!(resumed_nickname, Some(original_nickname));
        assert_eq!(resumed_role, Some("worker".to_string()));

        let _ = harness
            .control
            .shutdown_agent(resumed_thread_id)
            .await
            .expect("resumed child shutdown should submit");
        wait_for_agent_status(&harness.control, resumed_thread_id, AgentStatus::Shutdown).await;
    }

    #[tokio::test]
    async fn spawn_agent_respects_max_threads_limit() {
        let max_threads = 1usize;
        let (_home, config) = test_config_with_cli_overrides(vec![(
            "agents.max_threads".to_string(),
            TomlValue::Integer(max_threads as i64),
        )])
        .await;
        let manager = ThreadManager::with_models_provider_and_home(
            CodexAuth::from_api_key("dummy"),
            config.model_provider_id.as_str(),
            config.model_provider.clone(),
            config.codex_home.clone(),
        );
        let control = manager.agent_control();

        let first_agent_id = control
            .spawn_agent(
                config.clone(),
                vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect("spawn_agent should succeed");

        let err = control
            .spawn_agent(
                config,
                vec![UserInput::Text {
                    text: "hello again".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect_err("spawn_agent should respect max threads");
        let CodexErr::AgentLimitReached {
            max_threads: seen_max_threads,
        } = err
        else {
            panic!("expected CodexErr::AgentLimitReached");
        };
        assert_eq!(seen_max_threads, max_threads);

        let _ = control
            .shutdown_agent(first_agent_id)
            .await
            .expect("shutdown agent");
    }

    #[tokio::test]
    async fn spawn_agent_releases_slot_after_shutdown() {
        let max_threads = 1usize;
        let (_home, config) = test_config_with_cli_overrides(vec![(
            "agents.max_threads".to_string(),
            TomlValue::Integer(max_threads as i64),
        )])
        .await;
        let manager = ThreadManager::with_models_provider_and_home(
            CodexAuth::from_api_key("dummy"),
            config.model_provider_id.as_str(),
            config.model_provider.clone(),
            config.codex_home.clone(),
        );
        let control = manager.agent_control();

        let first_agent_id = control
            .spawn_agent(
                config.clone(),
                vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect("spawn_agent should succeed");
        let _ = control
            .shutdown_agent(first_agent_id)
            .await
            .expect("shutdown agent");

        let second_agent_id = control
            .spawn_agent(
                config.clone(),
                vec![UserInput::Text {
                    text: "hello again".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect("spawn_agent should succeed after shutdown");
        let _ = control
            .shutdown_agent(second_agent_id)
            .await
            .expect("shutdown agent");
    }

    #[tokio::test]
    async fn spawn_agent_limit_shared_across_clones() {
        let max_threads = 1usize;
        let (_home, config) = test_config_with_cli_overrides(vec![(
            "agents.max_threads".to_string(),
            TomlValue::Integer(max_threads as i64),
        )])
        .await;
        let manager = ThreadManager::with_models_provider_and_home(
            CodexAuth::from_api_key("dummy"),
            config.model_provider_id.as_str(),
            config.model_provider.clone(),
            config.codex_home.clone(),
        );
        let control = manager.agent_control();
        let cloned = control.clone();

        let first_agent_id = cloned
            .spawn_agent(
                config.clone(),
                vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect("spawn_agent should succeed");

        let err = control
            .spawn_agent(
                config,
                vec![UserInput::Text {
                    text: "hello again".to_string(),
                    text_elements: Vec::new(),
                }],
                None,
            )
            .await
            .expect_err("spawn_agent should respect shared guard");
        let CodexErr::AgentLimitReached { max_threads } = err else {
            panic!("expected CodexErr::AgentLimitReached");
        };
        assert_eq!(max_threads, 1);

        let _ = control
            .shutdown_agent(first_agent_id)
            .await
            .expect("shutdown agent");
    }
}
