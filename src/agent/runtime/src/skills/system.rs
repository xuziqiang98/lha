use include_dir::Dir;
use lha_utils_absolute_path::AbsolutePathBuf;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

const SYSTEM_SKILLS_DIR: Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/skills/assets/samples");

const SYSTEM_SKILLS_DIR_NAME: &str = ".system";
const SKILLS_DIR_NAME: &str = "skills";
const SYSTEM_SKILLS_MARKER_FILENAME: &str = ".codex-system-skills.marker";
const SYSTEM_SKILLS_MARKER_SALT: &str = "v1";

/// Returns the on-disk cache location for embedded system skills.
///
/// This is typically located at `LHA_HOME/skills/.system`.
pub(crate) fn system_cache_root_dir(lha_home: &Path) -> PathBuf {
    AbsolutePathBuf::try_from(lha_home)
        .and_then(|lha_home| system_cache_root_dir_abs(&lha_home))
        .map(AbsolutePathBuf::into_path_buf)
        .unwrap_or_else(|_| lha_home.join(SKILLS_DIR_NAME).join(SYSTEM_SKILLS_DIR_NAME))
}

fn system_cache_root_dir_abs(lha_home: &AbsolutePathBuf) -> std::io::Result<AbsolutePathBuf> {
    lha_home.join(SKILLS_DIR_NAME)?.join(SYSTEM_SKILLS_DIR_NAME)
}

/// Installs embedded system skills into `LHA_HOME/skills/.system`.
///
/// Clears any existing system skills directory first and then writes the embedded
/// skills directory into place.
///
/// To avoid doing unnecessary work on every startup, a marker file is written
/// with a fingerprint of the embedded directory. When the marker matches, the
/// install is skipped.
pub(crate) fn install_system_skills(lha_home: &Path) -> Result<(), SystemSkillsError> {
    let lha_home = AbsolutePathBuf::try_from(lha_home)
        .map_err(|source| SystemSkillsError::io("normalize codex home dir", source))?;
    let skills_root_dir = lha_home
        .join(SKILLS_DIR_NAME)
        .map_err(|source| SystemSkillsError::io("resolve skills root dir", source))?;
    fs::create_dir_all(skills_root_dir.as_path())
        .map_err(|source| SystemSkillsError::io("create skills root dir", source))?;

    let dest_system = system_cache_root_dir_abs(&lha_home)
        .map_err(|source| SystemSkillsError::io("resolve system skills cache root dir", source))?;

    let marker_path = dest_system
        .join(SYSTEM_SKILLS_MARKER_FILENAME)
        .map_err(|source| SystemSkillsError::io("resolve system skills marker path", source))?;
    let expected_fingerprint = embedded_system_skills_fingerprint();
    if dest_system.as_path().is_dir()
        && read_marker(&marker_path).is_ok_and(|marker| marker == expected_fingerprint)
    {
        return Ok(());
    }

    if dest_system.as_path().exists() {
        fs::remove_dir_all(dest_system.as_path())
            .map_err(|source| SystemSkillsError::io("remove existing system skills dir", source))?;
    }

    write_embedded_dir(&SYSTEM_SKILLS_DIR, &dest_system)?;
    fs::write(marker_path.as_path(), format!("{expected_fingerprint}\n"))
        .map_err(|source| SystemSkillsError::io("write system skills marker", source))?;
    Ok(())
}

fn read_marker(path: &AbsolutePathBuf) -> Result<String, SystemSkillsError> {
    Ok(fs::read_to_string(path.as_path())
        .map_err(|source| SystemSkillsError::io("read system skills marker", source))?
        .trim()
        .to_string())
}

fn embedded_system_skills_fingerprint() -> String {
    let mut items = Vec::new();
    collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
    items.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    let mut hasher = DefaultHasher::new();
    SYSTEM_SKILLS_MARKER_SALT.hash(&mut hasher);
    for (path, contents_hash) in items {
        path.hash(&mut hasher);
        contents_hash.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn collect_fingerprint_items(dir: &Dir<'_>, items: &mut Vec<(String, Option<u64>)>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                items.push((subdir.path().to_string_lossy().to_string(), None));
                collect_fingerprint_items(subdir, items);
            }
            include_dir::DirEntry::File(file) => {
                let mut file_hasher = DefaultHasher::new();
                file.contents().hash(&mut file_hasher);
                items.push((
                    file.path().to_string_lossy().to_string(),
                    Some(file_hasher.finish()),
                ));
            }
        }
    }
}

/// Writes the embedded `include_dir::Dir` to disk under `dest`.
///
/// Preserves the embedded directory structure.
fn write_embedded_dir(dir: &Dir<'_>, dest: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    fs::create_dir_all(dest.as_path())
        .map_err(|source| SystemSkillsError::io("create system skills dir", source))?;

    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_dest = dest.join(subdir.path()).map_err(|source| {
                    SystemSkillsError::io("resolve system skills subdir", source)
                })?;
                fs::create_dir_all(subdir_dest.as_path()).map_err(|source| {
                    SystemSkillsError::io("create system skills subdir", source)
                })?;
                write_embedded_dir(subdir, dest)?;
            }
            include_dir::DirEntry::File(file) => {
                let path = dest.join(file.path()).map_err(|source| {
                    SystemSkillsError::io("resolve system skills file", source)
                })?;
                if let Some(parent) = path.as_path().parent() {
                    fs::create_dir_all(parent).map_err(|source| {
                        SystemSkillsError::io("create system skills file parent", source)
                    })?;
                }
                fs::write(path.as_path(), file.contents())
                    .map_err(|source| SystemSkillsError::io("write system skill file", source))?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum SystemSkillsError {
    #[error("io error while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl SystemSkillsError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}

#[cfg(test)]
mod tests {
    use super::SYSTEM_SKILLS_DIR;
    use super::collect_fingerprint_items;

    #[test]
    fn fingerprint_traverses_nested_entries() {
        let mut items = Vec::new();
        collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
        let mut paths: Vec<String> = items.into_iter().map(|(path, _)| path).collect();
        paths.sort_unstable();

        for expected in [
            "skill-creator/SKILL.md",
            "skill-creator/scripts/init_skill.py",
            "skill-installer/SKILL.md",
            "imagegen/SKILL.md",
            "imagegen/scripts/image_gen.py",
            "imagegen/scripts/remove_chroma_key.py",
        ] {
            assert!(
                paths
                    .binary_search_by(|probe| probe.as_str().cmp(expected))
                    .is_ok(),
                "expected embedded system skills fingerprint to include {expected}"
            );
        }
    }

    #[test]
    fn skill_installer_keeps_codex_github_source() {
        let list_skills = SYSTEM_SKILLS_DIR
            .get_file("skill-installer/scripts/list-skills.py")
            .expect("list-skills.py should be embedded");
        let list_skills =
            std::str::from_utf8(list_skills.contents()).expect("list-skills.py should be utf-8");
        assert!(
            list_skills.contains("DEFAULT_REPO = \"openai/skills\""),
            "skill-installer must list curated skills from openai/skills"
        );
        assert!(
            list_skills.contains("DEFAULT_PATH = \"skills/.curated\""),
            "skill-installer must list the same curated path as Codex"
        );
        assert!(
            list_skills.contains("github_request(url, \"codex-skill-list\")"),
            "skill-installer list requests should match Codex GitHub request behavior"
        );

        let install_skill = SYSTEM_SKILLS_DIR
            .get_file("skill-installer/scripts/install-skill-from-github.py")
            .expect("install-skill-from-github.py should be embedded");
        let install_skill = std::str::from_utf8(install_skill.contents())
            .expect("install-skill-from-github.py should be utf-8");
        assert!(
            install_skill.contains("github_request(url, \"codex-skill-install\")"),
            "skill-installer install requests should match Codex GitHub request behavior"
        );
    }
}
