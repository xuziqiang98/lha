//! Read-path helpers for LHA memories.
//!
//! This crate owns memory prompt injection and memory citation parsing. It does
//! not depend on the memory write pipeline.

pub mod citations;
pub mod usage;

use crate::product::utils_absolute_path::AbsolutePathBuf;
use std::path::Path;
use std::path::PathBuf;

const READ_PATH_TEMPLATE: &str = include_str!("../templates/memories/read_path.md");

pub fn memory_root(lha_home: &AbsolutePathBuf) -> std::io::Result<AbsolutePathBuf> {
    lha_home.join("memories")
}

pub fn memory_root_path(lha_home: &Path) -> PathBuf {
    lha_home.join("memories")
}

pub async fn build_memory_developer_instructions(
    lha_home: &Path,
) -> std::io::Result<Option<String>> {
    let root = memory_root_path(lha_home);
    let summary_path = root.join("memory_summary.md");
    let summary = match tokio::fs::read_to_string(&summary_path).await {
        Ok(summary) => summary,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let summary = summary.trim();
    if summary.is_empty() {
        return Ok(None);
    }
    let rendered = READ_PATH_TEMPLATE
        .replace("{{ base_path }}", &root.display().to_string())
        .replace("{{ memory_summary }}", summary);
    Ok(Some(rendered))
}
