use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::unix::fs::symlink;

static ARG0_ALIAS_DIR: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum CargoBinError {
    #[error("failed to read current directory")]
    CurrentDir {
        #[source]
        source: std::io::Error,
    },
    #[error("CARGO_BIN_EXE env var {key} resolved to {path:?}, but it does not exist")]
    ResolvedPathDoesNotExist { key: String, path: PathBuf },
    #[error("could not locate binary {name:?}; tried env vars {env_keys:?}; {fallback}")]
    NotFound {
        name: String,
        env_keys: Vec<String>,
        fallback: String,
    },
}

/// Returns an absolute path to a binary target built for the current test run.
pub fn cargo_bin(name: &str) -> Result<PathBuf, CargoBinError> {
    let env_keys = cargo_bin_env_keys(name);
    for key in &env_keys {
        if let Some(value) = std::env::var_os(key) {
            return resolve_bin_from_env(key, value);
        }
    }
    match assert_cmd::Command::cargo_bin(name) {
        Ok(cmd) => {
            let mut path = PathBuf::from(cmd.get_program());
            if !path.is_absolute() {
                path = std::env::current_dir()
                    .map_err(|source| CargoBinError::CurrentDir { source })?
                    .join(path);
            }
            if path.exists() {
                Ok(path)
            } else {
                Err(CargoBinError::ResolvedPathDoesNotExist {
                    key: "assert_cmd::Command::cargo_bin".to_owned(),
                    path,
                })
            }
        }
        Err(err) => {
            if is_single_binary_alias(name) {
                return legacy_arg0_alias(name).map_err(|alias_err| CargoBinError::NotFound {
                    name: name.to_owned(),
                    env_keys,
                    fallback: format!(
                        "assert_cmd fallback failed: {err}; arg0 alias fallback failed: {alias_err}"
                    ),
                });
            }

            Err(CargoBinError::NotFound {
                name: name.to_owned(),
                env_keys,
                fallback: format!("assert_cmd fallback failed: {err}"),
            })
        }
    }
}

fn is_single_binary_alias(name: &str) -> bool {
    matches!(
        name,
        "lha-exec"
            | "lha-app-server"
            | "lha-linux-sandbox"
            | "lha-mcp-server"
            | "codex-responses-api-proxy"
            | "lha-stdio-to-uds"
            | "test_stdio_server"
            | "test_streamable_http_server"
    )
}

fn legacy_arg0_alias(name: &str) -> io::Result<PathBuf> {
    let lha_binary = resolve_lha_binary()?;
    let alias_dir = ARG0_ALIAS_DIR
        .get_or_init(|| std::env::temp_dir().join(format!("lha-test-arg0-{}", std::process::id())));
    create_arg0_alias(alias_dir, name, &lha_binary)
}

fn resolve_lha_binary() -> io::Result<PathBuf> {
    for key in cargo_bin_env_keys("lha") {
        if let Some(value) = std::env::var_os(&key) {
            return resolve_bin_from_env(&key, value).map_err(cargo_bin_error_to_io);
        }
    }

    let cmd = assert_cmd::Command::cargo_bin("lha").map_err(|err| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("assert_cmd::Command::cargo_bin(\"lha\") failed: {err}"),
        )
    })?;
    let mut path = PathBuf::from(cmd.get_program());
    if !path.is_absolute() {
        path = std::env::current_dir()?.join(path);
    }
    if path.exists() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "assert_cmd::Command::cargo_bin(\"lha\") resolved to {path:?}, but it does not exist"
            ),
        ))
    }
}

fn cargo_bin_error_to_io(err: CargoBinError) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, err.to_string())
}

fn create_arg0_alias(alias_dir: &Path, name: &str, target_lha: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(alias_dir)?;
    let alias_name = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let alias = alias_dir.join(alias_name);
    if std::fs::symlink_metadata(&alias).is_ok() {
        return Ok(alias);
    }

    #[cfg(unix)]
    symlink(target_lha, &alias)?;
    #[cfg(windows)]
    std::fs::copy(target_lha, &alias).map(|_| ())?;

    Ok(alias)
}

fn cargo_bin_env_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    keys.push(format!("CARGO_BIN_EXE_{name}"));

    // Cargo replaces dashes in target names when exporting env vars.
    let underscore_name = name.replace('-', "_");
    if underscore_name != name {
        keys.push(format!("CARGO_BIN_EXE_{underscore_name}"));
    }

    keys
}

fn resolve_bin_from_env(key: &str, value: OsString) -> Result<PathBuf, CargoBinError> {
    let raw = PathBuf::from(&value);
    if raw.is_absolute() && raw.exists() {
        return Ok(raw);
    }

    Err(CargoBinError::ResolvedPathDoesNotExist {
        key: key.to_owned(),
        path: raw,
    })
}

/// Macro that derives the path to a test resource at runtime, the value of
/// which depends on the calling crate's manifest directory. Note the return
/// value may be a relative or absolute path. (Incidentally, this is a macro
/// rather than a function because it reads compile-time environment variables
/// that need to be captured at the call site.)
///
/// This is expected to be used exclusively in test code because LHA CLI is a
/// standalone binary with no packaged resources.
macro_rules! find_resource {
    ($resource:expr) => {{
        let resource = std::path::Path::new(&$resource);
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        std::io::Result::Ok(manifest_dir.join(resource))
    }};
}

pub(crate) use find_resource;

pub fn resolve_cargo_runfile(resource: &Path) -> std::io::Result<PathBuf> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir.join(resource))
}

pub fn repo_root() -> io::Result<PathBuf> {
    let marker = resolve_cargo_runfile(Path::new("repo_root.marker"))?;
    for ancestor in marker.ancestors().skip(1) {
        if ancestor.join(".git").exists() {
            return Ok(ancestor.to_path_buf());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "failed to locate repository root from repo_root.marker",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn creates_production_arg0_alias_to_lha_binary() -> io::Result<()> {
        assert_alias_points_to_lha_binary("lha-exec")
    }

    #[test]
    fn creates_test_server_arg0_alias_to_lha_binary() -> io::Result<()> {
        assert_alias_points_to_lha_binary("test_stdio_server")
    }

    fn assert_alias_points_to_lha_binary(name: &str) -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let lha_binary = temp_dir
            .path()
            .join(if cfg!(windows) { "lha.exe" } else { "lha" });
        let lha_contents = b"fake lha binary";
        std::fs::write(&lha_binary, lha_contents)?;

        let alias = create_arg0_alias(&temp_dir.path().join("aliases"), name, &lha_binary)?;

        #[cfg(unix)]
        assert_eq!(std::fs::read_link(&alias)?, lha_binary);
        #[cfg(windows)]
        assert_eq!(std::fs::read(&alias)?, lha_contents);

        Ok(())
    }
}
