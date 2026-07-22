use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

pub(crate) struct TuiStderrGuard {
    platform: PlatformStderrGuard,
}

impl TuiStderrGuard {
    pub(crate) fn redirect_to(path: &Path) -> io::Result<Self> {
        let log_file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut guard = Self {
            platform: PlatformStderrGuard::new(log_file)?,
        };
        guard.resume()?;
        Ok(guard)
    }

    pub(crate) fn suspend(&mut self) -> io::Result<()> {
        self.platform.suspend()
    }

    pub(crate) fn resume(&mut self) -> io::Result<()> {
        self.platform.resume()
    }
}

impl std::fmt::Debug for TuiStderrGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiStderrGuard")
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
struct PlatformStderrGuard {
    original: std::os::fd::OwnedFd,
    log_file: File,
    redirected: bool,
}

#[cfg(unix)]
impl PlatformStderrGuard {
    fn new(log_file: File) -> io::Result<Self> {
        use std::os::fd::FromRawFd;

        let original = unsafe { libc::dup(libc::STDERR_FILENO) };
        if original < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            original: unsafe { std::os::fd::OwnedFd::from_raw_fd(original) },
            log_file,
            redirected: false,
        })
    }

    fn suspend(&mut self) -> io::Result<()> {
        use std::os::fd::AsRawFd;

        if self.redirected {
            dup2(self.original.as_raw_fd(), libc::STDERR_FILENO)?;
            self.redirected = false;
        }
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        use std::os::fd::AsRawFd;

        if !self.redirected {
            dup2(self.log_file.as_raw_fd(), libc::STDERR_FILENO)?;
            self.redirected = true;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for PlatformStderrGuard {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        if self.redirected {
            let _ = dup2(self.original.as_raw_fd(), libc::STDERR_FILENO);
        }
    }
}

#[cfg(unix)]
fn dup2(source: libc::c_int, target: libc::c_int) -> io::Result<()> {
    if unsafe { libc::dup2(source, target) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
struct PlatformStderrGuard {
    original: std::os::windows::io::OwnedHandle,
    log_file: File,
    redirected: bool,
}

#[cfg(windows)]
impl PlatformStderrGuard {
    fn new(log_file: File) -> io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS;
        use windows_sys::Win32::Foundation::DuplicateHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::GetStdHandle;
        use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if stderr == 0 || stderr == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let process = unsafe { GetCurrentProcess() };
        let mut original: HANDLE = 0;
        if unsafe {
            DuplicateHandle(
                process,
                stderr,
                process,
                &mut original,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            original: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(original as _) },
            log_file,
            redirected: false,
        })
    }

    fn suspend(&mut self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;

        if self.redirected {
            set_stderr_handle(self.original.as_raw_handle())?;
            self.redirected = false;
        }
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;

        if !self.redirected {
            set_stderr_handle(self.log_file.as_raw_handle())?;
            self.redirected = true;
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for PlatformStderrGuard {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;

        if self.redirected {
            let _ = set_stderr_handle(self.original.as_raw_handle());
        }
    }
}

#[cfg(windows)]
fn set_stderr_handle(handle: std::os::windows::io::RawHandle) -> io::Result<()> {
    use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
    use windows_sys::Win32::System::Console::SetStdHandle;

    if unsafe { SetStdHandle(STD_ERROR_HANDLE, handle as _) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
struct PlatformStderrGuard;

#[cfg(not(any(unix, windows)))]
impl PlatformStderrGuard {
    fn new(_log_file: File) -> io::Result<Self> {
        Ok(Self)
    }

    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::Write;
    use std::process::Command;

    const CHILD_LOG_PATH: &str = "LHA_STDERR_GUARD_TEST_PATH";

    fn write_stderr(message: &str) {
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(message.as_bytes()).expect("write stderr");
        stderr.flush().expect("flush stderr");
    }

    #[test]
    fn stderr_guard_child() {
        let Some(path) = std::env::var_os(CHILD_LOG_PATH) else {
            return;
        };

        write_stderr("before-guard\n");
        let mut guard =
            TuiStderrGuard::redirect_to(Path::new(&path)).expect("redirect child stderr");
        write_stderr("redirected\n");
        guard.suspend().expect("suspend stderr redirect");
        write_stderr("suspended\n");
        guard.resume().expect("resume stderr redirect");
        write_stderr("resumed\n");
        drop(guard);
        write_stderr("after-drop\n");
    }

    #[test]
    fn stderr_guard_redirects_restores_and_suspends() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("lha-tui.log");
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .arg("stderr_guard_child")
            .arg("--nocapture")
            .env(CHILD_LOG_PATH, &log_path)
            .output()
            .expect("run stderr guard child");

        assert_eq!(output.status.success(), true, "{output:?}");
        let child_stderr = String::from_utf8_lossy(&output.stderr);
        assert!(child_stderr.contains("before-guard"), "{child_stderr:?}");
        assert!(child_stderr.contains("suspended"), "{child_stderr:?}");
        assert!(child_stderr.contains("after-drop"), "{child_stderr:?}");
        assert!(!child_stderr.contains("redirected"), "{child_stderr:?}");
        assert!(!child_stderr.contains("resumed"), "{child_stderr:?}");

        let log = std::fs::read_to_string(log_path).expect("read redirected stderr");
        assert!(log.contains("redirected"), "{log:?}");
        assert!(log.contains("resumed"), "{log:?}");
        assert!(!log.contains("suspended"), "{log:?}");
    }
}
