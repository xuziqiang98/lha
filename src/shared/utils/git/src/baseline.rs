use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBaselineDiff {
    pub changes: Vec<GitBaselineChange>,
    pub unified_diff: String,
}

impl GitBaselineDiff {
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty() || !self.unified_diff.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBaselineChange {
    pub status: GitBaselineChangeStatus,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBaselineChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Unknown,
}

impl GitBaselineChangeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Unmerged => "U",
            Self::Unknown => "?",
        }
    }
}

pub fn ensure_git_baseline_repository(root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    if is_valid_git_repository(root) {
        return Ok(());
    }
    remove_git_dir_or_file(root)?;
    init_and_commit(root)
}

pub fn reset_git_repository(root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    remove_git_dir_or_file(root)?;
    init_and_commit(root)
}

pub fn diff_since_latest_init(root: &Path) -> anyhow::Result<GitBaselineDiff> {
    ensure_git_baseline_repository(root)?;
    run_git(root, ["add", "-N", "."])?;
    let status = run_git(root, ["status", "--porcelain=v1"])?;
    let changes = parse_status(status.stdout.as_str());
    let unified_diff = run_git(root, ["diff", "--no-ext-diff", "--binary", "HEAD", "--"])
        .map(|output| output.stdout)?;
    Ok(GitBaselineDiff {
        changes,
        unified_diff,
    })
}

fn init_and_commit(root: &Path) -> anyhow::Result<()> {
    run_git(root, ["init"])?;
    run_git(root, ["add", "-A"])?;
    run_git(
        root,
        [
            "-c",
            "user.name=LHA Memory",
            "-c",
            "user.email=lha-memory@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Initialize LHA memory git baseline",
        ],
    )?;
    Ok(())
}

fn remove_git_dir_or_file(root: &Path) -> std::io::Result<()> {
    let git_dir = root.join(".git");
    let Ok(metadata) = std::fs::symlink_metadata(&git_dir) else {
        return Ok(());
    };
    if metadata.is_dir() {
        std::fs::remove_dir_all(git_dir)
    } else {
        std::fs::remove_file(git_dir)
    }
}

fn is_valid_git_repository(root: &Path) -> bool {
    let Ok(canonical_root) = canonical_root(root) else {
        return false;
    };
    let inside_work_tree = run_git(root, ["rev-parse", "--is-inside-work-tree"])
        .map(|output| output.stdout.trim() == "true")
        .unwrap_or(false);
    if !inside_work_tree {
        return false;
    }
    let Ok(Some(top_level)) = git_top_level(root) else {
        return false;
    };
    if top_level != canonical_root {
        return false;
    }
    run_git(root, ["rev-parse", "--verify", "HEAD"])
        .map(|output| !output.stdout.trim().is_empty())
        .unwrap_or(false)
}

fn canonical_root(root: &Path) -> anyhow::Result<PathBuf> {
    Ok(root.canonicalize()?)
}

fn git_top_level(root: &Path) -> anyhow::Result<Option<PathBuf>> {
    let output = match run_git(root, ["rev-parse", "--show-toplevel"]) {
        Ok(output) => output,
        Err(_) => return Ok(None),
    };
    let top_level = output.stdout.trim();
    if top_level.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(top_level).canonicalize()?))
}

struct GitOutput {
    stdout: String,
}

fn run_git<I, S>(root: &Path, args: I) -> anyhow::Result<GitOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "git command failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(GitOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
    })
}

fn parse_status(status: &str) -> Vec<GitBaselineChange> {
    status
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            let code = &line[..2];
            let path = line[3..].trim().to_string();
            if path.is_empty() {
                return None;
            }
            Some(GitBaselineChange {
                status: status_from_code(code),
                path,
            })
        })
        .collect()
}

fn status_from_code(code: &str) -> GitBaselineChangeStatus {
    if code.contains('U') {
        GitBaselineChangeStatus::Unmerged
    } else if code.contains('R') {
        GitBaselineChangeStatus::Renamed
    } else if code.contains('C') {
        GitBaselineChangeStatus::Copied
    } else if code.contains('A') || code.contains('?') {
        GitBaselineChangeStatus::Added
    } else if code.contains('D') {
        GitBaselineChangeStatus::Deleted
    } else if code.contains('M') {
        GitBaselineChangeStatus::Modified
    } else if code.contains('T') {
        GitBaselineChangeStatus::TypeChanged
    } else {
        GitBaselineChangeStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn baseline_diff_detects_added_file() {
        let tmp = TempDir::new().expect("tempdir");
        ensure_git_baseline_repository(tmp.path()).expect("baseline");
        std::fs::write(tmp.path().join("added.txt"), "hello").expect("write");

        let diff = diff_since_latest_init(tmp.path()).expect("diff");

        assert_change(&diff, GitBaselineChangeStatus::Added, "added.txt");
    }

    #[test]
    fn baseline_diff_detects_modified_file() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("memory.md"), "before").expect("write");
        ensure_git_baseline_repository(tmp.path()).expect("baseline");
        std::fs::write(tmp.path().join("memory.md"), "after").expect("write");

        let diff = diff_since_latest_init(tmp.path()).expect("diff");

        assert_change(&diff, GitBaselineChangeStatus::Modified, "memory.md");
    }

    #[test]
    fn baseline_diff_detects_deleted_file() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("memory.md"), "before").expect("write");
        ensure_git_baseline_repository(tmp.path()).expect("baseline");
        std::fs::remove_file(tmp.path().join("memory.md")).expect("delete");

        let diff = diff_since_latest_init(tmp.path()).expect("diff");

        assert_change(&diff, GitBaselineChangeStatus::Deleted, "memory.md");
    }

    #[test]
    fn baseline_diff_detects_nested_path() {
        let tmp = TempDir::new().expect("tempdir");
        ensure_git_baseline_repository(tmp.path()).expect("baseline");
        std::fs::create_dir_all(tmp.path().join("rollout_summaries")).expect("mkdir");
        std::fs::write(tmp.path().join("rollout_summaries/thread.md"), "summary").expect("write");

        let diff = diff_since_latest_init(tmp.path()).expect("diff");

        assert_change(
            &diff,
            GitBaselineChangeStatus::Added,
            "rollout_summaries/thread.md",
        );
    }

    #[test]
    fn reset_baseline_clears_diff() {
        let tmp = TempDir::new().expect("tempdir");
        ensure_git_baseline_repository(tmp.path()).expect("baseline");
        std::fs::write(tmp.path().join("memory.md"), "memory").expect("write");
        assert!(
            diff_since_latest_init(tmp.path())
                .expect("diff")
                .has_changes()
        );

        reset_git_repository(tmp.path()).expect("reset");

        assert!(
            !diff_since_latest_init(tmp.path())
                .expect("diff")
                .has_changes()
        );
    }

    #[test]
    fn baseline_diff_handles_binary_non_utf8_without_panic() {
        let tmp = TempDir::new().expect("tempdir");
        ensure_git_baseline_repository(tmp.path()).expect("baseline");
        std::fs::write(tmp.path().join("binary.bin"), [0, 159, 146, 150]).expect("write");

        let diff = diff_since_latest_init(tmp.path()).expect("diff");

        assert_change(&diff, GitBaselineChangeStatus::Added, "binary.bin");
    }

    #[test]
    fn invalid_git_directory_is_replaced() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".git")).expect("mkdir");
        std::fs::write(tmp.path().join(".git/garbage"), "not git").expect("write");

        ensure_git_baseline_repository(tmp.path()).expect("baseline");

        assert!(is_valid_git_repository(tmp.path()));
    }

    #[test]
    fn invalid_git_file_is_replaced() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join(".git"), "not git").expect("write");

        ensure_git_baseline_repository(tmp.path()).expect("baseline");

        assert!(is_valid_git_repository(tmp.path()));
    }

    #[test]
    fn nested_memory_root_gets_own_git_repo() {
        let tmp = TempDir::new().expect("tempdir");
        let parent = tmp.path();
        std::fs::write(parent.join(".gitignore"), "memories/\n").expect("write gitignore");
        init_and_commit(parent).expect("parent baseline");
        let memories = parent.join("memories");
        std::fs::create_dir_all(&memories).expect("mkdir memories");

        ensure_git_baseline_repository(&memories).expect("memory baseline");

        let top_level = git_top_level(&memories)
            .expect("top level")
            .expect("top level should exist");
        assert_eq!(
            top_level,
            memories.canonicalize().expect("canonical memories")
        );
        let parent_status = run_git(parent, ["status", "--porcelain=v1"])
            .expect("parent status")
            .stdout;
        assert_eq!(parent_status, "");
    }

    #[test]
    fn diff_inside_nested_memory_root_does_not_touch_parent_index() {
        let tmp = TempDir::new().expect("tempdir");
        let parent = tmp.path();
        std::fs::write(parent.join(".gitignore"), "memories/\n").expect("write gitignore");
        init_and_commit(parent).expect("parent baseline");
        let memories = parent.join("memories");
        ensure_git_baseline_repository(&memories).expect("memory baseline");
        std::fs::write(memories.join("memory.md"), "remember this").expect("write memory");

        let diff = diff_since_latest_init(&memories).expect("diff");

        assert_eq!(
            diff.changes,
            vec![GitBaselineChange {
                status: GitBaselineChangeStatus::Added,
                path: "memory.md".to_string(),
            }]
        );
        let parent_cached_diff = run_git(parent, ["diff", "--cached", "--name-only"])
            .expect("parent cached diff")
            .stdout;
        assert_eq!(parent_cached_diff, "");
    }

    fn assert_change(diff: &GitBaselineDiff, status: GitBaselineChangeStatus, path: &'static str) {
        assert!(
            diff.changes
                .iter()
                .any(|change| change.status == status && change.path == path),
            "expected {status:?} change for {path}; got {:?}",
            diff.changes
        );
    }

    #[test]
    fn status_labels_match_git_porcelain_codes() {
        assert_eq!(GitBaselineChangeStatus::Added.label(), "A");
        assert_eq!(GitBaselineChangeStatus::Modified.label(), "M");
        assert_eq!(GitBaselineChangeStatus::Deleted.label(), "D");
    }
}
