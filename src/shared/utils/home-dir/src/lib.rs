use dirs::home_dir;
use std::path::PathBuf;

/// Returns the path to the Adam configuration directory, which can be
/// specified by the `ADAM_HOME` environment variable. If not set, defaults to
/// `~/.adam`.
///
/// - If `ADAM_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `ADAM_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_adam_home() -> std::io::Result<PathBuf> {
    let adam_home_env = std::env::var("ADAM_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_adam_home_from_env(adam_home_env.as_deref())
}

fn find_adam_home_from_env(adam_home_env: Option<&str>) -> std::io::Result<PathBuf> {
    // Honor the `ADAM_HOME` environment variable when it is set to allow users
    // (and tests) to override the default location.
    match adam_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("ADAM_HOME points to {val:?}, but that path does not exist"),
                ),
                _ => std::io::Error::new(
                    err.kind(),
                    format!("failed to read ADAM_HOME {val:?}: {err}"),
                ),
            })?;

            if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("ADAM_HOME points to {val:?}, but that path is not a directory"),
                ))
            } else {
                path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize ADAM_HOME {val:?}: {err}"),
                    )
                })
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".adam");
            Ok(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_adam_home_from_env;
    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use serial_test::serial;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_adam_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-adam-home");
        let missing_str = missing
            .to_str()
            .expect("missing adam home path should be valid utf-8");

        let err = find_adam_home_from_env(Some(missing_str)).expect_err("missing ADAM_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("ADAM_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_adam_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("adam-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file adam home path should be valid utf-8");

        let err = find_adam_home_from_env(Some(file_str)).expect_err("file ADAM_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_adam_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp adam home path should be valid utf-8");

        let resolved = find_adam_home_from_env(Some(temp_str)).expect("valid ADAM_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_adam_home_without_env_uses_default_home_dir() {
        let resolved = find_adam_home_from_env(None).expect("default ADAM_HOME");
        let mut expected = home_dir().expect("home dir");
        expected.push(".adam");
        assert_eq!(resolved, expected);
    }

    #[test]
    #[serial]
    fn find_adam_home_ignores_legacy_codey_home_env() {
        let temp_home = TempDir::new().expect("temp home");
        let legacy_env_var = concat!("CODEY", "_HOME");
        let previous_adam_home = std::env::var_os("ADAM_HOME");
        let previous_codey_home = std::env::var_os(legacy_env_var);
        unsafe {
            std::env::remove_var("ADAM_HOME");
            std::env::set_var(legacy_env_var, temp_home.path());
        }

        let resolved = super::find_adam_home().expect("default ADAM_HOME");
        let mut expected = home_dir().expect("home dir");
        expected.push(".adam");
        assert_eq!(resolved, expected);

        unsafe {
            match previous_adam_home {
                Some(value) => std::env::set_var("ADAM_HOME", value),
                None => std::env::remove_var("ADAM_HOME"),
            }
            match previous_codey_home {
                Some(value) => std::env::set_var(legacy_env_var, value),
                None => std::env::remove_var(legacy_env_var),
            }
        }
    }
}
