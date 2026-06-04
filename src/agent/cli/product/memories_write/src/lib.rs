//! Write-path helpers for LHA memories.
//!
//! Runtime orchestration lives in the LHA product runtime; this module owns
//! deterministic filesystem artifacts, prompt rendering, and memory workspace
//! diffing.

mod prompts;
mod storage;
pub mod workspace;

use crate::product::utils_absolute_path::AbsolutePathBuf;
use std::path::Path;
use std::path::PathBuf;

pub use prompts::STAGE_ONE_SYSTEM_PROMPT;
pub use prompts::StageOneOutput;
pub use prompts::build_consolidation_prompt;
pub use prompts::build_stage_one_input_message;
pub use prompts::stage_one_output_schema;
pub use storage::rebuild_raw_memories_file_from_memories;
pub use storage::rollout_summary_file_stem;
pub use storage::sync_rollout_summaries_from_memories;
pub use workspace::memory_workspace_diff;
pub use workspace::prepare_memory_workspace;
pub use workspace::reset_memory_workspace_baseline;
pub use workspace::write_workspace_diff;

mod artifacts {
    pub(super) const EXTENSIONS_SUBDIR: &str = "extensions";
    pub(super) const ROLLOUT_SUMMARIES_SUBDIR: &str = "rollout_summaries";
    pub(super) const RAW_MEMORIES_FILENAME: &str = "raw_memories.md";
}

mod stage_one {
    pub const MODEL: &str = "gpt-5.4-mini";
    pub const CONCURRENCY_LIMIT: usize = 8;
    pub const JOB_LEASE_SECONDS: i64 = 3_600;
    pub const JOB_RETRY_DELAY_SECONDS: i64 = 3_600;
    pub const THREAD_SCAN_LIMIT: usize = 5_000;
    pub const PRUNE_BATCH_SIZE: usize = 200;
    pub const DEFAULT_ROLLOUT_TOKEN_LIMIT: usize = 150_000;
    pub const CONTEXT_WINDOW_PERCENT: i64 = 70;
}

mod stage_two {
    pub const MODEL: &str = "gpt-5.4";
    pub const JOB_LEASE_SECONDS: i64 = 3_600;
    pub const JOB_RETRY_DELAY_SECONDS: i64 = 3_600;
    pub const JOB_HEARTBEAT_SECONDS: u64 = 90;
}

mod workspace_diff {
    pub(super) const FILENAME: &str = "phase2_workspace_diff.md";
    pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
}

pub fn memory_root(lha_home: &AbsolutePathBuf) -> std::io::Result<AbsolutePathBuf> {
    lha_home.join("memories")
}

pub fn memory_root_path(lha_home: &Path) -> PathBuf {
    lha_home.join("memories")
}

pub fn rollout_summaries_dir(root: &Path) -> PathBuf {
    root.join(artifacts::ROLLOUT_SUMMARIES_SUBDIR)
}

pub fn memory_extensions_root(root: &Path) -> PathBuf {
    root.join(artifacts::EXTENSIONS_SUBDIR)
}

pub fn raw_memories_file(root: &Path) -> PathBuf {
    root.join(artifacts::RAW_MEMORIES_FILENAME)
}

pub async fn ensure_layout(root: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(rollout_summaries_dir(root)).await?;
    tokio::fs::create_dir_all(memory_extensions_root(root).join("ad_hoc/notes")).await?;
    seed_ad_hoc_instructions(root).await
}

pub async fn seed_ad_hoc_instructions(root: &Path) -> std::io::Result<()> {
    let path = memory_extensions_root(root).join("ad_hoc/instructions.md");
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(
        path,
        include_str!("../templates/extensions/ad_hoc/instructions.md"),
    )
    .await
}

pub use stage_one::CONCURRENCY_LIMIT as STAGE_ONE_CONCURRENCY_LIMIT;
pub use stage_one::CONTEXT_WINDOW_PERCENT as STAGE_ONE_CONTEXT_WINDOW_PERCENT;
pub use stage_one::DEFAULT_ROLLOUT_TOKEN_LIMIT as STAGE_ONE_DEFAULT_ROLLOUT_TOKEN_LIMIT;
pub use stage_one::JOB_LEASE_SECONDS as STAGE_ONE_JOB_LEASE_SECONDS;
pub use stage_one::JOB_RETRY_DELAY_SECONDS as STAGE_ONE_JOB_RETRY_DELAY_SECONDS;
pub use stage_one::MODEL as STAGE_ONE_MODEL;
pub use stage_one::PRUNE_BATCH_SIZE as STAGE_ONE_PRUNE_BATCH_SIZE;
pub use stage_one::THREAD_SCAN_LIMIT as STAGE_ONE_THREAD_SCAN_LIMIT;
pub use stage_two::JOB_HEARTBEAT_SECONDS as STAGE_TWO_JOB_HEARTBEAT_SECONDS;
pub use stage_two::JOB_LEASE_SECONDS as STAGE_TWO_JOB_LEASE_SECONDS;
pub use stage_two::JOB_RETRY_DELAY_SECONDS as STAGE_TWO_JOB_RETRY_DELAY_SECONDS;
pub use stage_two::MODEL as STAGE_TWO_MODEL;
