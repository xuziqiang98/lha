use crate::product::state::Stage1Output;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use tracing::warn;
use uuid::Uuid;

use crate::product::memories_write::ensure_layout;
use crate::product::memories_write::raw_memories_file;
use crate::product::memories_write::rollout_summaries_dir;

/// Rebuild `raw_memories.md` from DB-backed stage-1 outputs.
pub async fn rebuild_raw_memories_file_from_memories(
    root: &Path,
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> std::io::Result<()> {
    ensure_layout(root).await?;
    rebuild_raw_memories_file(root, memories, max_raw_memories_for_consolidation).await
}

/// Syncs canonical rollout summary files from DB-backed stage-1 output rows.
pub async fn sync_rollout_summaries_from_memories(
    root: &Path,
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> std::io::Result<()> {
    ensure_layout(root).await?;

    let retained = retained_memories(memories, max_raw_memories_for_consolidation);
    let keep = retained
        .iter()
        .map(rollout_summary_file_stem)
        .collect::<HashSet<_>>();
    prune_rollout_summaries(root, &keep).await?;

    for memory in retained {
        write_rollout_summary_for_thread(root, memory).await?;
    }

    Ok(())
}

async fn rebuild_raw_memories_file(
    root: &Path,
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> std::io::Result<()> {
    let retained = retained_memories(memories, max_raw_memories_for_consolidation);
    let mut body = String::from("# Raw Memories\n\n");

    if retained.is_empty() {
        body.push_str("No raw memories yet.\n");
        return tokio::fs::write(raw_memories_file(root), body).await;
    }

    body.push_str("Merged stage-1 raw memories (stable ascending thread-id order):\n\n");
    for memory in retained {
        writeln!(body, "## Thread `{}`", memory.thread_id).map_err(format_error)?;
        writeln!(
            body,
            "updated_at: {}",
            memory.source_updated_at.to_rfc3339()
        )
        .map_err(format_error)?;
        writeln!(body, "cwd: {}", memory.cwd.display()).map_err(format_error)?;
        writeln!(body, "rollout_path: {}", memory.rollout_path.display()).map_err(format_error)?;
        let rollout_summary_file = format!("{}.md", rollout_summary_file_stem(memory));
        writeln!(body, "rollout_summary_file: {rollout_summary_file}").map_err(format_error)?;
        writeln!(body).map_err(format_error)?;
        body.push_str(memory.raw_memory.trim());
        body.push_str("\n\n");
    }

    tokio::fs::write(raw_memories_file(root), body).await
}

async fn prune_rollout_summaries(root: &Path, keep: &HashSet<String>) -> std::io::Result<()> {
    let dir_path = rollout_summaries_dir(root);
    let mut dir = match tokio::fs::read_dir(&dir_path).await {
        Ok(dir) => dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".md") else {
            continue;
        };
        if !keep.contains(stem)
            && let Err(err) = tokio::fs::remove_file(&path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!(
                "failed pruning outdated rollout summary {}: {err}",
                path.display()
            );
        }
    }

    Ok(())
}

async fn write_rollout_summary_for_thread(
    root: &Path,
    memory: &Stage1Output,
) -> std::io::Result<()> {
    let file_stem = rollout_summary_file_stem(memory);
    let path = rollout_summaries_dir(root).join(format!("{file_stem}.md"));

    let mut body = String::new();
    writeln!(body, "thread_id: {}", memory.thread_id).map_err(format_error)?;
    writeln!(
        body,
        "updated_at: {}",
        memory.source_updated_at.to_rfc3339()
    )
    .map_err(format_error)?;
    writeln!(body, "rollout_path: {}", memory.rollout_path.display()).map_err(format_error)?;
    writeln!(body, "cwd: {}", memory.cwd.display()).map_err(format_error)?;
    if let Some(git_branch) = memory.git_branch.as_deref() {
        writeln!(body, "git_branch: {git_branch}").map_err(format_error)?;
    }
    writeln!(body).map_err(format_error)?;
    body.push_str(&memory.rollout_summary);
    body.push('\n');

    tokio::fs::write(path, body).await
}

fn retained_memories(
    memories: &[Stage1Output],
    max_raw_memories_for_consolidation: usize,
) -> &[Stage1Output] {
    &memories[..memories.len().min(max_raw_memories_for_consolidation)]
}

fn format_error(err: std::fmt::Error) -> std::io::Error {
    std::io::Error::other(format!("format memory artifact: {err}"))
}

pub fn rollout_summary_file_stem(memory: &Stage1Output) -> String {
    rollout_summary_file_stem_from_parts(
        memory.thread_id,
        memory.source_updated_at,
        memory.rollout_slug.as_deref(),
    )
}

fn rollout_summary_file_stem_from_parts(
    thread_id: crate::product::protocol::ThreadId,
    source_updated_at: chrono::DateTime<chrono::Utc>,
    rollout_slug: Option<&str>,
) -> String {
    const ROLLOUT_SLUG_MAX_LEN: usize = 60;
    const SHORT_HASH_ALPHABET: &[u8; 62] =
        b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const SHORT_HASH_SPACE: u32 = 14_776_336;

    let thread_id = thread_id.to_string();
    let (timestamp_fragment, short_hash_seed) = match Uuid::parse_str(&thread_id) {
        Ok(thread_uuid) => {
            let timestamp = thread_uuid
                .get_timestamp()
                .and_then(|uuid_timestamp| {
                    let (seconds, nanos) = uuid_timestamp.to_unix();
                    i64::try_from(seconds).ok().and_then(|secs| {
                        chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
                    })
                })
                .unwrap_or(source_updated_at);
            let short_hash_seed = (thread_uuid.as_u128() & 0xFFFF_FFFF) as u32;
            (
                timestamp.format("%Y-%m-%dT%H-%M-%S").to_string(),
                short_hash_seed,
            )
        }
        Err(_) => {
            let mut short_hash_seed = 0u32;
            for byte in thread_id.bytes() {
                short_hash_seed = short_hash_seed
                    .wrapping_mul(31)
                    .wrapping_add(u32::from(byte));
            }
            (
                source_updated_at.format("%Y-%m-%dT%H-%M-%S").to_string(),
                short_hash_seed,
            )
        }
    };
    let mut short_hash_value = short_hash_seed % SHORT_HASH_SPACE;
    let mut short_hash_chars = ['0'; 4];
    for idx in (0..short_hash_chars.len()).rev() {
        let alphabet_idx = (short_hash_value % SHORT_HASH_ALPHABET.len() as u32) as usize;
        short_hash_chars[idx] = SHORT_HASH_ALPHABET[alphabet_idx] as char;
        short_hash_value /= SHORT_HASH_ALPHABET.len() as u32;
    }
    let short_hash: String = short_hash_chars.iter().collect();
    let file_prefix = format!("{timestamp_fragment}-{short_hash}");

    let Some(raw_slug) = rollout_slug else {
        return file_prefix;
    };

    let mut slug = String::with_capacity(ROLLOUT_SLUG_MAX_LEN);
    for ch in raw_slug.chars() {
        if slug.len() >= ROLLOUT_SLUG_MAX_LEN {
            break;
        }

        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else {
            slug.push('_');
        }
    }

    while slug.ends_with('_') {
        slug.pop();
    }

    if slug.is_empty() {
        file_prefix
    } else {
        format!("{file_prefix}-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::protocol::ThreadId;
    use chrono::TimeZone;
    use tempfile::TempDir;

    #[tokio::test]
    async fn rebuilds_raw_memories_deterministically() {
        let tmp = TempDir::new().expect("tempdir");
        let first = stage1_output(1, "first raw", "first summary", Some("First Task"));
        let second = stage1_output(2, "second raw", "second summary", None);

        rebuild_raw_memories_file(tmp.path(), &[first.clone(), second.clone()], 10)
            .await
            .expect("rebuild");
        let raw = tokio::fs::read_to_string(raw_memories_file(tmp.path()))
            .await
            .expect("read raw");

        assert!(raw.contains("# Raw Memories"));
        assert!(raw.contains("first raw"));
        assert!(raw.contains("second raw"));
        let first_pos = raw.find("first raw").expect("first raw");
        let second_pos = raw.find("second raw").expect("second raw");
        assert!(first_pos < second_pos);
    }

    #[tokio::test]
    async fn sync_rollout_summaries_prunes_unselected_files() {
        let tmp = TempDir::new().expect("tempdir");
        let stale = rollout_summaries_dir(tmp.path()).join("stale.md");
        tokio::fs::create_dir_all(rollout_summaries_dir(tmp.path()))
            .await
            .expect("mkdir");
        tokio::fs::write(&stale, "stale")
            .await
            .expect("write stale");
        let output = stage1_output(1, "raw", "summary", Some("Useful Task"));

        sync_rollout_summaries_from_memories(tmp.path(), std::slice::from_ref(&output), 10)
            .await
            .expect("sync");

        assert!(!tokio::fs::try_exists(&stale).await.expect("exists"));
        let summary = tokio::fs::read_to_string(
            rollout_summaries_dir(tmp.path())
                .join(format!("{}.md", rollout_summary_file_stem(&output))),
        )
        .await
        .expect("read summary");
        assert!(summary.contains("summary"));
        assert!(summary.contains("thread_id:"));
    }

    fn stage1_output(n: u128, raw_memory: &str, summary: &str, slug: Option<&str>) -> Stage1Output {
        let thread_id =
            ThreadId::from_string(&uuid::Uuid::from_u128(n).to_string()).expect("thread id");
        let source_updated_at = chrono::Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, n as u32)
            .single()
            .expect("timestamp");
        Stage1Output {
            thread_id,
            rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
            source_updated_at,
            raw_memory: raw_memory.to_string(),
            rollout_summary: summary.to_string(),
            rollout_slug: slug.map(str::to_string),
            cwd: Path::new("/tmp/project").to_path_buf(),
            git_branch: Some("main".to_string()),
            generated_at: source_updated_at,
        }
    }
}
