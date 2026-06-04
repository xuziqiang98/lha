use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;

use tracing::warn;

const ASSERTION_REASON: &str = "LHA is running an active turn";
const APP_ID: &str = "codex";
// Keep the blocker process alive "long enough" without needing restarts.
// This is `i32::MAX` seconds, which is accepted by common `sleep` implementations.
const BLOCKER_SLEEP_SECONDS: &str = "2147483647";

#[derive(Debug, Default)]
pub(crate) struct LinuxSleepInhibitor {
    state: InhibitState,
    preferred_backend: Option<LinuxBackend>,
    missing_backend_logged: bool,
}

pub(crate) use LinuxSleepInhibitor as SleepInhibitor;

#[derive(Debug, Default)]
enum InhibitState {
    #[default]
    Inactive,
    Active {
        backend: LinuxBackend,
        child: Child,
    },
}

#[derive(Debug, Clone, Copy)]
enum LinuxBackend {
    SystemdInhibit,
    GnomeSessionInhibit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveBackendStatus {
    Running,
    Exited,
    Unknown,
}

impl LinuxSleepInhibitor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn acquire(&mut self) {
        let should_restart = match &mut self.state {
            InhibitState::Inactive => true,
            InhibitState::Active { backend, child } => {
                match classify_active_backend_status(*backend, try_wait_retry_interrupted(child)) {
                    ActiveBackendStatus::Running | ActiveBackendStatus::Unknown => false,
                    ActiveBackendStatus::Exited => true,
                }
            }
        };

        if !should_restart {
            return;
        }

        self.state = InhibitState::Inactive;
        let should_log_backend_failures = !self.missing_backend_logged;
        let backends = match self.preferred_backend {
            Some(LinuxBackend::SystemdInhibit) => [
                LinuxBackend::SystemdInhibit,
                LinuxBackend::GnomeSessionInhibit,
            ],
            Some(LinuxBackend::GnomeSessionInhibit) => [
                LinuxBackend::GnomeSessionInhibit,
                LinuxBackend::SystemdInhibit,
            ],
            None => [
                LinuxBackend::SystemdInhibit,
                LinuxBackend::GnomeSessionInhibit,
            ],
        };

        for backend in backends {
            match spawn_backend(backend) {
                Ok(mut child) => match try_wait_retry_interrupted(&mut child) {
                    Ok(None) => {
                        self.state = InhibitState::Active { backend, child };
                        self.preferred_backend = Some(backend);
                        self.missing_backend_logged = false;
                        return;
                    }
                    Ok(Some(status)) => {
                        if should_log_backend_failures {
                            warn!(
                                ?backend,
                                ?status,
                                "Linux sleep inhibitor backend exited immediately"
                            );
                        }
                    }
                    Err(error) => {
                        if should_log_backend_failures {
                            warn!(
                                ?backend,
                                reason = %error,
                                "Failed to query Linux sleep inhibitor backend status after spawn"
                            );
                        }
                        if let Err(kill_error) = terminate_backend(&mut child) {
                            warn!(
                                ?backend,
                                reason = %kill_error,
                                "Failed to stop Linux sleep inhibitor backend after status probe failure"
                            );
                        }
                    }
                },
                Err(error) => {
                    if should_log_backend_failures && error.kind() != std::io::ErrorKind::NotFound {
                        warn!(
                            ?backend,
                            reason = %error,
                            "Failed to start Linux sleep inhibitor backend"
                        );
                    }
                }
            }
        }

        if should_log_backend_failures {
            warn!("No Linux sleep inhibitor backend is available");
            self.missing_backend_logged = true;
        }
    }

    pub(crate) fn release(&mut self) {
        match std::mem::take(&mut self.state) {
            InhibitState::Inactive => {}
            InhibitState::Active { backend, mut child } => {
                if let Err(error) = terminate_backend(&mut child) {
                    warn!(?backend, reason = %error, "Failed to stop Linux sleep inhibitor backend");
                }
            }
        }
    }
}

impl Drop for LinuxSleepInhibitor {
    fn drop(&mut self) {
        self.release();
    }
}

fn spawn_backend(backend: LinuxBackend) -> Result<Child, std::io::Error> {
    // Ensure the helper receives SIGTERM when the original parent dies.
    // `parent_pid` is captured before spawn and checked in `pre_exec` to avoid
    // the fork/exec race where the parent exits before PDEATHSIG is armed.
    // SAFETY: `getpid` has no preconditions and is safe to call here.
    let parent_pid = unsafe { libc::getpid() };
    let mut command = match backend {
        LinuxBackend::SystemdInhibit => {
            let mut command = Command::new("systemd-inhibit");
            command.args([
                "--what=idle",
                "--mode=block",
                "--who",
                APP_ID,
                "--why",
                ASSERTION_REASON,
                "--",
                "sleep",
                BLOCKER_SLEEP_SECONDS,
            ]);
            command
        }
        LinuxBackend::GnomeSessionInhibit => {
            let mut command = Command::new("gnome-session-inhibit");
            command.args([
                "--inhibit",
                "idle",
                "--reason",
                ASSERTION_REASON,
                "sleep",
                BLOCKER_SLEEP_SECONDS,
            ]);
            command
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `pre_exec` must be registered before spawn. The closure only
    // performs libc setup for the child process and returns an `io::Error`
    // when parent-death signal setup fails.
    unsafe {
        command.pre_exec(move || {
            set_process_group()?;
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                libc::raise(libc::SIGTERM);
            }
            Ok(())
        });
    }

    command.spawn()
}

fn classify_active_backend_status(
    backend: LinuxBackend,
    status: std::io::Result<Option<ExitStatus>>,
) -> ActiveBackendStatus {
    match status {
        Ok(None) => ActiveBackendStatus::Running,
        Ok(Some(status)) => {
            warn!(
                ?backend,
                ?status,
                "Linux sleep inhibitor backend exited unexpectedly; attempting fallback"
            );
            ActiveBackendStatus::Exited
        }
        Err(error) => {
            warn!(
                ?backend,
                reason = %error,
                "Failed to query Linux sleep inhibitor backend status; preserving existing helper"
            );
            ActiveBackendStatus::Unknown
        }
    }
}

fn try_wait_retry_interrupted(child: &mut Child) -> std::io::Result<Option<ExitStatus>> {
    loop {
        match child.try_wait() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

fn terminate_backend(child: &mut Child) -> std::io::Result<()> {
    let pid = child.id();
    kill_process_group_by_pid(pid)?;

    wait_for_child(child)
}

fn wait_for_child(child: &mut Child) -> std::io::Result<()> {
    loop {
        match child.wait() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if child_exited(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn set_process_group() -> std::io::Result<()> {
    let result = unsafe { libc::setpgid(0, 0) };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn kill_process_group_by_pid(pid: u32) -> std::io::Result<()> {
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    if pgid == -1 {
        let error = std::io::Error::last_os_error();
        if process_not_found(&error) {
            return Ok(());
        }
        return Err(error);
    }

    let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if process_not_found(&error) {
            return Ok(());
        }
        return Err(error);
    }

    Ok(())
}

fn child_exited(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::InvalidInput)
}

fn process_not_found(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH) || error.kind() == std::io::ErrorKind::NotFound
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::ActiveBackendStatus;
    use super::BLOCKER_SLEEP_SECONDS;
    use super::InhibitState;
    use super::LinuxBackend;
    use super::LinuxSleepInhibitor;
    use super::classify_active_backend_status;
    use super::set_process_group;

    #[test]
    fn status_probe_errors_preserve_existing_helper() {
        let status = classify_active_backend_status(
            LinuxBackend::SystemdInhibit,
            Err(std::io::Error::other("boom")),
        );

        assert_eq!(status, ActiveBackendStatus::Unknown);
    }

    #[test]
    fn running_helpers_remain_active() {
        let status = classify_active_backend_status(LinuxBackend::SystemdInhibit, Ok(None));

        assert_eq!(status, ActiveBackendStatus::Running);
    }

    #[test]
    fn exited_helpers_trigger_restart() {
        let status = classify_active_backend_status(
            LinuxBackend::SystemdInhibit,
            Ok(Some(std::process::ExitStatus::from_raw(9 << 8))),
        );

        assert_eq!(status, ActiveBackendStatus::Exited);
    }

    #[test]
    fn sleep_seconds_is_i32_max() {
        let i32_max = i32::MAX;
        assert_eq!(BLOCKER_SLEEP_SECONDS, format!("{i32_max}"));
    }

    #[test]
    fn release_kills_the_full_process_group() {
        let pid_file = blocker_pid_file_path();
        let mut child = std::process::Command::new("sh");
        child.args([
            "-c",
            &format!(
                "sleep {BLOCKER_SLEEP_SECONDS} & echo $! > '{}' ; wait",
                pid_file.display()
            ),
        ]);

        // SAFETY: `pre_exec` runs after fork and before exec. The closure only
        // calls `setpgid` so the test helper mirrors production process-group setup.
        unsafe {
            child.pre_exec(set_process_group);
        }

        let child = child.spawn().expect("spawn wrapper process");
        let blocker_pid = wait_for_blocker_pid(&pid_file);

        let mut inhibitor = LinuxSleepInhibitor {
            state: InhibitState::Active {
                backend: LinuxBackend::GnomeSessionInhibit,
                child,
            },
            preferred_backend: None,
            missing_backend_logged: false,
        };

        inhibitor.release();

        wait_for_process_exit(blocker_pid);
        let _ = fs::remove_file(pid_file);
    }

    fn blocker_pid_file_path() -> std::path::PathBuf {
        let process_id = std::process::id();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "codex-sleep-inhibitor-blocker-{process_id}-{unique}.pid"
        ))
    }

    fn wait_for_blocker_pid(pid_file: &std::path::Path) -> libc::pid_t {
        for _ in 0..50 {
            if let Ok(contents) = fs::read_to_string(pid_file)
                && let Ok(pid) = contents.trim().parse::<libc::pid_t>()
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        panic!(
            "timed out waiting for blocker pid file at {}",
            pid_file.display()
        );
    }

    fn process_exists(pid: libc::pid_t) -> bool {
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 {
            return true;
        }

        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    fn wait_for_process_exit(pid: libc::pid_t) {
        for _ in 0..50 {
            if !process_exists(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        panic!("timed out waiting for process {pid} to exit");
    }
}
