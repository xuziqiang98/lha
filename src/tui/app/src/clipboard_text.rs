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
                    let _ = tx.send(Ok(()));
                    std::thread::sleep(CLIPBOARD_KEEPALIVE);
                    drop(clipboard);
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

#[cfg(target_os = "android")]
pub(crate) fn write_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("clipboard text copy is unsupported on Android".to_string())
}
