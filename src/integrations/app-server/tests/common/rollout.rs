use anyhow::Result;
use lha_protocol::ThreadId;
use lha_protocol::config_types::ReasoningSummary;
use lha_protocol::protocol::AskForApproval;
use lha_protocol::protocol::GitInfo;
use lha_protocol::protocol::RolloutItem;
use lha_protocol::protocol::RolloutLine;
use lha_protocol::protocol::SandboxPolicy;
use lha_protocol::protocol::SessionMeta;
use lha_protocol::protocol::SessionMetaLine;
use lha_protocol::protocol::SessionSource;
use lha_protocol::protocol::TurnContextItem;
use lha_protocol::protocol::current_rollout_schema_version;
use serde_json::json;
use std::fs;
use std::fs::FileTimes;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

pub fn rollout_path(lha_home: &Path, filename_ts: &str, thread_id: &str) -> PathBuf {
    let year = &filename_ts[0..4];
    let month = &filename_ts[5..7];
    let day = &filename_ts[8..10];
    lha_home
        .join("sessions")
        .join(year)
        .join(month)
        .join(day)
        .join(format!("rollout-{filename_ts}-{thread_id}.jsonl"))
}

/// Create a minimal rollout file under `LHA_HOME/sessions/YYYY/MM/DD/`.
///
/// - `filename_ts` is the filename timestamp component in `YYYY-MM-DDThh-mm-ss` format.
/// - `meta_rfc3339` is the envelope timestamp used in JSON lines.
/// - `preview` is the user message preview text.
/// - `model_provider` optionally sets the provider in the session meta payload.
///
/// Returns the generated conversation/session UUID as a string.
pub fn create_fake_rollout(
    lha_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
) -> Result<String> {
    create_fake_rollout_with_source(
        lha_home,
        filename_ts,
        meta_rfc3339,
        preview,
        model_provider,
        git_info,
        SessionSource::Cli,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_fake_rollout_with_cwds(
    lha_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
    session_cwd: PathBuf,
    latest_turn_context_cwd: Option<PathBuf>,
) -> Result<String> {
    let uuid = Uuid::new_v4();
    let uuid_str = uuid.to_string();
    let conversation_id = ThreadId::from_string(&uuid_str)?;

    let file_path = rollout_path(lha_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(dir)?;

    let meta = SessionMeta {
        id: conversation_id,
        forked_from_id: None,
        timestamp: meta_rfc3339.to_string(),
        cwd: session_cwd,
        originator: "codex".to_string(),
        cli_version: "0.0.0".to_string(),
        rollout_schema_version: current_rollout_schema_version(),
        source: SessionSource::Cli,
        model_provider: model_provider.map(str::to_string),
        base_instructions: None,
        dynamic_tools: None,
    };
    let payload = serde_json::to_value(SessionMetaLine {
        meta,
        git: git_info,
    })?;

    let mut lines = vec![
        json!({
            "timestamp": meta_rfc3339,
            "type": "session_meta",
            "payload": payload
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"transcript_item",
            "payload": {
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text": preview}]
            }
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"event_msg",
            "payload": {
                "type":"user_message",
                "message": preview,
                "kind": "plain"
            }
        })
        .to_string(),
    ];

    if let Some(cwd) = latest_turn_context_cwd {
        lines.push(serde_json::to_string(&RolloutLine {
            timestamp: meta_rfc3339.to_string(),
            item: RolloutItem::TurnContext(TurnContextItem {
                cwd,
                approval_policy: AskForApproval::OnRequest,
                sandbox_policy: SandboxPolicy::ReadOnly,
                model: "mock-model".to_string(),
                personality: None,
                identity: None,
                effort: None,
                summary: ReasoningSummary::Auto,
                user_instructions: None,
                developer_instructions: None,
                final_output_json_schema: None,
                truncation_policy: None,
            }),
        })?);
    }

    fs::write(&file_path, lines.join("\n") + "\n")?;
    let parsed = chrono::DateTime::parse_from_rfc3339(meta_rfc3339)?.with_timezone(&chrono::Utc);
    let times = FileTimes::new().set_modified(parsed.into());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file_path)?
        .set_times(times)?;
    Ok(uuid_str)
}

/// Create a minimal rollout file with an explicit session source.
pub fn create_fake_rollout_with_source(
    lha_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
    source: SessionSource,
) -> Result<String> {
    let uuid = Uuid::new_v4();
    let uuid_str = uuid.to_string();
    let conversation_id = ThreadId::from_string(&uuid_str)?;

    let file_path = rollout_path(lha_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(dir)?;

    // Build JSONL lines
    let meta = SessionMeta {
        id: conversation_id,
        forked_from_id: None,
        timestamp: meta_rfc3339.to_string(),
        cwd: PathBuf::from("/"),
        originator: "codex".to_string(),
        cli_version: "0.0.0".to_string(),
        rollout_schema_version: current_rollout_schema_version(),
        source,
        model_provider: model_provider.map(str::to_string),
        base_instructions: None,
        dynamic_tools: None,
    };
    let payload = serde_json::to_value(SessionMetaLine {
        meta,
        git: git_info,
    })?;

    let lines = [
        json!({
            "timestamp": meta_rfc3339,
            "type": "session_meta",
            "payload": payload
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"transcript_item",
            "payload": {
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text": preview}]
            }
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"event_msg",
            "payload": {
                "type":"user_message",
                "message": preview,
                "kind": "plain"
            }
        })
        .to_string(),
    ];

    fs::write(&file_path, lines.join("\n") + "\n")?;
    let parsed = chrono::DateTime::parse_from_rfc3339(meta_rfc3339)?.with_timezone(&chrono::Utc);
    let times = FileTimes::new().set_modified(parsed.into());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file_path)?
        .set_times(times)?;
    Ok(uuid_str)
}

pub fn create_fake_rollout_with_schema_version(
    lha_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    model_provider: Option<&str>,
    schema_version: Option<u32>,
) -> Result<String> {
    let uuid = Uuid::new_v4();
    let uuid_str = uuid.to_string();
    let conversation_id = ThreadId::from_string(&uuid_str)?;

    let file_path = rollout_path(lha_home, filename_ts, &uuid_str);
    let dir = file_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing rollout parent directory"))?;
    fs::create_dir_all(dir)?;

    let meta = SessionMeta {
        id: conversation_id,
        forked_from_id: None,
        timestamp: meta_rfc3339.to_string(),
        cwd: PathBuf::from("/"),
        originator: "codex".to_string(),
        cli_version: "0.0.0".to_string(),
        rollout_schema_version: schema_version.unwrap_or_else(current_rollout_schema_version),
        source: SessionSource::Cli,
        model_provider: model_provider.map(str::to_string),
        base_instructions: None,
        dynamic_tools: None,
    };
    let mut payload = serde_json::to_value(SessionMetaLine { meta, git: None })?;
    if schema_version.is_none()
        && let Some(payload) = payload.as_object_mut()
    {
        payload.remove("rollout_schema_version");
    }

    let lines = [
        json!({
            "timestamp": meta_rfc3339,
            "type": "session_meta",
            "payload": payload
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"transcript_item",
            "payload": {
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text": preview}]
            }
        })
        .to_string(),
        json!({
            "timestamp": meta_rfc3339,
            "type":"event_msg",
            "payload": {
                "type":"user_message",
                "message": preview,
                "kind": "plain"
            }
        })
        .to_string(),
    ];

    fs::write(&file_path, lines.join("\n") + "\n")?;
    let parsed = chrono::DateTime::parse_from_rfc3339(meta_rfc3339)?.with_timezone(&chrono::Utc);
    let times = FileTimes::new().set_modified(parsed.into());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file_path)?
        .set_times(times)?;
    Ok(uuid_str)
}

pub fn create_fake_rollout_with_text_elements(
    lha_home: &Path,
    filename_ts: &str,
    meta_rfc3339: &str,
    preview: &str,
    text_elements: Vec<serde_json::Value>,
    model_provider: Option<&str>,
    git_info: Option<GitInfo>,
) -> Result<String> {
    let uuid = Uuid::new_v4();
    let uuid_str = uuid.to_string();
    let conversation_id = ThreadId::from_string(&uuid_str)?;

    // sessions/YYYY/MM/DD derived from filename_ts (YYYY-MM-DDThh-mm-ss)
    let year = &filename_ts[0..4];
    let month = &filename_ts[5..7];
    let day = &filename_ts[8..10];
    let dir = lha_home.join("sessions").join(year).join(month).join(day);
    fs::create_dir_all(&dir)?;

    let file_path = dir.join(format!("rollout-{filename_ts}-{uuid}.jsonl"));

    // Build JSONL lines
    let meta = SessionMeta {
        id: conversation_id,
        forked_from_id: None,
        timestamp: meta_rfc3339.to_string(),
        cwd: PathBuf::from("/"),
        originator: "codex".to_string(),
        cli_version: "0.0.0".to_string(),
        rollout_schema_version: current_rollout_schema_version(),
        source: SessionSource::Cli,
        model_provider: model_provider.map(str::to_string),
        base_instructions: None,
        dynamic_tools: None,
    };
    let payload = serde_json::to_value(SessionMetaLine {
        meta,
        git: git_info,
    })?;

    let lines = [
        json!( {
            "timestamp": meta_rfc3339,
            "type": "session_meta",
            "payload": payload
        })
        .to_string(),
        json!( {
            "timestamp": meta_rfc3339,
            "type":"transcript_item",
            "payload": {
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text": preview}]
            }
        })
        .to_string(),
        json!( {
            "timestamp": meta_rfc3339,
            "type":"event_msg",
            "payload": {
                "type":"user_message",
                "message": preview,
                "text_elements": text_elements,
                "local_images": []
            }
        })
        .to_string(),
    ];

    fs::write(file_path, lines.join("\n") + "\n")?;
    Ok(uuid_str)
}
