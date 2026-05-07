#[cfg(not(target_os = "android"))]
pub(crate) fn write_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|err| err.to_string())?;
    clipboard
        .set_text(text.to_string())
        .map_err(|err| err.to_string())
}

#[cfg(target_os = "android")]
pub(crate) fn write_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("clipboard text copy is unsupported on Android".to_string())
}
