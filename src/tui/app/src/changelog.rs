use dirs::home_dir;
use pathdiff::diff_paths;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tokio::task;
use tracing::warn;
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ChangelogKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangelogEntry {
    pub(crate) kind: ChangelogKind,
    pub(crate) path: PathBuf,
    pub(crate) line_stats: Option<LineStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineStats {
    pub(crate) added: usize,
    pub(crate) removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChangelogOutput {
    Entries {
        display_root: PathBuf,
        entries: Vec<ChangelogEntry>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectorySnapshot {
    files: HashMap<PathBuf, FileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileFingerprint {
    Regular { len: u64, sha256: [u8; 32] },
    Symlink { target: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitStatusEntry {
    kind: ChangelogKind,
    path: PathBuf,
    is_untracked: bool,
}

pub(crate) async fn get_git_changelog(cwd: &Path) -> io::Result<Option<ChangelogOutput>> {
    let Some(display_root) = git_repo_root(cwd).await? else {
        return Ok(None);
    };

    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all", "-z"])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git status failed with status {}",
            output.status
        )));
    }

    let status_entries = parse_porcelain_v1_z(&output.stdout, &display_root);
    let line_stats = collect_git_line_stats(cwd, &display_root, &status_entries).await?;
    let mut entries: Vec<ChangelogEntry> = status_entries
        .into_iter()
        .map(|entry| ChangelogEntry {
            kind: entry.kind,
            line_stats: line_stats.get(&entry.path).copied(),
            path: entry.path,
        })
        .collect();
    sort_entries(cwd, &display_root, &mut entries);

    Ok(Some(ChangelogOutput::Entries {
        display_root,
        entries,
    }))
}

pub(crate) async fn git_repo_root(cwd: &Path) -> io::Result<Option<PathBuf>> {
    let output = match Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    Ok(Some(PathBuf::from(trimmed)))
}

pub(crate) async fn capture_directory_snapshot(root: PathBuf) -> io::Result<DirectorySnapshot> {
    task::spawn_blocking(move || capture_directory_snapshot_blocking(&root))
        .await
        .map_err(|err| io::Error::other(format!("snapshot task failed: {err}")))?
}

pub(crate) async fn get_non_git_changelog(
    cwd: &Path,
    baseline: &DirectorySnapshot,
) -> io::Result<ChangelogOutput> {
    let display_root = cwd.to_path_buf();
    let current = capture_directory_snapshot(display_root.clone()).await?;
    let mut entries = diff_snapshots(baseline, &current);
    sort_entries(cwd, &display_root, &mut entries);

    Ok(ChangelogOutput::Entries {
        display_root,
        entries,
    })
}

fn capture_directory_snapshot_blocking(root: &Path) -> io::Result<DirectorySnapshot> {
    let metadata = fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "changelog root is not a directory: {}",
            root.display()
        )));
    }

    let mut files = HashMap::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn!("skipping unreadable path while building changelog snapshot: {err}");
                continue;
            }
        };

        if let Some((path, fingerprint)) = snapshot_entry(root, &entry) {
            files.insert(path, fingerprint);
        }
    }

    Ok(DirectorySnapshot { files })
}

fn snapshot_entry(root: &Path, entry: &walkdir::DirEntry) -> Option<(PathBuf, FileFingerprint)> {
    let path = entry.path();
    if path == root {
        return None;
    }

    let file_type = entry.file_type();
    if file_type.is_dir() {
        return None;
    }

    if file_type.is_symlink() {
        return match fs::read_link(path) {
            Ok(target) => Some((path.to_path_buf(), FileFingerprint::Symlink { target })),
            Err(err) => {
                warn!("skipping unreadable symlink {}: {err}", path.display());
                None
            }
        };
    }

    if file_type.is_file() {
        return match fingerprint_regular_file(path) {
            Ok(fingerprint) => Some((path.to_path_buf(), fingerprint)),
            Err(err) => {
                warn!("skipping unreadable file {}: {err}", path.display());
                None
            }
        };
    }

    None
}

fn fingerprint_regular_file(path: &Path) -> io::Result<FileFingerprint> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(FileFingerprint::Regular {
        len,
        sha256: hasher.finalize().into(),
    })
}

fn diff_snapshots(
    baseline: &DirectorySnapshot,
    current: &DirectorySnapshot,
) -> Vec<ChangelogEntry> {
    let mut entries = Vec::new();

    for (path, current_fingerprint) in &current.files {
        match baseline.files.get(path) {
            None => entries.push(ChangelogEntry {
                kind: ChangelogKind::Added,
                path: path.clone(),
                line_stats: None,
            }),
            Some(baseline_fingerprint) if baseline_fingerprint != current_fingerprint => {
                entries.push(ChangelogEntry {
                    kind: ChangelogKind::Modified,
                    path: path.clone(),
                    line_stats: None,
                });
            }
            Some(_) => {}
        }
    }

    for path in baseline.files.keys() {
        if !current.files.contains_key(path) {
            entries.push(ChangelogEntry {
                kind: ChangelogKind::Deleted,
                path: path.clone(),
                line_stats: None,
            });
        }
    }

    entries
}

fn sort_entries(cwd: &Path, display_root: &Path, entries: &mut [ChangelogEntry]) {
    entries.sort_by(|left, right| {
        left.kind.cmp(&right.kind).then_with(|| {
            format_changelog_path(&left.path, cwd, display_root).cmp(&format_changelog_path(
                &right.path,
                cwd,
                display_root,
            ))
        })
    });
}

async fn collect_git_line_stats(
    cwd: &Path,
    display_root: &Path,
    entries: &[GitStatusEntry],
) -> io::Result<HashMap<PathBuf, LineStats>> {
    let mut stats = HashMap::new();
    let cached_stats = collect_tracked_git_line_stats(
        cwd,
        display_root,
        ["diff", "--numstat", "-z", "--cached", "--"],
    )
    .await?;
    merge_line_stats(&mut stats, cached_stats);

    let unstaged_stats =
        collect_tracked_git_line_stats(cwd, display_root, ["diff", "--numstat", "-z", "--"])
            .await?;
    merge_line_stats(&mut stats, unstaged_stats);

    for entry in entries.iter().filter(|entry| entry.is_untracked) {
        if stats.contains_key(&entry.path) {
            continue;
        }

        match collect_untracked_git_line_stats(display_root, &entry.path).await {
            Ok(Some(line_stats)) => {
                stats.insert(entry.path.clone(), line_stats);
            }
            Ok(None) => {}
            Err(err) => {
                warn!(
                    "skipping line stats for untracked path {}: {err}",
                    entry.path.display()
                );
            }
        }
    }

    Ok(stats)
}

async fn collect_tracked_git_line_stats<const N: usize>(
    cwd: &Path,
    display_root: &Path,
    args: [&str; N],
) -> io::Result<HashMap<PathBuf, LineStats>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git diff --numstat failed with status {}",
            output.status
        )));
    }

    Ok(parse_numstat_z(&output.stdout, display_root))
}

async fn collect_untracked_git_line_stats(
    display_root: &Path,
    path: &Path,
) -> io::Result<Option<LineStats>> {
    let Ok(relative_path) = path.strip_prefix(display_root) else {
        return Ok(None);
    };

    let output = Command::new("git")
        .args(["diff", "--numstat", "-z", "--no-index", "--", "/dev/null"])
        .arg(relative_path)
        .current_dir(display_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    let status_code = output.status.code();
    if !matches!(status_code, Some(0 | 1)) {
        return Err(io::Error::other(format!(
            "git diff --no-index failed with status {}",
            output.status
        )));
    }

    let mut stats = parse_numstat_z(&output.stdout, display_root);
    Ok(stats.remove(path).or_else(|| stats.into_values().next()))
}

fn merge_line_stats(
    stats: &mut HashMap<PathBuf, LineStats>,
    incoming: HashMap<PathBuf, LineStats>,
) {
    for (path, line_stats) in incoming {
        stats
            .entry(path)
            .and_modify(|existing| {
                existing.added += line_stats.added;
                existing.removed += line_stats.removed;
            })
            .or_insert(line_stats);
    }
}

fn parse_numstat_z(output: &[u8], display_root: &Path) -> HashMap<PathBuf, LineStats> {
    let mut stats = HashMap::new();
    let mut records = output.split(|byte| *byte == 0);

    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }

        let fields: Vec<&[u8]> = record.splitn(3, |byte| *byte == b'\t').collect();
        if fields.len() != 3 {
            continue;
        }

        let line_stats = parse_numstat_counts(fields[0], fields[1]);
        let path = if fields[2].is_empty() {
            let _old_path = records.next();
            records.next()
        } else {
            Some(fields[2])
        };

        let Some(path) = path else {
            continue;
        };
        let Some(line_stats) = line_stats else {
            continue;
        };

        stats.insert(display_root.join(decode_path(path)), line_stats);
    }

    stats
}

fn parse_numstat_counts(added: &[u8], removed: &[u8]) -> Option<LineStats> {
    let added = std::str::from_utf8(added).ok()?;
    let removed = std::str::from_utf8(removed).ok()?;
    if added == "-" || removed == "-" {
        return None;
    }

    Some(LineStats {
        added: added.parse().ok()?,
        removed: removed.parse().ok()?,
    })
}

fn parse_porcelain_v1_z(output: &[u8], display_root: &Path) -> Vec<GitStatusEntry> {
    let mut entries = Vec::new();
    let mut records = output.split(|byte| *byte == 0);

    while let Some(record) = records.next() {
        if record.is_empty() || record.len() < 3 {
            continue;
        }

        let status = &record[..3];
        let Some(kind) = classify_status(status) else {
            continue;
        };

        let path = decode_path(&record[3..]);
        if path.as_os_str().is_empty() {
            continue;
        }

        if is_rename_or_copy(status) {
            let _ = records.next();
        }

        entries.push(GitStatusEntry {
            kind,
            path: display_root.join(path),
            is_untracked: status[0] == b'?' && status[1] == b'?',
        });
    }

    entries
}

fn classify_status(status: &[u8]) -> Option<ChangelogKind> {
    if status == b"!! " {
        return None;
    }

    let x = status[0] as char;
    let y = status[1] as char;

    if x == '?' && y == '?' {
        return Some(ChangelogKind::Added);
    }

    if x == 'D' || y == 'D' {
        return Some(ChangelogKind::Deleted);
    }

    if x == 'A' || y == 'A' {
        return Some(ChangelogKind::Added);
    }

    if matches!(x, 'M' | 'R' | 'C' | 'T' | 'U') || matches!(y, 'M' | 'R' | 'C' | 'T' | 'U') {
        return Some(ChangelogKind::Modified);
    }

    None
}

fn is_rename_or_copy(status: &[u8]) -> bool {
    let x = status[0] as char;
    let y = status[1] as char;
    matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C')
}

fn decode_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

pub(crate) fn format_changelog_path(path: &Path, cwd: &Path, display_root: &Path) -> String {
    if let Ok(stripped) = path.strip_prefix(cwd) {
        return if stripped.as_os_str().is_empty() {
            ".".to_string()
        } else {
            format!("./{}", stripped.display())
        };
    }

    if path.starts_with(display_root)
        && let Some(relative) = diff_paths(path, cwd)
    {
        return relative.display().to_string();
    }

    if let Some(home) = home_dir()
        && let Ok(stripped) = path.strip_prefix(home)
    {
        return if stripped.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", stripped.display())
        };
    }

    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    #[test]
    fn parse_status_marks_untracked_as_added() {
        let entries = parse_porcelain_v1_z(b"?? new.txt\0", Path::new("/repo"));
        assert_eq!(
            entries,
            vec![GitStatusEntry {
                kind: ChangelogKind::Added,
                path: PathBuf::from("/repo/new.txt"),
                is_untracked: true,
            }]
        );
    }

    #[test]
    fn parse_status_marks_modified_files() {
        let entries = parse_porcelain_v1_z(b" M changed.txt\0M  staged.txt\0", Path::new("/repo"));
        assert_eq!(
            entries,
            vec![
                GitStatusEntry {
                    kind: ChangelogKind::Modified,
                    path: PathBuf::from("/repo/changed.txt"),
                    is_untracked: false,
                },
                GitStatusEntry {
                    kind: ChangelogKind::Modified,
                    path: PathBuf::from("/repo/staged.txt"),
                    is_untracked: false,
                },
            ]
        );
    }

    #[test]
    fn parse_status_marks_deleted_files() {
        let entries =
            parse_porcelain_v1_z(b" D gone.txt\0D  staged-gone.txt\0", Path::new("/repo"));
        assert_eq!(
            entries,
            vec![
                GitStatusEntry {
                    kind: ChangelogKind::Deleted,
                    path: PathBuf::from("/repo/gone.txt"),
                    is_untracked: false,
                },
                GitStatusEntry {
                    kind: ChangelogKind::Deleted,
                    path: PathBuf::from("/repo/staged-gone.txt"),
                    is_untracked: false,
                },
            ]
        );
    }

    #[test]
    fn parse_status_treats_renames_as_modified() {
        let entries = parse_porcelain_v1_z(b"R  new.txt\0old.txt\0", Path::new("/repo"));
        assert_eq!(
            entries,
            vec![GitStatusEntry {
                kind: ChangelogKind::Modified,
                path: PathBuf::from("/repo/new.txt"),
                is_untracked: false,
            }]
        );
    }

    #[test]
    fn parse_status_ignores_ignored_files() {
        let entries = parse_porcelain_v1_z(b"!! ignored.txt\0", Path::new("/repo"));
        assert!(entries.is_empty());
    }

    #[test]
    fn format_path_uses_current_directory_prefix() {
        let cwd = Path::new("/repo/worktree");
        let display_root = Path::new("/repo");
        let path = Path::new("/repo/worktree/src/main.rs");
        assert_eq!(
            format_changelog_path(path, cwd, display_root),
            "./src/main.rs"
        );
    }

    #[test]
    fn format_path_uses_display_relative_path_outside_cwd() {
        let cwd = Path::new("/repo/worktree/app");
        let display_root = Path::new("/repo/worktree");
        let path = Path::new("/repo/worktree/shared/lib.rs");
        assert_eq!(
            format_changelog_path(path, cwd, display_root),
            "../shared/lib.rs"
        );
    }

    #[test]
    fn format_path_uses_home_prefix_outside_root() {
        let home = home_dir().expect("home directory");
        let cwd = home.join("project");
        let display_root = cwd.clone();
        let path = home.join("notes/todo.md");
        assert_eq!(
            format_changelog_path(&path, &cwd, &display_root),
            "~/notes/todo.md"
        );
    }

    #[test]
    fn format_path_keeps_absolute_path_outside_home() {
        let cwd = Path::new("/repo/worktree");
        let display_root = Path::new("/repo/worktree");
        let path = Path::new("/opt/shared/file.txt");
        assert_eq!(
            format_changelog_path(path, cwd, display_root),
            "/opt/shared/file.txt"
        );
    }

    #[test]
    fn changelog_entries_sort_by_group_then_display_path() {
        let cwd = Path::new("/repo/worktree");
        let display_root = Path::new("/repo/worktree");
        let mut entries = vec![
            ChangelogEntry {
                kind: ChangelogKind::Deleted,
                path: PathBuf::from("/repo/worktree/z.txt"),
                line_stats: None,
            },
            ChangelogEntry {
                kind: ChangelogKind::Added,
                path: PathBuf::from("/repo/worktree/b.txt"),
                line_stats: None,
            },
            ChangelogEntry {
                kind: ChangelogKind::Added,
                path: PathBuf::from("/repo/worktree/a.txt"),
                line_stats: None,
            },
            ChangelogEntry {
                kind: ChangelogKind::Modified,
                path: PathBuf::from("/repo/worktree/c.txt"),
                line_stats: None,
            },
        ];

        sort_entries(cwd, display_root, &mut entries);

        assert_eq!(
            entries,
            vec![
                ChangelogEntry {
                    kind: ChangelogKind::Added,
                    path: PathBuf::from("/repo/worktree/a.txt"),
                    line_stats: None,
                },
                ChangelogEntry {
                    kind: ChangelogKind::Added,
                    path: PathBuf::from("/repo/worktree/b.txt"),
                    line_stats: None,
                },
                ChangelogEntry {
                    kind: ChangelogKind::Modified,
                    path: PathBuf::from("/repo/worktree/c.txt"),
                    line_stats: None,
                },
                ChangelogEntry {
                    kind: ChangelogKind::Deleted,
                    path: PathBuf::from("/repo/worktree/z.txt"),
                    line_stats: None,
                },
            ]
        );
    }

    #[test]
    fn parse_numstat_reads_regular_paths() {
        let stats = parse_numstat_z(b"12\t3\tsrc/lib.rs\0", Path::new("/repo"));

        assert_eq!(
            stats,
            HashMap::from([(
                PathBuf::from("/repo/src/lib.rs"),
                LineStats {
                    added: 12,
                    removed: 3,
                },
            )])
        );
    }

    #[test]
    fn parse_numstat_uses_new_path_for_renames() {
        let stats = parse_numstat_z(b"1\t2\t\0old.rs\0new.rs\0", Path::new("/repo"));

        assert_eq!(
            stats,
            HashMap::from([(
                PathBuf::from("/repo/new.rs"),
                LineStats {
                    added: 1,
                    removed: 2,
                },
            )])
        );
    }

    #[test]
    fn parse_numstat_ignores_binary_counts() {
        let stats = parse_numstat_z(b"-\t-\timage.png\0", Path::new("/repo"));

        assert_eq!(stats, HashMap::new());
    }

    #[test]
    fn merge_line_stats_sums_existing_paths() {
        let path = PathBuf::from("/repo/src/lib.rs");
        let mut stats = HashMap::from([(
            path.clone(),
            LineStats {
                added: 1,
                removed: 2,
            },
        )]);

        merge_line_stats(
            &mut stats,
            HashMap::from([(
                path.clone(),
                LineStats {
                    added: 3,
                    removed: 4,
                },
            )]),
        );

        assert_eq!(
            stats,
            HashMap::from([(
                path,
                LineStats {
                    added: 4,
                    removed: 6,
                },
            )])
        );
    }

    #[tokio::test]
    async fn git_changelog_includes_modified_file_line_stats() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("changed.txt"), "one\ntwo\n").expect("write original file");
        run_git(dir.path(), ["add", "changed.txt"]);
        run_git(dir.path(), ["commit", "-m", "initial"]);

        std::fs::write(dir.path().join("changed.txt"), "one\nthree\nfour\n")
            .expect("write changed file");

        let output = get_git_changelog(dir.path())
            .await
            .expect("git changelog")
            .expect("git repo");
        let display_root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: display_root.clone(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Modified,
                    path: display_root.join("changed.txt"),
                    line_stats: Some(LineStats {
                        added: 2,
                        removed: 1,
                    }),
                }],
            }
        );
    }

    #[tokio::test]
    async fn git_changelog_includes_untracked_file_line_stats() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("new.txt"), "one\ntwo\n").expect("write new file");

        let output = get_git_changelog(dir.path())
            .await
            .expect("git changelog")
            .expect("git repo");
        let display_root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: display_root.clone(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Added,
                    path: display_root.join("new.txt"),
                    line_stats: Some(LineStats {
                        added: 2,
                        removed: 0,
                    }),
                }],
            }
        );
    }

    #[tokio::test]
    async fn git_changelog_includes_deleted_file_line_stats() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());
        let path = dir.path().join("gone.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("write original file");
        run_git(dir.path(), ["add", "gone.txt"]);
        run_git(dir.path(), ["commit", "-m", "initial"]);
        std::fs::remove_file(&path).expect("remove file");

        let output = get_git_changelog(dir.path())
            .await
            .expect("git changelog")
            .expect("git repo");
        let display_root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: display_root.clone(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Deleted,
                    path: display_root.join("gone.txt"),
                    line_stats: Some(LineStats {
                        added: 0,
                        removed: 3,
                    }),
                }],
            }
        );
    }

    #[tokio::test]
    async fn non_git_diff_marks_new_file_as_added() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("existing.txt"), "hello").expect("write existing file");
        let baseline = capture_directory_snapshot(dir.path().to_path_buf())
            .await
            .expect("baseline snapshot");
        std::fs::write(dir.path().join("new.txt"), "world").expect("write new file");

        let output = get_non_git_changelog(dir.path(), &baseline)
            .await
            .expect("non-git changelog");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: dir.path().to_path_buf(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Added,
                    path: dir.path().join("new.txt"),
                    line_stats: None,
                }],
            }
        );
    }

    #[tokio::test]
    async fn non_git_diff_marks_removed_file_as_deleted() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("gone.txt");
        std::fs::write(&path, "hello").expect("write file");
        let baseline = capture_directory_snapshot(dir.path().to_path_buf())
            .await
            .expect("baseline snapshot");
        std::fs::remove_file(&path).expect("remove file");

        let output = get_non_git_changelog(dir.path(), &baseline)
            .await
            .expect("non-git changelog");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: dir.path().to_path_buf(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Deleted,
                    path,
                    line_stats: None,
                }],
            }
        );
    }

    #[tokio::test]
    async fn non_git_diff_marks_changed_content_as_modified() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("changed.txt");
        std::fs::write(&path, "before").expect("write file");
        let baseline = capture_directory_snapshot(dir.path().to_path_buf())
            .await
            .expect("baseline snapshot");
        std::fs::write(&path, "after").expect("update file");

        let output = get_non_git_changelog(dir.path(), &baseline)
            .await
            .expect("non-git changelog");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: dir.path().to_path_buf(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Modified,
                    path,
                    line_stats: None,
                }],
            }
        );
    }

    #[tokio::test]
    async fn non_git_diff_includes_dotfiles() {
        let dir = tempdir().expect("tempdir");
        let baseline = capture_directory_snapshot(dir.path().to_path_buf())
            .await
            .expect("baseline snapshot");
        let path = dir.path().join(".env");
        std::fs::write(&path, "A=1").expect("write dotfile");

        let output = get_non_git_changelog(dir.path(), &baseline)
            .await
            .expect("non-git changelog");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: dir.path().to_path_buf(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Added,
                    path,
                    line_stats: None,
                }],
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_git_diff_detects_symlink_target_change() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().expect("tempdir");
        let target_a = dir.path().join("a.txt");
        let target_b = dir.path().join("b.txt");
        std::fs::write(&target_a, "a").expect("write target a");
        std::fs::write(&target_b, "b").expect("write target b");
        let link = dir.path().join("link.txt");
        symlink(&target_a, &link).expect("create symlink");

        let baseline = capture_directory_snapshot(dir.path().to_path_buf())
            .await
            .expect("baseline snapshot");

        std::fs::remove_file(&link).expect("remove old symlink");
        symlink(&target_b, &link).expect("create updated symlink");

        let output = get_non_git_changelog(dir.path(), &baseline)
            .await
            .expect("non-git changelog");

        assert_eq!(
            output,
            ChangelogOutput::Entries {
                display_root: dir.path().to_path_buf(),
                entries: vec![ChangelogEntry {
                    kind: ChangelogKind::Modified,
                    path: link,
                    line_stats: None,
                }],
            }
        );
    }

    fn init_git_repo(path: &Path) {
        run_git(path, ["init"]);
        run_git(path, ["config", "user.email", "test@example.com"]);
        run_git(path, ["config", "user.name", "Test User"]);
    }

    fn run_git<const N: usize>(path: &Path, args: [&str; N]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
