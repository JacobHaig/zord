//! Cross-platform "reveal in file manager" and "open in editor" helpers.
//!
//! These shell out to the OS's native tools and intentionally ignore the
//! result — the worst case is nothing opens, which is non-fatal for the UI.

use std::path::Path;
use std::process::Command;

/// Reveal `path` in the OS file manager, selecting the file if possible.
pub fn reveal_in_file_manager(path: &str) {
    let p = Path::new(path);

    #[cfg(target_os = "macos")]
    {
        // `-R` reveals (selects) the file in Finder.
        let _ = Command::new("open").arg("-R").arg(p).spawn();
    }

    #[cfg(target_os = "windows")]
    {
        // explorer returns a non-zero exit code even on success, so we ignore it.
        let _ = Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No portable "select file" on Linux; open the containing folder.
        let dir = p.parent().unwrap_or(p);
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Open a folder in the OS file manager.
pub fn open_folder(path: &str) {
    let p = Path::new(path);
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(p).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(p).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(p).spawn();
    }
}

/// Open a URL in the default web browser (used for the manual model-download
/// fallback — the browser honors the system proxy where our fetch may not).
pub fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd builtin; the empty "" is the window title arg.
        let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// Copy `text` to the system clipboard. Best-effort.
pub fn copy_to_clipboard(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

/// Open `path` in the OS default text editor. Exports are plain text
/// (Markdown / SRT / JSON), so we bias toward an editor over a viewer.
pub fn open_in_editor(path: &str) {
    let p = Path::new(path);

    #[cfg(target_os = "macos")]
    {
        // `-t` opens in the default text editor regardless of file extension.
        let _ = Command::new("open").arg("-t").arg(p).spawn();
    }

    #[cfg(target_os = "windows")]
    {
        // Notepad is the OS default text editor on Windows.
        let _ = Command::new("notepad").arg(p).spawn();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Use the default handler (text/plain usually maps to an editor).
        let _ = Command::new("xdg-open").arg(p).spawn();
    }
}
