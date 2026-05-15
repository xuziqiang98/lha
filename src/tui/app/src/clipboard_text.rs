#[cfg(any(target_os = "macos", target_os = "emscripten", not(unix)))]
pub(crate) fn write_text_to_clipboard(text: &str) -> Result<(), String> {
    write_text_to_clipboard_immediate(text)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
pub(crate) fn write_text_to_clipboard(text: &str) -> Result<(), String> {
    write_text_to_linux_clipboard(text)
}

#[cfg(any(target_os = "macos", target_os = "emscripten", not(unix)))]
fn write_text_to_clipboard_immediate(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|err| err.to_string())?;
    clipboard
        .set_text(text.to_string())
        .map_err(|err| err.to_string())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn write_text_to_linux_clipboard(text: &str) -> Result<(), String> {
    write_text_to_linux_clipboard_with(
        text,
        is_remote_terminal_session(),
        write_text_to_linux_system_clipboard,
        write_text_with_osc52,
    )
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn write_text_to_linux_clipboard_with(
    text: &str,
    is_remote: bool,
    mut write_system_clipboard: impl FnMut(&str) -> Result<(), String>,
    mut write_osc52: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    if is_remote {
        return match write_osc52(text) {
            Ok(()) => Ok(()),
            Err(osc52_err) => match write_system_clipboard(text) {
                Ok(()) => Ok(()),
                Err(system_err) => Err(format!(
                    "OSC52 failed: {osc52_err}; system clipboard failed: {system_err}"
                )),
            },
        };
    }

    match write_system_clipboard(text) {
        Ok(()) => Ok(()),
        Err(system_err) => match write_osc52(text) {
            Ok(()) => Ok(()),
            Err(osc52_err) => Err(format!(
                "system clipboard failed: {system_err}; OSC52 failed: {osc52_err}"
            )),
        },
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn write_text_to_linux_system_clipboard(text: &str) -> Result<(), String> {
    use std::sync::mpsc;
    use std::time::Duration;

    const CLIPBOARD_KEEPALIVE: Duration = Duration::from_secs(2);

    let text = text.to_string();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("adam-clipboard-text".to_string())
        .spawn(move || {
            let result = (|| -> Result<arboard::Clipboard, String> {
                let mut clipboard = arboard::Clipboard::new().map_err(|err| err.to_string())?;
                clipboard.set_text(text).map_err(|err| err.to_string())?;
                Ok(clipboard)
            })();

            match result {
                Ok(clipboard) => {
                    std::thread::sleep(CLIPBOARD_KEEPALIVE);
                    // On X11, dropping the final Clipboard performs the clipboard-manager handoff.
                    drop(clipboard);
                    let _ = tx.send(Ok(()));
                }
                Err(err) => {
                    let _ = tx.send(Err(err));
                }
            }
        })
        .map_err(|err| err.to_string())?;

    rx.recv()
        .map_err(|err| format!("clipboard helper exited before reporting status: {err}"))?
}

#[cfg(not(target_os = "android"))]
const OSC52_MAX_BYTES: usize = 100 * 1024;

#[cfg(not(target_os = "android"))]
fn write_text_with_osc52(text: &str) -> Result<(), String> {
    use std::io;
    use std::io::IsTerminal;
    use std::io::Write;

    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        return Err("stdout is not a terminal".to_string());
    }

    let sequence = osc52_sequence(text, std::env::var_os("TMUX").is_some())?;
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|err| err.to_string())
}

#[cfg(not(target_os = "android"))]
fn osc52_sequence(text: &str, in_tmux: bool) -> Result<String, String> {
    use base64::Engine as _;

    if text.len() > OSC52_MAX_BYTES {
        return Err(format!(
            "selection is too large for OSC52 clipboard copy: {} bytes exceeds {OSC52_MAX_BYTES} bytes",
            text.len()
        ));
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    if in_tmux {
        Ok(format!("\x1bPtmux;\x1b{sequence}\x1b\\"))
    } else {
        Ok(sequence)
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn is_remote_terminal_session() -> bool {
    is_remote_terminal_session_with(|name| std::env::var_os(name).is_some())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn is_remote_terminal_session_with(has_var: impl FnMut(&str) -> bool) -> bool {
    const REMOTE_TERMINAL_ENV_VARS: [&str; 5] = [
        "SSH_CONNECTION",
        "SSH_CLIENT",
        "SSH_TTY",
        "VSCODE_IPC_HOOK_CLI",
        "VSCODE_INJECTION",
    ];

    REMOTE_TERMINAL_ENV_VARS.into_iter().any(has_var)
}

#[cfg(target_os = "android")]
pub(crate) fn write_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("clipboard text copy is unsupported on Android".to_string())
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;

    #[test]
    fn osc52_sequence_encodes_text_clipboard_write() {
        assert_eq!(
            osc52_sequence("hello", false).unwrap(),
            "\x1b]52;c;aGVsbG8=\x07"
        );
    }

    #[test]
    fn osc52_sequence_wraps_for_tmux_passthrough() {
        assert_eq!(
            osc52_sequence("copy", true).unwrap(),
            "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\"
        );
    }

    #[test]
    fn osc52_sequence_rejects_oversized_selection() {
        let text = "x".repeat(OSC52_MAX_BYTES + 1);
        let err = osc52_sequence(&text, false).unwrap_err();

        assert!(err.contains("selection is too large"));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn remote_linux_clipboard_uses_osc52_before_system_clipboard() {
        let calls = RefCell::new(Vec::new());

        let result = write_text_to_linux_clipboard_with(
            "copy",
            true,
            |_| {
                calls.borrow_mut().push("system");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("osc52");
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(calls.into_inner(), vec!["osc52"]);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn remote_linux_clipboard_falls_back_to_system_after_osc52_failure() {
        let calls = RefCell::new(Vec::new());

        let result = write_text_to_linux_clipboard_with(
            "copy",
            true,
            |_| {
                calls.borrow_mut().push("system");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("osc52");
                Err("osc52 unavailable".to_string())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(calls.into_inner(), vec!["osc52", "system"]);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn local_linux_clipboard_uses_system_clipboard_before_osc52() {
        let calls = RefCell::new(Vec::new());

        let result = write_text_to_linux_clipboard_with(
            "copy",
            false,
            |_| {
                calls.borrow_mut().push("system");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("osc52");
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(calls.into_inner(), vec!["system"]);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn local_linux_clipboard_falls_back_to_osc52_after_system_failure() {
        let calls = RefCell::new(Vec::new());

        let result = write_text_to_linux_clipboard_with(
            "copy",
            false,
            |_| {
                calls.borrow_mut().push("system");
                Err("system unavailable".to_string())
            },
            |_| {
                calls.borrow_mut().push("osc52");
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(calls.into_inner(), vec!["system", "osc52"]);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn remote_linux_clipboard_reports_failures_in_attempt_order() {
        let result = write_text_to_linux_clipboard_with(
            "copy",
            true,
            |_| Err("system unavailable".to_string()),
            |_| Err("osc52 unavailable".to_string()),
        );

        assert_eq!(
            result.unwrap_err(),
            "OSC52 failed: osc52 unavailable; system clipboard failed: system unavailable"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn local_linux_clipboard_reports_failures_in_attempt_order() {
        let result = write_text_to_linux_clipboard_with(
            "copy",
            false,
            |_| Err("system unavailable".to_string()),
            |_| Err("osc52 unavailable".to_string()),
        );

        assert_eq!(
            result.unwrap_err(),
            "system clipboard failed: system unavailable; OSC52 failed: osc52 unavailable"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn remote_terminal_session_detects_ssh_connection() {
        assert!(is_remote_terminal_session_with(
            |name| name == "SSH_CONNECTION"
        ));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn remote_terminal_session_detects_vscode_remote() {
        assert!(is_remote_terminal_session_with(
            |name| name == "VSCODE_IPC_HOOK_CLI"
        ));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    #[test]
    fn remote_terminal_session_is_false_without_remote_vars() {
        assert!(!is_remote_terminal_session_with(|_| false));
    }
}
