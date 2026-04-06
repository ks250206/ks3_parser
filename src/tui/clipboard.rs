/// OS クリップボード（Windows / macOS / X11 / Wayland は arboard に委譲）
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}
