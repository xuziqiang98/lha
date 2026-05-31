use crate::client::TurnRuntime;
use crate::config::model_ref::ModelRef;
use crate::env::LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR;
use crate::env::LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR;
use crate::env::LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR;
use crate::env::LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR;
use crate::error::CodexErr;
use lha_llm::RuntimeEndpoint;
use lha_protocol::ThreadId;
use lha_protocol::config_types::WindowsSandboxLevel;
use lha_protocol::protocol::AgentJobDisplayStatus;
use lha_protocol::protocol::AgentJobKind;
use lha_protocol::protocol::AgentJobStatusEvent;
use lha_protocol::protocol::Event;
use lha_protocol::protocol::EventMsg;
use lha_protocol::protocol::SandboxPolicy;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEFAULT_JOB_MAX_RUNTIME_SECONDS: u64 = 3600;
const STDERR_TAIL_BYTES: usize = 4096;
const JOBS_DIR: &str = "agent_jobs";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentJobType {
    Explorer,
    Reviewer,
}

impl AgentJobType {
    fn identity_arg(&self) -> &'static str {
        match self {
            Self::Explorer => "explorer",
            Self::Reviewer => "reviewer",
        }
    }

    fn prompt_prefix(&self) -> Option<&'static str> {
        match self {
            Self::Explorer => None,
            Self::Reviewer => Some(crate::REVIEW_PROMPT),
        }
    }

    fn protocol_kind(&self) -> AgentJobKind {
        match self {
            Self::Explorer => AgentJobKind::Explorer,
            Self::Reviewer => AgentJobKind::Reviewer,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum AgentJobStatus {
    Running,
    Completed {
        result: String,
        exit_code: Option<i32>,
    },
    Failed {
        message: String,
        exit_code: Option<i32>,
    },
    Cancelled,
    TimedOut,
    NotFound,
}

impl AgentJobStatus {
    pub(crate) fn is_final(&self) -> bool {
        !matches!(self, Self::Running)
    }

    fn display_status(&self) -> AgentJobDisplayStatus {
        match self {
            Self::Running => AgentJobDisplayStatus::Running,
            Self::Completed { .. } => AgentJobDisplayStatus::Completed,
            Self::Failed { .. } => AgentJobDisplayStatus::Failed,
            Self::Cancelled => AgentJobDisplayStatus::Cancelled,
            Self::TimedOut => AgentJobDisplayStatus::TimedOut,
            Self::NotFound => AgentJobDisplayStatus::NotFound,
        }
    }

    fn display_message(&self) -> Option<String> {
        match self {
            Self::Failed { message, .. } if !message.trim().is_empty() => Some(message.clone()),
            Self::Running
            | Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled
            | Self::TimedOut
            | Self::NotFound => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AgentJobSnapshot {
    pub(crate) id: String,
    pub(crate) agent_type: AgentJobType,
    pub(crate) status: AgentJobStatus,
}

impl AgentJobSnapshot {
    pub(crate) fn status_event(&self) -> EventMsg {
        EventMsg::AgentJobStatus(AgentJobStatusEvent {
            job_id: self.id.clone(),
            agent_type: self.agent_type.protocol_kind(),
            status: self.status.display_status(),
            message: self.status.display_message(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentJobExecConfig {
    pub(crate) lha_home: PathBuf,
    pub(crate) model_arg: Option<String>,
    pub(crate) profile_arg: Option<String>,
    pub(crate) model_provider_id: String,
    pub(crate) model_provider: RuntimeEndpoint,
    pub(crate) auth_token: Option<String>,
    pub(crate) sandbox_policy: SandboxPolicy,
    pub(crate) windows_sandbox_level: WindowsSandboxLevel,
}

pub(crate) enum AgentJobOutputMode {
    LogOnly,
    RawEvents {
        progress_tx: mpsc::UnboundedSender<EventMsg>,
    },
}

pub(crate) struct AgentJobSpawnOptions {
    max_runtime_seconds: Option<u64>,
    output_mode: AgentJobOutputMode,
}

impl AgentJobSpawnOptions {
    pub(crate) fn log_only(max_runtime_seconds: Option<u64>) -> Self {
        Self {
            max_runtime_seconds,
            output_mode: AgentJobOutputMode::LogOnly,
        }
    }

    pub(crate) fn raw_events(
        max_runtime_seconds: Option<u64>,
        progress_tx: mpsc::UnboundedSender<EventMsg>,
    ) -> Self {
        Self {
            max_runtime_seconds,
            output_mode: AgentJobOutputMode::RawEvents { progress_tx },
        }
    }
}

impl AgentJobExecConfig {
    pub(crate) fn from_runtime(
        runtime: &TurnRuntime,
        model: &str,
        sandbox_policy: SandboxPolicy,
        windows_sandbox_level: WindowsSandboxLevel,
    ) -> Self {
        let config = runtime.config();
        let mut model_provider = runtime.endpoint();
        let auth_token = model_provider.bearer_token.take();
        Self {
            lha_home: config.lha_home.clone(),
            model_arg: Some(canonical_model_arg(&config.model_provider_id, model)),
            profile_arg: config.active_profile.clone(),
            model_provider_id: config.model_provider_id.clone(),
            model_provider,
            auth_token,
            sandbox_policy,
            windows_sandbox_level,
        }
    }
}

#[derive(Debug, Serialize)]
struct AgentJobProviderContext<'a> {
    model_provider_id: &'a str,
    model_provider: &'a RuntimeEndpoint,
}

#[derive(Debug, Serialize)]
struct AgentJobMetadata<'a> {
    id: &'a str,
    parent_thread_id: ThreadId,
    agent_type: &'a AgentJobType,
    cwd: &'a Path,
    created_at_unix_seconds: u64,
    max_runtime_seconds: u64,
}

fn canonical_model_arg(model_provider_id: &str, model: &str) -> String {
    if ModelRef::parse(model).is_ok() {
        return model.to_string();
    }

    if model_provider_id.contains('.') {
        format!("{model_provider_id}:{model}")
    } else {
        format!("{model_provider_id}.main:{model}")
    }
}

#[derive(Debug)]
struct ManagedAgentJob {
    agent_type: AgentJobType,
    status: Arc<Mutex<AgentJobStatus>>,
    cancellation_token: CancellationToken,
    completion: watch::Receiver<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentJobManager {
    jobs: Arc<Mutex<HashMap<String, ManagedAgentJob>>>,
    semaphore: Arc<Semaphore>,
    configured_max_runtime: Duration,
    exec_bin_override: Option<PathBuf>,
    lha_home: PathBuf,
}

impl AgentJobManager {
    pub(crate) fn new(
        lha_home: PathBuf,
        max_jobs: Option<usize>,
        default_max_runtime_seconds: Option<u64>,
    ) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(max_jobs.unwrap_or(usize::MAX))),
            configured_max_runtime: Duration::from_secs(
                default_max_runtime_seconds.unwrap_or(DEFAULT_JOB_MAX_RUNTIME_SECONDS),
            ),
            exec_bin_override: None,
            lha_home,
        }
    }

    #[cfg(test)]
    fn with_exec_bin_for_tests(mut self, exec_bin: PathBuf) -> Self {
        self.exec_bin_override = Some(exec_bin);
        self
    }

    pub(crate) async fn spawn(
        &self,
        parent_thread_id: ThreadId,
        agent_type: AgentJobType,
        prompt: String,
        cwd: PathBuf,
        exec_config: AgentJobExecConfig,
        options: AgentJobSpawnOptions,
    ) -> Result<AgentJobSnapshot, CodexErr> {
        let AgentJobSpawnOptions {
            max_runtime_seconds,
            output_mode,
        } = options;
        let max_runtime = self.resolve_max_runtime(max_runtime_seconds)?;
        let permit = self.semaphore.clone().try_acquire_owned().map_err(|_| {
            CodexErr::UnsupportedOperation("agent job concurrency limit reached".to_string())
        })?;
        let id = format!("agent-job-{}", Uuid::new_v4());
        let job_dir = create_job_dir(&self.lha_home, parent_thread_id, &id).await?;
        let prompt_path = job_dir.join("prompt.txt");
        let metadata_path = job_dir.join("metadata.json");
        let status_path = job_dir.join("status.json");
        let result_path = job_dir.join("result.txt");
        let stdout_path = job_dir.join("stdout.log");
        let stderr_path = job_dir.join("stderr.log");
        drop(create_private_std_file(&result_path, "agent job result")?);
        let prompt = agent_type
            .prompt_prefix()
            .map(|prefix| format!("{prefix}\n\n{prompt}"))
            .unwrap_or(prompt);
        write_private_file(&prompt_path, prompt.as_bytes(), "agent job prompt").await?;
        let metadata = AgentJobMetadata {
            id: &id,
            parent_thread_id,
            agent_type: &agent_type,
            cwd: cwd.as_path(),
            created_at_unix_seconds: unix_timestamp_seconds(),
            max_runtime_seconds: max_runtime.as_secs(),
        };
        write_json_private_file(&metadata_path, &metadata, "agent job metadata").await?;
        let mut command = match build_lha_exec_command(
            self.exec_bin_override.as_ref(),
            &agent_type,
            &cwd,
            &exec_config,
            &result_path,
            matches!(output_mode, AgentJobOutputMode::RawEvents { .. }),
        ) {
            Ok(command) => command,
            Err(err) => {
                let status = AgentJobStatus::Failed {
                    message: err.to_string(),
                    exit_code: None,
                };
                persist_status(&status_path, &status).await?;
                return Err(err);
            }
        };
        persist_status(&status_path, &AgentJobStatus::Running).await?;
        command.stdin(Stdio::piped());
        match &output_mode {
            AgentJobOutputMode::LogOnly => {
                command.stdout(Stdio::from(create_private_std_file(
                    &stdout_path,
                    "agent job stdout log",
                )?));
            }
            AgentJobOutputMode::RawEvents { .. } => {
                drop(create_private_std_file(
                    &stdout_path,
                    "agent job stdout log",
                )?);
                command.stdout(Stdio::piped());
            }
        }
        command.stderr(Stdio::from(create_private_std_file(
            &stderr_path,
            "agent job stderr log",
        )?));

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                let status = AgentJobStatus::Failed {
                    message: format!("failed to spawn lha exec agent job: {err}"),
                    exit_code: None,
                };
                if let Err(status_err) = persist_status(&status_path, &status).await {
                    tracing::warn!("failed to persist agent job spawn failure: {status_err}");
                }
                return Err(CodexErr::Fatal(format!(
                    "failed to spawn lha exec agent job: {err}"
                )));
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let prompt_for_stdin = prompt.clone();
            tokio::spawn(async move {
                let _ = stdin.write_all(prompt_for_stdin.as_bytes()).await;
            });
        }
        let stdout_task = match output_mode {
            AgentJobOutputMode::LogOnly => None,
            AgentJobOutputMode::RawEvents { progress_tx } => child.stdout.take().map(|stdout| {
                spawn_raw_event_stdout_reader(stdout, stdout_path.clone(), progress_tx)
            }),
        };

        let status = Arc::new(Mutex::new(AgentJobStatus::Running));
        let cancellation_token = CancellationToken::new();
        let (completion_tx, completion_rx) = watch::channel(false);
        self.jobs.lock().await.insert(
            id.clone(),
            ManagedAgentJob {
                agent_type: agent_type.clone(),
                status: Arc::clone(&status),
                cancellation_token: cancellation_token.clone(),
                completion: completion_rx,
            },
        );

        spawn_job_watcher(AgentJobWatcher {
            _id: id.clone(),
            status,
            child,
            cancellation_token,
            result_path,
            stderr_path,
            status_path,
            stdout_task,
            max_runtime,
            completion_tx,
            _permit: permit,
        });

        Ok(AgentJobSnapshot {
            id,
            agent_type,
            status: AgentJobStatus::Running,
        })
    }

    pub(crate) async fn status(&self, id: &str) -> AgentJobSnapshot {
        let jobs = self.jobs.lock().await;
        let Some(job) = jobs.get(id) else {
            return AgentJobSnapshot {
                id: id.to_string(),
                agent_type: AgentJobType::Explorer,
                status: AgentJobStatus::NotFound,
            };
        };
        AgentJobSnapshot {
            id: id.to_string(),
            agent_type: job.agent_type.clone(),
            status: job.status.lock().await.clone(),
        }
    }

    pub(crate) async fn wait(&self, ids: &[String], timeout: Duration) -> Vec<AgentJobSnapshot> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let mut snapshots = Vec::with_capacity(ids.len());
            let mut all_final = true;
            for id in ids {
                let snapshot = self.status(id).await;
                all_final &= snapshot.status.is_final();
                snapshots.push(snapshot);
            }
            if all_final || tokio::time::Instant::now() >= deadline {
                return snapshots;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub(crate) async fn close(&self, id: &str) -> AgentJobSnapshot {
        let Some(handle) = ({
            let jobs = self.jobs.lock().await;
            jobs.get(id).map(JobCloseHandle::from)
        }) else {
            return AgentJobSnapshot {
                id: id.to_string(),
                agent_type: AgentJobType::Explorer,
                status: AgentJobStatus::NotFound,
            };
        };
        handle.cancel_and_wait(id.to_string()).await
    }

    pub(crate) async fn close_all(&self) -> Vec<AgentJobSnapshot> {
        let handles = {
            let jobs = self.jobs.lock().await;
            jobs.iter()
                .map(|(id, job)| (id.clone(), JobCloseHandle::from(job)))
                .collect::<Vec<_>>()
        };

        for (_, handle) in &handles {
            if !handle.status.lock().await.is_final() {
                handle.cancellation_token.cancel();
            }
        }

        let mut snapshots = Vec::with_capacity(handles.len());
        for (id, handle) in handles {
            snapshots.push(handle.wait_for_completion(id).await);
        }
        snapshots
    }

    fn resolve_max_runtime(&self, requested_seconds: Option<u64>) -> Result<Duration, CodexErr> {
        match requested_seconds {
            Some(0) => Err(CodexErr::UnsupportedOperation(
                "max_runtime_seconds must be at least 1".to_string(),
            )),
            Some(seconds) => Ok(Duration::from_secs(seconds).min(self.configured_max_runtime)),
            None => Ok(self.configured_max_runtime),
        }
    }
}

struct JobCloseHandle {
    agent_type: AgentJobType,
    status: Arc<Mutex<AgentJobStatus>>,
    cancellation_token: CancellationToken,
    completion: watch::Receiver<bool>,
}

impl From<&ManagedAgentJob> for JobCloseHandle {
    fn from(job: &ManagedAgentJob) -> Self {
        Self {
            agent_type: job.agent_type.clone(),
            status: Arc::clone(&job.status),
            cancellation_token: job.cancellation_token.clone(),
            completion: job.completion.clone(),
        }
    }
}

impl JobCloseHandle {
    async fn cancel_and_wait(self, id: String) -> AgentJobSnapshot {
        if !self.status.lock().await.is_final() {
            self.cancellation_token.cancel();
        }
        self.wait_for_completion(id).await
    }

    async fn wait_for_completion(self, id: String) -> AgentJobSnapshot {
        if !self.status.lock().await.is_final() {
            let mut completion = self.completion;
            while !*completion.borrow_and_update() {
                if completion.changed().await.is_err() {
                    break;
                }
            }
        }

        AgentJobSnapshot {
            id,
            agent_type: self.agent_type,
            status: self.status.lock().await.clone(),
        }
    }
}

async fn create_job_dir(
    lha_home: &Path,
    parent_thread_id: ThreadId,
    id: &str,
) -> Result<PathBuf, CodexErr> {
    let root = lha_home.join(JOBS_DIR);
    let session_dir = root.join(parent_thread_id.to_string());
    let job_dir = session_dir.join(id);
    for path in [&root, &session_dir, &job_dir] {
        create_private_dir(path).await?;
    }
    Ok(job_dir)
}

async fn create_private_dir(path: &Path) -> Result<(), CodexErr> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|err| CodexErr::Fatal(format!("failed to create agent job dir: {err}")))?;
    set_private_dir_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), CodexErr> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| CodexErr::Fatal(format!("failed to set agent job dir permissions: {err}")))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), CodexErr> {
    Ok(())
}

fn create_private_std_file(path: &Path, description: &str) -> Result<std::fs::File, CodexErr> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|err| CodexErr::Fatal(format!("failed to create {description}: {err}")))?;
    set_private_file_permissions(path, description)?;
    Ok(file)
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path, description: &str) -> Result<(), CodexErr> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| CodexErr::Fatal(format!("failed to set {description} permissions: {err}")))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path, _description: &str) -> Result<(), CodexErr> {
    Ok(())
}

async fn write_private_file(
    path: &Path,
    contents: &[u8],
    description: &str,
) -> Result<(), CodexErr> {
    let file = create_private_std_file(path, description)?;
    let mut file = tokio::fs::File::from_std(file);
    file.write_all(contents)
        .await
        .map_err(|err| CodexErr::Fatal(format!("failed to write {description}: {err}")))?;
    file.flush()
        .await
        .map_err(|err| CodexErr::Fatal(format!("failed to flush {description}: {err}")))?;
    Ok(())
}

async fn write_json_private_file<T: Serialize>(
    path: &Path,
    value: &T,
    description: &str,
) -> Result<(), CodexErr> {
    let json = serde_json::to_vec_pretty(value)
        .map_err(|err| CodexErr::Fatal(format!("failed to serialize {description}: {err}")))?;
    write_private_file(path, &json, description).await
}

async fn persist_status(path: &Path, status: &AgentJobStatus) -> Result<(), CodexErr> {
    write_json_private_file(path, status, "agent job status").await
}

fn unix_timestamp_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn build_lha_exec_command(
    exec_bin_override: Option<&PathBuf>,
    agent_type: &AgentJobType,
    cwd: &PathBuf,
    exec_config: &AgentJobExecConfig,
    result_path: &std::path::Path,
    internal_raw_events: bool,
) -> Result<Command, CodexErr> {
    let program = resolve_lha_exec_program(exec_bin_override)?;
    let mut command = Command::new(&program);
    let file_name = program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !file_name.contains("lha-exec") {
        command.arg("exec");
    }
    if let Some(profile) = exec_config.profile_arg.as_deref() {
        command.arg("--profile").arg(profile);
    }
    if let Some(model) = exec_config.model_arg.as_deref() {
        command.arg("--model").arg(model);
    }
    let provider_context = AgentJobProviderContext {
        model_provider_id: &exec_config.model_provider_id,
        model_provider: &exec_config.model_provider,
    };
    let provider_context_json = serde_json::to_string(&provider_context)
        .map_err(|err| CodexErr::Fatal(format!("failed to serialize agent job context: {err}")))?;
    command.env("LHA_HOME", &exec_config.lha_home);
    command.env(
        LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR,
        provider_context_json,
    );
    if let Some(auth_token) = exec_config.auth_token.as_deref() {
        command.env(LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR, auth_token);
    }
    let sandbox_policy_json = serde_json::to_string(&exec_config.sandbox_policy)
        .map_err(|err| CodexErr::Fatal(format!("failed to serialize agent job sandbox: {err}")))?;
    let windows_sandbox_level_json = serde_json::to_string(&exec_config.windows_sandbox_level)
        .map_err(|err| {
            CodexErr::Fatal(format!(
                "failed to serialize agent job windows sandbox level: {err}"
            ))
        })?;
    command.env(LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR, sandbox_policy_json);
    command.env(
        LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR,
        windows_sandbox_level_json,
    );
    if internal_raw_events {
        command.arg("--internal-raw-events");
    }
    command
        .arg("--identity")
        .arg(agent_type.identity_arg())
        .arg("--skip-git-repo-check")
        .arg("--color")
        .arg("never")
        .arg("--output-last-message")
        .arg(result_path)
        .arg("--cd")
        .arg(cwd)
        .arg("-");
    Ok(command)
}

fn spawn_raw_event_stdout_reader(
    stdout: tokio::process::ChildStdout,
    stdout_path: PathBuf,
    progress_tx: mpsc::UnboundedSender<EventMsg>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let log_file = match create_private_std_file(&stdout_path, "agent job stdout log") {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("failed to open agent job stdout log: {err}");
                return;
            }
        };
        let mut log_file = tokio::fs::File::from_std(log_file);
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Err(err) = log_file.write_all(line.as_bytes()).await {
                        tracing::warn!("failed to write agent job stdout log: {err}");
                    }
                    if let Err(err) = log_file.write_all(b"\n").await {
                        tracing::warn!("failed to write agent job stdout newline: {err}");
                    }
                    match serde_json::from_str::<Event>(&line) {
                        Ok(event) => {
                            let _ = progress_tx.send(event.msg);
                        }
                        Err(err) => {
                            tracing::debug!("ignoring non-event agent job stdout line: {err}");
                        }
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!("failed to read agent job stdout: {err}");
                    break;
                }
            }
        }
        if let Err(err) = log_file.flush().await {
            tracing::warn!("failed to flush agent job stdout log: {err}");
        }
    })
}

fn resolve_lha_exec_program(exec_bin_override: Option<&PathBuf>) -> Result<PathBuf, CodexErr> {
    if let Some(path) = exec_bin_override {
        return Ok(path.clone());
    }
    if let Some(path) = std::env::var_os("LHA_AGENT_EXEC_BIN") {
        return Ok(PathBuf::from(path));
    }

    let current_exe = std::env::current_exe()
        .map_err(|err| CodexErr::Fatal(format!("failed to resolve current executable: {err}")))?;
    if executable_supports_exec(&current_exe) {
        return Ok(current_exe);
    }
    if let Some(path) = sibling_binary(&current_exe, "lha-exec") {
        return Ok(path);
    }
    if let Some(path) = sibling_binary(&current_exe, "lha") {
        return Ok(path);
    }
    if let Ok(path) = which::which("lha-exec") {
        return Ok(path);
    }
    if let Ok(path) = which::which("lha") {
        return Ok(path);
    }
    Err(CodexErr::Fatal(
        "lha exec not found; install lha-exec/lha or set LHA_AGENT_EXEC_BIN".to_string(),
    ))
}

fn sibling_binary(current_exe: &std::path::Path, name: &str) -> Option<PathBuf> {
    let executable_name = executable_name(name);
    let current_dir = current_exe.parent()?;
    for dir in [Some(current_dir), current_dir.parent()] {
        let candidate = dir?.join(&executable_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn executable_supports_exec(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    file_name == executable_name("lha-exec") || file_name == executable_name("lha")
}

struct AgentJobWatcher {
    _id: String,
    status: Arc<Mutex<AgentJobStatus>>,
    child: Child,
    cancellation_token: CancellationToken,
    result_path: PathBuf,
    stderr_path: PathBuf,
    status_path: PathBuf,
    stdout_task: Option<JoinHandle<()>>,
    max_runtime: Duration,
    completion_tx: watch::Sender<bool>,
    _permit: OwnedSemaphorePermit,
}

fn spawn_job_watcher(watcher: AgentJobWatcher) {
    tokio::spawn(async move {
        let AgentJobWatcher {
            _id: _,
            status,
            mut child,
            cancellation_token,
            result_path,
            stderr_path,
            status_path,
            stdout_task,
            max_runtime,
            completion_tx,
            _permit,
        } = watcher;
        let timeout = tokio::time::sleep(max_runtime);
        tokio::pin!(timeout);
        let next_status = tokio::select! {
            wait_result = child.wait() => match wait_result {
                Ok(exit_status) => {
                    let exit_code = exit_status.code();
                    if exit_status.success() {
                        match tokio::fs::read_to_string(&result_path).await {
                            Ok(result) if !result.trim().is_empty() => {
                                AgentJobStatus::Completed { result, exit_code }
                            }
                            _ => AgentJobStatus::Failed {
                                message: "agent job exited successfully but produced no final message"
                                    .to_string(),
                                exit_code,
                            },
                        }
                    } else {
                        AgentJobStatus::Failed {
                            message: stderr_tail(&stderr_path).await,
                            exit_code,
                        }
                    }
                }
                Err(err) => AgentJobStatus::Failed {
                    message: format!("failed to wait for agent job: {err}"),
                    exit_code: None,
                },
            },
            () = &mut timeout => {
                let _ = child.kill().await;
                AgentJobStatus::TimedOut
            }
            () = cancellation_token.cancelled() => {
                let _ = child.kill().await;
                AgentJobStatus::Cancelled
            }
        };
        if let Some(stdout_task) = stdout_task
            && let Err(err) = stdout_task.await
        {
            tracing::warn!("agent job stdout reader failed: {err}");
        }
        let mut status_guard = status.lock().await;
        *status_guard = next_status.clone();
        drop(status_guard);
        if let Err(err) = persist_status(&status_path, &next_status).await {
            tracing::warn!("failed to persist agent job status: {err}");
        }
        let _ = completion_tx.send(true);
    });
}

async fn stderr_tail(path: &std::path::Path) -> String {
    match tokio::fs::read(path).await {
        Ok(bytes) if !bytes.is_empty() => {
            let start = bytes.len().saturating_sub(STDERR_TAIL_BYTES);
            String::from_utf8_lossy(&bytes[start..]).to_string()
        }
        _ => "agent job failed without stderr output".to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn write_script(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap_or_else(|err| panic!("write fake lha exec: {err}"));
        let mut permissions = std::fs::metadata(&path)
            .unwrap_or_else(|err| panic!("metadata: {err}"))
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)
            .unwrap_or_else(|err| panic!("chmod fake lha exec: {err}"));
        path
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .unwrap_or_else(|err| panic!("metadata {}: {err}", path.display()))
            .permissions()
            .mode()
            & 0o777
    }

    fn test_exec_config(lha_home: &Path, model_arg: &str) -> AgentJobExecConfig {
        AgentJobExecConfig {
            lha_home: lha_home.to_path_buf(),
            model_arg: Some(model_arg.to_string()),
            profile_arg: None,
            model_provider_id: "test-provider.main".to_string(),
            model_provider: RuntimeEndpoint::openai_compatible_responses(
                "test-provider",
                "http://127.0.0.1:9/v1",
            ),
            auth_token: None,
            sandbox_policy: SandboxPolicy::ReadOnly,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
        }
    }

    #[test]
    fn canonical_model_arg_uses_main_endpoint_for_provider_id() {
        assert_eq!(
            canonical_model_arg("openai", "gpt-5.2"),
            "openai.main:gpt-5.2"
        );
    }

    #[test]
    fn canonical_model_arg_preserves_provider_endpoint_id() {
        assert_eq!(
            canonical_model_arg("anthropic.messages", "claude-sonnet-4"),
            "anthropic.messages:claude-sonnet-4"
        );
    }

    #[test]
    fn snapshot_status_event_omits_completed_result() {
        let snapshot = AgentJobSnapshot {
            id: "agent-job-1".to_string(),
            agent_type: AgentJobType::Explorer,
            status: AgentJobStatus::Completed {
                result: "large final answer".to_string(),
                exit_code: Some(0),
            },
        };

        let EventMsg::AgentJobStatus(event) = snapshot.status_event() else {
            panic!("expected agent job status event");
        };
        assert_eq!(
            event,
            AgentJobStatusEvent {
                job_id: "agent-job-1".to_string(),
                agent_type: AgentJobKind::Explorer,
                status: AgentJobDisplayStatus::Completed,
                message: None,
            }
        );
    }

    #[test]
    fn canonical_model_arg_preserves_existing_model_ref() {
        assert_eq!(
            canonical_model_arg("openai", "openrouter.main:anthropic/claude-sonnet-4"),
            "openrouter.main:anthropic/claude-sonnet-4"
        );
    }

    #[test]
    fn executable_supports_exec_only_accepts_lha_or_lha_exec() {
        assert!(executable_supports_exec(Path::new(&executable_name("lha"))));
        assert!(executable_supports_exec(Path::new(&executable_name(
            "lha-exec"
        ))));
        assert!(!executable_supports_exec(Path::new("lha-tui")));
        assert!(!executable_supports_exec(Path::new("lha-app-server")));
        assert!(!executable_supports_exec(Path::new("lha-agent-tests")));
    }

    #[tokio::test]
    async fn job_completes_with_result_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let script = write_script(
            &dir,
            "lha-exec-fake",
            r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
cat >/dev/null
printf "explorer result" > "$out"
"#,
        );
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(1), Some(5))
            .with_exec_bin_for_tests(script);
        let parent_thread_id = ThreadId::new();
        let snapshot = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect this".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::log_only(None),
            )
            .await
            .expect("spawn job");
        let snapshots = manager
            .wait(std::slice::from_ref(&snapshot.id), Duration::from_secs(5))
            .await;

        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].agent_type, AgentJobType::Explorer);
        assert_eq!(
            snapshots[0].status,
            AgentJobStatus::Completed {
                result: "explorer result".to_string(),
                exit_code: Some(0)
            }
        );

        let job_dir = lha_home
            .path()
            .join(JOBS_DIR)
            .join(parent_thread_id.to_string())
            .join(&snapshot.id);
        assert!(job_dir.join("metadata.json").is_file());
        assert!(job_dir.join("prompt.txt").is_file());
        assert!(job_dir.join("stdout.log").is_file());
        assert!(job_dir.join("stderr.log").is_file());
        assert!(job_dir.join("result.txt").is_file());
        let persisted_status = serde_json::from_str::<AgentJobStatus>(
            &std::fs::read_to_string(job_dir.join("status.json")).expect("status json"),
        )
        .expect("parse status json");
        assert_eq!(persisted_status, snapshots[0].status);
        assert_eq!(mode(&job_dir), 0o700);
        assert_eq!(mode(&job_dir.join("prompt.txt")), 0o600);
        assert_eq!(mode(&job_dir.join("stdout.log")), 0o600);
        assert_eq!(mode(&job_dir.join("stderr.log")), 0o600);
        assert_eq!(mode(&job_dir.join("result.txt")), 0o600);
        assert_eq!(mode(&job_dir.join("status.json")), 0o600);
        assert_eq!(mode(&job_dir.join("metadata.json")), 0o600);
    }

    #[tokio::test]
    async fn raw_event_mode_forwards_events_and_logs_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let args_log = dir.path().join("args.txt");
        let script = write_script(
            &dir,
            "lha-exec-fake",
            &format!(
                r#"#!/bin/sh
out=""
: > "{args_log}"
while [ "$#" -gt 0 ]; do
  printf "%s\n" "$1" >> "{args_log}"
  if [ "$1" = "--output-last-message" ]; then
    out="$2"
    printf "%s\n" "$2" >> "{args_log}"
    shift 2
  else
    shift
  fi
done
cat >/dev/null
printf "%s\n" "not json"
printf "%s\n" '{{"id":"child-reasoning","msg":{{"type":"agent_reasoning","text":"checking changes"}}}}'
printf "raw mode result" > "$out"
"#,
                args_log = args_log.display()
            ),
        );
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(1), Some(5))
            .with_exec_bin_for_tests(script);
        let parent_thread_id = ThreadId::new();
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        let snapshot = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect this".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::raw_events(None, progress_tx),
            )
            .await
            .expect("spawn job");

        let event = tokio::time::timeout(Duration::from_secs(5), progress_rx.recv())
            .await
            .expect("timed out waiting for raw event")
            .expect("raw event channel should be open");
        match event {
            EventMsg::AgentReasoning(reasoning) => {
                assert_eq!(reasoning.text, "checking changes");
            }
            other => panic!("expected AgentReasoning, got {other:?}"),
        }

        let snapshots = manager
            .wait(std::slice::from_ref(&snapshot.id), Duration::from_secs(5))
            .await;
        assert_eq!(
            snapshots[0].status,
            AgentJobStatus::Completed {
                result: "raw mode result".to_string(),
                exit_code: Some(0)
            }
        );

        let args = std::fs::read_to_string(args_log).expect("args log");
        assert!(
            args.lines().any(|arg| arg == "--internal-raw-events"),
            "expected raw event mode arg: {args:?}"
        );

        let stdout_path = lha_home
            .path()
            .join(JOBS_DIR)
            .join(parent_thread_id.to_string())
            .join(&snapshot.id)
            .join("stdout.log");
        let stdout_log = std::fs::read_to_string(&stdout_path).expect("stdout log");
        assert!(stdout_log.contains("not json"));
        assert!(stdout_log.contains("checking changes"));
        assert_eq!(mode(&stdout_path), 0o600);
    }

    #[tokio::test]
    async fn requested_runtime_is_capped_by_configured_max() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let script = write_script(
            &dir,
            "lha-exec-fake",
            r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
cat >/dev/null
printf "runtime result" > "$out"
"#,
        );
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(1), Some(3))
            .with_exec_bin_for_tests(script);
        let parent_thread_id = ThreadId::new();
        let snapshot = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect this".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::log_only(Some(9999)),
            )
            .await
            .expect("spawn job");
        let _ = manager
            .wait(std::slice::from_ref(&snapshot.id), Duration::from_secs(5))
            .await;

        let metadata_path = lha_home
            .path()
            .join(JOBS_DIR)
            .join(parent_thread_id.to_string())
            .join(&snapshot.id)
            .join("metadata.json");
        let metadata = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(metadata_path).unwrap(),
        )
        .expect("metadata json");

        assert_eq!(metadata["max_runtime_seconds"], serde_json::json!(3));
    }

    #[tokio::test]
    async fn zero_requested_runtime_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(1), Some(5));

        let err = manager
            .spawn(
                ThreadId::new(),
                AgentJobType::Explorer,
                "inspect this".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::log_only(Some(0)),
            )
            .await
            .expect_err("zero max runtime should fail");

        assert_eq!(
            err.to_string(),
            "unsupported operation: max_runtime_seconds must be at least 1"
        );
    }

    #[tokio::test]
    async fn job_passes_profile_and_model_ref_to_lha_exec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let script = write_script(
            &dir,
            "lha-exec-fake",
            r#"#!/bin/sh
script_dir=$(dirname "$0")
out=""
: > "$script_dir/args.txt"
{
  printf "LHA_HOME=%s\n" "$LHA_HOME"
  printf "AUTH_TOKEN=%s\n" "$LHA_AGENT_JOB_AUTH_TOKEN"
  printf "PROVIDER_CONTEXT=%s\n" "$LHA_AGENT_JOB_PROVIDER_CONTEXT"
  printf "SANDBOX_POLICY=%s\n" "$LHA_AGENT_JOB_SANDBOX_POLICY"
  printf "WINDOWS_SANDBOX_LEVEL=%s\n" "$LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL"
} > "$script_dir/env.txt"
for arg in "$@"; do
  printf "%s\n" "$arg" >> "$script_dir/args.txt"
done
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
cat >/dev/null
printf "args result" > "$out"
"#,
        );
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(1), Some(5))
            .with_exec_bin_for_tests(script);
        let parent_thread_id = ThreadId::new();
        let snapshot = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect this".to_string(),
                dir.path().to_path_buf(),
                AgentJobExecConfig {
                    lha_home: lha_home.path().to_path_buf(),
                    model_arg: Some("provider-a.main:model-a".to_string()),
                    profile_arg: Some("work".to_string()),
                    model_provider_id: "provider-a.main".to_string(),
                    model_provider: RuntimeEndpoint::openai_compatible_responses(
                        "provider-a",
                        "http://127.0.0.1:9/v1",
                    ),
                    auth_token: Some("secret-token".to_string()),
                    sandbox_policy: SandboxPolicy::WorkspaceWrite {
                        writable_roots: vec![],
                        network_access: false,
                        exclude_tmpdir_env_var: false,
                        exclude_slash_tmp: false,
                    },
                    windows_sandbox_level: WindowsSandboxLevel::Disabled,
                },
                AgentJobSpawnOptions::log_only(None),
            )
            .await
            .expect("spawn job");
        let snapshots = manager
            .wait(std::slice::from_ref(&snapshot.id), Duration::from_secs(5))
            .await;

        assert!(matches!(
            &snapshots[0].status,
            AgentJobStatus::Completed { .. }
        ));
        let args = std::fs::read_to_string(dir.path().join("args.txt")).expect("args log");
        let args = args.lines().collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|window| window == ["--profile", "work"])
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["--model", "provider-a.main:model-a"])
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["--identity", "explorer"])
        );
        assert!(
            !args.contains(&"--sandbox"),
            "agent jobs should inherit sandbox via env, not CLI args: {args:?}"
        );
        assert!(
            !args.contains(&"read-only"),
            "agent jobs should not force read-only sandbox args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["--cd", dir.path().to_str().expect("utf-8 path")])
        );
        assert!(
            !args.contains(&"--internal-raw-events"),
            "log-only jobs should not request raw event output: {args:?}"
        );

        let child_env = std::fs::read_to_string(dir.path().join("env.txt")).expect("child env log");
        assert!(child_env.contains(&format!("LHA_HOME={}", lha_home.path().display())));
        assert!(child_env.contains("AUTH_TOKEN=secret-token"));
        assert!(child_env.contains("provider-a.main"));
        assert!(child_env.contains("SANDBOX_POLICY={\"type\":\"workspace-write\""));
        assert!(child_env.contains("WINDOWS_SANDBOX_LEVEL=\"disabled\""));

        let job_dir = lha_home
            .path()
            .join(JOBS_DIR)
            .join(parent_thread_id.to_string())
            .join(&snapshot.id);
        for name in [
            "metadata.json",
            "prompt.txt",
            "stdout.log",
            "stderr.log",
            "result.txt",
            "status.json",
        ] {
            let contents = std::fs::read_to_string(job_dir.join(name)).expect("job log file");
            assert!(
                !contents.contains("secret-token"),
                "{name} should not persist delegated job auth token"
            );
        }
    }

    #[tokio::test]
    async fn close_cancels_running_job() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let script = write_script(
            &dir,
            "lha-exec-fake",
            r#"#!/bin/sh
exec sleep 30
"#,
        );
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(1), Some(30))
            .with_exec_bin_for_tests(script);
        let parent_thread_id = ThreadId::new();
        let snapshot = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect this".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::log_only(None),
            )
            .await
            .expect("spawn job");

        let closed = manager.close(&snapshot.id).await;

        assert_eq!(closed.status, AgentJobStatus::Cancelled);

        let persisted_status = serde_json::from_str::<AgentJobStatus>(
            &std::fs::read_to_string(
                lha_home
                    .path()
                    .join(JOBS_DIR)
                    .join(parent_thread_id.to_string())
                    .join(&snapshot.id)
                    .join("status.json"),
            )
            .expect("status json"),
        )
        .expect("parse status json");
        assert_eq!(persisted_status, AgentJobStatus::Cancelled);
    }

    #[tokio::test]
    async fn close_all_cancels_running_jobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lha_home = tempfile::tempdir().expect("lha home");
        let script = write_script(
            &dir,
            "lha-exec-fake",
            r#"#!/bin/sh
exec sleep 30
"#,
        );
        let manager = AgentJobManager::new(lha_home.path().to_path_buf(), Some(2), Some(30))
            .with_exec_bin_for_tests(script);
        let parent_thread_id = ThreadId::new();
        let first = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect first".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::log_only(None),
            )
            .await
            .expect("spawn first job");
        let second = manager
            .spawn(
                parent_thread_id,
                AgentJobType::Explorer,
                "inspect second".to_string(),
                dir.path().to_path_buf(),
                test_exec_config(lha_home.path(), "test-provider.main:test-model"),
                AgentJobSpawnOptions::log_only(None),
            )
            .await
            .expect("spawn second job");

        let closed = manager.close_all().await;

        assert_eq!(closed.len(), 2);
        assert!(
            closed
                .iter()
                .all(|snapshot| snapshot.status == AgentJobStatus::Cancelled)
        );
        for snapshot in [&first, &second] {
            let persisted_status = serde_json::from_str::<AgentJobStatus>(
                &std::fs::read_to_string(
                    lha_home
                        .path()
                        .join(JOBS_DIR)
                        .join(parent_thread_id.to_string())
                        .join(&snapshot.id)
                        .join("status.json"),
                )
                .expect("status json"),
            )
            .expect("parse status json");
            assert_eq!(persisted_status, AgentJobStatus::Cancelled);
        }
    }
}
