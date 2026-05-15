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
    use arboard::SetExtLinux;
    use std::time::Duration;
    use std::time::Instant;

    const CLIPBOARD_KEEPALIVE: Duration = Duration::from_secs(2);

    let text = text.to_string();
    std::thread::Builder::new()
        .name("adam-clipboard-text".to_string())
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let mut clipboard = arboard::Clipboard::new().map_err(|err| err.to_string())?;
                clipboard
                    .set()
                    .wait_until(Instant::now() + CLIPBOARD_KEEPALIVE)
                    .text(text)
                    .map_err(|err| err.to_string())
            })();

            if let Err(err) = result {
                tracing::warn!("failed to keep Linux clipboard text alive: {err}");
            }
        })
        .map_err(|err| err.to_string())?;

    Ok(())
}

#[cfg(target_os = "android")]
pub(crate) fn write_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("clipboard text copy is unsupported on Android".to_string())
}
