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
            | "lha-stdio-to-uds"
            | "test_stdio_server"
            | "test_streamable_http_server"
    )
}

fn legacy_arg0_alias(name: &str) -> io::Result<PathBuf> {
    let alias_dir = ARG0_ALIAS_DIR
        .get_or_init(|| std::env::temp_dir().join(format!("lha-test-arg0-{}", std::process::id())));
    std::fs::create_dir_all(alias_dir)?;
    let alias_name = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let alias = alias_dir.join(alias_name);
    if alias.exists() {
        return Ok(alias);
    }

    let current_exe = std::env::current_exe()?;
    #[cfg(unix)]
    symlink(&current_exe, &alias)?;
    #[cfg(windows)]
    std::fs::copy(&current_exe, &alias).map(|_| ())?;

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
