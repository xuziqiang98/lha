use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub const MEMORIES_LIST_TOOL: &str = "memories__list";
pub const MEMORIES_READ_TOOL: &str = "memories__read";
pub const MEMORIES_SEARCH_TOOL: &str = "memories__search";
pub const MEMORIES_ADD_AD_HOC_NOTE_TOOL: &str = "memories__add_ad_hoc_note";

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1_000;
const READ_DEFAULT_LIMIT: usize = 200;
const READ_MAX_BYTES: usize = 256 * 1024;
const SEARCH_CONTEXT_LINES: usize = 2;

pub struct MemoriesHandler;

#[derive(Deserialize)]
struct ListArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    cursor: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    cursor: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct AddAdHocNoteArgs {
    content: String,
    #[serde(default)]
    slug: Option<String>,
}

#[async_trait]
impl ToolHandler for MemoriesHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, invocation: &ToolInvocation) -> bool {
        invocation.tool_name == MEMORIES_ADD_AD_HOC_NOTE_TOOL
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            tool_name,
            payload,
            ..
        } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "memory handler received unsupported payload".to_string(),
                ));
            }
        };
        let lha_home = session.lha_home().await;
        let root = lha_home.join("memories");
        let output = match tool_name.as_str() {
            MEMORIES_LIST_TOOL => list_memories(&root, parse_arguments(&arguments)?).await,
            MEMORIES_READ_TOOL => read_memory(&root, parse_arguments(&arguments)?).await,
            MEMORIES_SEARCH_TOOL => search_memories(&root, parse_arguments(&arguments)?).await,
            MEMORIES_ADD_AD_HOC_NOTE_TOOL => {
                add_ad_hoc_note(&root, parse_arguments(&arguments)?).await
            }
            _ => Err(FunctionCallError::RespondToModel(format!(
                "unsupported memory tool: {tool_name}"
            ))),
        }?;
        Ok(ToolOutput::Function {
            content: output.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}

async fn list_memories(
    root: &Path,
    args: ListArgs,
) -> Result<serde_json::Value, FunctionCallError> {
    let dir = resolve_memory_path(root, args.path.as_deref().unwrap_or(""), true).await?;
    let root_canon = tokio::fs::canonicalize(root).await.map_err(tool_error)?;
    let mut entries = tokio::fs::read_dir(&dir).await.map_err(tool_error)?;
    let mut items = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(tool_error)? {
        let file_type = entry.file_type().await.map_err(tool_error)?;
        let path = entry.path();
        let relative = path.strip_prefix(&root_canon).unwrap_or(path.as_path());
        let name = relative.to_string_lossy().replace('\\', "/");
        if name == ".git" || name.starts_with(".git/") {
            continue;
        }
        items.push(json!({
            "path": name,
            "kind": if file_type.is_dir() { "directory" } else if file_type.is_file() { "file" } else { "other" }
        }));
    }
    items.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(page_json(
        items,
        args.cursor.unwrap_or(0),
        args.limit.unwrap_or(DEFAULT_LIMIT),
    ))
}

async fn read_memory(root: &Path, args: ReadArgs) -> Result<serde_json::Value, FunctionCallError> {
    let path = resolve_memory_path(root, &args.path, false).await?;
    let metadata = tokio::fs::metadata(&path).await.map_err(tool_error)?;
    if !metadata.is_file() {
        return Err(FunctionCallError::RespondToModel(
            "memory path is not a file".to_string(),
        ));
    }
    let content = tokio::fs::read_to_string(&path).await.map_err(tool_error)?;
    let mut truncated = false;
    let lines: Vec<&str> = content.lines().collect();
    let offset = args.offset.unwrap_or(1).max(1);
    let limit = args.limit.unwrap_or(READ_DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let start = offset.saturating_sub(1);
    let end = lines.len().min(start.saturating_add(limit));
    let mut selected = if start < lines.len() {
        lines[start..end].join("\n")
    } else {
        String::new()
    };
    truncated |= truncate_at_char_boundary(&mut selected, READ_MAX_BYTES);
    truncated |= end < lines.len();
    Ok(json!({
        "path": args.path,
        "start_line": offset,
        "end_line": end,
        "content": selected,
        "truncated": truncated
    }))
}

fn truncate_at_char_boundary(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        return false;
    }

    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

async fn search_memories(
    root: &Path,
    args: SearchArgs,
) -> Result<serde_json::Value, FunctionCallError> {
    if args.query.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "query must not be empty".to_string(),
        ));
    }
    let base = resolve_memory_path(root, args.path.as_deref().unwrap_or(""), true).await?;
    let mut files = Vec::new();
    let root_canon = tokio::fs::canonicalize(root).await.map_err(tool_error)?;
    let metadata = tokio::fs::metadata(&base).await.map_err(tool_error)?;
    if metadata.is_file() {
        files.push(base);
    } else if metadata.is_dir() {
        collect_files(&root_canon, &base, &mut files).await?;
    } else {
        return Err(FunctionCallError::RespondToModel(
            "memory search path is neither a file nor a directory".to_string(),
        ));
    }
    files.sort();

    let query = args.query.to_lowercase();
    let mut matches = Vec::new();
    for file in files {
        let Ok(content) = tokio::fs::read_to_string(&file).await else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.to_lowercase().contains(&query) {
                continue;
            }
            let context_start = idx.saturating_sub(SEARCH_CONTEXT_LINES);
            let context_end = lines.len().min(idx + SEARCH_CONTEXT_LINES + 1);
            let relative = file.strip_prefix(&root_canon).unwrap_or(file.as_path());
            matches.push(json!({
                "path": relative.to_string_lossy().replace('\\', "/"),
                "line": idx + 1,
                "text": line,
                "context": lines[context_start..context_end].join("\n")
            }));
        }
    }
    Ok(page_json(
        matches,
        args.cursor.unwrap_or(0),
        args.limit.unwrap_or(DEFAULT_LIMIT),
    ))
}

async fn add_ad_hoc_note(
    root: &Path,
    args: AddAdHocNoteArgs,
) -> Result<serde_json::Value, FunctionCallError> {
    add_ad_hoc_note_at(root, args, Utc::now()).await
}

async fn add_ad_hoc_note_at(
    root: &Path,
    args: AddAdHocNoteArgs,
    now: DateTime<Utc>,
) -> Result<serde_json::Value, FunctionCallError> {
    if args.content.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "content must not be empty".to_string(),
        ));
    }
    tokio::fs::create_dir_all(root).await.map_err(tool_error)?;
    let root_canon = tokio::fs::canonicalize(root).await.map_err(tool_error)?;
    let notes_dir = root_canon.join("extensions/ad_hoc/notes");
    tokio::fs::create_dir_all(&notes_dir)
        .await
        .map_err(tool_error)?;
    let notes_dir = tokio::fs::canonicalize(&notes_dir)
        .await
        .map_err(tool_error)?;
    if !notes_dir.starts_with(&root_canon) {
        return Err(FunctionCallError::RespondToModel(
            "ad-hoc memory notes path escapes the memory root".to_string(),
        ));
    }
    let slug = validate_slug(args.slug.as_deref().unwrap_or("memory-note"))?;
    let timestamp = now.format("%Y-%m-%dT%H-%M-%S");
    let file_name = format!("{timestamp}-{slug}.md");
    let path = notes_dir.join(file_name);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                FunctionCallError::RespondToModel("ad-hoc memory note already exists".to_string())
            } else {
                tool_error(err)
            }
        })?;
    file.write_all(args.content.as_bytes())
        .await
        .map_err(tool_error)?;
    let relative = path.strip_prefix(&root_canon).unwrap_or(path.as_path());
    Ok(json!({
        "created_path": relative.to_string_lossy().replace('\\', "/")
    }))
}

async fn resolve_memory_path(
    root: &Path,
    relative: &str,
    allow_dir: bool,
) -> Result<PathBuf, FunctionCallError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || relative.split('/').any(|part| part == "..")
        || relative.split('\\').any(|part| part == "..")
    {
        return Err(FunctionCallError::RespondToModel(
            "memory paths must be relative and stay under the memory root".to_string(),
        ));
    }
    tokio::fs::create_dir_all(root).await.map_err(tool_error)?;
    let root_canon = tokio::fs::canonicalize(root).await.map_err(tool_error)?;
    let path = root_canon.join(relative_path);
    let canon = tokio::fs::canonicalize(&path).await.map_err(tool_error)?;
    if !canon.starts_with(&root_canon) {
        return Err(FunctionCallError::RespondToModel(
            "memory path escapes the memory root".to_string(),
        ));
    }
    let metadata = tokio::fs::metadata(&canon).await.map_err(tool_error)?;
    if metadata.is_dir() && !allow_dir {
        return Err(FunctionCallError::RespondToModel(
            "memory path is a directory".to_string(),
        ));
    }
    Ok(canon)
}

async fn collect_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), FunctionCallError> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(tool_error)?;
        while let Some(entry) = entries.next_entry().await.map_err(tool_error)? {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(path.as_path());
            if relative.starts_with(".git") {
                continue;
            }
            let file_type = entry.file_type().await.map_err(tool_error)?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn page_json(mut items: Vec<serde_json::Value>, cursor: usize, limit: usize) -> serde_json::Value {
    let limit = limit.clamp(1, MAX_LIMIT);
    let start = cursor.min(items.len());
    let end = items.len().min(start.saturating_add(limit));
    let truncated = end < items.len();
    let next_cursor = truncated.then_some(end);
    let entries = items.drain(start..end).collect::<Vec<_>>();
    json!({
        "entries": entries,
        "cursor": next_cursor,
        "truncated": truncated
    })
}

fn validate_slug(input: &str) -> Result<String, FunctionCallError> {
    if input.len() > 60
        || input.is_empty()
        || input.starts_with('-')
        || input.ends_with('-')
        || input.contains("--")
        || !input
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(FunctionCallError::RespondToModel(
            "slug must match ^[a-z0-9]+(?:-[a-z0-9]+)*$ and be at most 60 bytes".to_string(),
        ));
    }
    Ok(input.to_string())
}

fn tool_error(err: std::io::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn list_read_and_search_memories() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");
        tokio::fs::create_dir_all(root.join("nested"))
            .await
            .expect("mkdir");
        tokio::fs::write(root.join("nested/note.md"), "alpha\nbeta\n")
            .await
            .expect("write");

        let listed = list_memories(
            root.as_path(),
            ListArgs {
                path: Some("nested".to_string()),
                cursor: None,
                limit: None,
            },
        )
        .await
        .expect("list");
        assert_eq!(listed["entries"][0]["path"], "nested/note.md");

        let read = read_memory(
            root.as_path(),
            ReadArgs {
                path: "nested/note.md".to_string(),
                offset: Some(2),
                limit: Some(1),
            },
        )
        .await
        .expect("read");
        assert_eq!(read["content"], "beta");

        let searched = search_memories(
            root.as_path(),
            SearchArgs {
                query: "alpha".to_string(),
                path: Some("nested/note.md".to_string()),
                cursor: None,
                limit: None,
            },
        )
        .await
        .expect("search");
        assert_eq!(searched["entries"][0]["path"], "nested/note.md");
        assert_eq!(searched["entries"][0]["line"], 1);
    }

    #[tokio::test]
    async fn read_memory_truncates_at_utf8_boundary() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");
        tokio::fs::create_dir_all(&root).await.expect("mkdir");
        let content = format!("{}é", "a".repeat(READ_MAX_BYTES - 1));
        tokio::fs::write(root.join("large.md"), content)
            .await
            .expect("write");

        let read = read_memory(
            root.as_path(),
            ReadArgs {
                path: "large.md".to_string(),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read");
        let content = read["content"].as_str().expect("content");

        assert_eq!(content.len(), READ_MAX_BYTES - 1);
        assert_eq!(content, "a".repeat(READ_MAX_BYTES - 1));
        assert_eq!(read["truncated"], true);
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");

        assert!(
            read_memory(
                root.as_path(),
                ReadArgs {
                    path: "../secret".to_string(),
                    offset: None,
                    limit: None,
                },
            )
            .await
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");
        let outside = tmp.path().join("outside");
        tokio::fs::create_dir_all(&root).await.expect("mkdir root");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("mkdir outside");
        tokio::fs::write(outside.join("secret.md"), "secret")
            .await
            .expect("write");
        std::os::unix::fs::symlink(&outside, root.join("escape")).expect("symlink");

        assert!(
            read_memory(
                root.as_path(),
                ReadArgs {
                    path: "escape/secret.md".to_string(),
                    offset: None,
                    limit: None,
                },
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn add_ad_hoc_note_validates_slug_and_rejects_duplicates() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");
        let now = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("timestamp");

        assert!(
            add_ad_hoc_note_at(
                root.as_path(),
                AddAdHocNoteArgs {
                    content: "remember".to_string(),
                    slug: Some("Bad Slug".to_string()),
                },
                now,
            )
            .await
            .is_err()
        );

        let created = add_ad_hoc_note_at(
            root.as_path(),
            AddAdHocNoteArgs {
                content: "remember".to_string(),
                slug: Some("valid-slug".to_string()),
            },
            now,
        )
        .await
        .expect("created");
        assert_eq!(
            created["created_path"],
            "extensions/ad_hoc/notes/2026-01-02T03-04-05-valid-slug.md"
        );

        assert!(
            add_ad_hoc_note_at(
                root.as_path(),
                AddAdHocNoteArgs {
                    content: "remember again".to_string(),
                    slug: Some("valid-slug".to_string()),
                },
                now,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn add_ad_hoc_note_uses_default_slug() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");
        let now = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("timestamp");

        let created = add_ad_hoc_note_at(
            root.as_path(),
            AddAdHocNoteArgs {
                content: "remember".to_string(),
                slug: None,
            },
            now,
        )
        .await
        .expect("created");

        assert_eq!(
            created["created_path"],
            "extensions/ad_hoc/notes/2026-01-02T03-04-05-memory-note.md"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn add_ad_hoc_note_rejects_symlinked_notes_escape() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("memories");
        let outside = tmp.path().join("outside");
        tokio::fs::create_dir_all(root.join("extensions"))
            .await
            .expect("mkdir extensions");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("mkdir outside");
        std::os::unix::fs::symlink(&outside, root.join("extensions/ad_hoc")).expect("symlink");

        assert!(
            add_ad_hoc_note_at(
                root.as_path(),
                AddAdHocNoteArgs {
                    content: "remember".to_string(),
                    slug: None,
                },
                Utc::now(),
            )
            .await
            .is_err()
        );
    }
}
