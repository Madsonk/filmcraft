//! Host side of Window ▸ Resources and resource drops (assets-dnd).
//!
//! - [`pointer_in_window`]: where the pointer is while files are dragged in from the Finder. winit
//!   0.30 reports the hovered and dropped files but no position on macOS (AppKit sends
//!   `draggingUpdated:` to the window, not mouse moves), so a drop could not tell a shot from an
//!   empty track. AppKit's `mouseLocationOutsideOfEventStream` gives it, in the window's content
//!   view, flipped to egui's top-left origin.
//! - [`preview_audio`]: auditions a sound with the system player (`afplay` on macOS), one at a
//!   time; the panel's ▶ starts it, ▶ again or another sound stops it.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

/// Pointer in logical points of the window content (top-left origin), if it is over one of the
/// app's visible windows.
pub fn pointer_in_window() -> Option<(f32, f32)> {
    #[cfg(target_os = "macos")]
    {
        let mtm = objc2::MainThreadMarker::new()?;
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        for w in app.windows().iter() {
            if !w.isVisible() {
                continue;
            }
            let Some(view) = w.contentView() else { continue };
            let p = view.convertPoint_fromView(w.mouseLocationOutsideOfEventStream(), None);
            let b = view.bounds();
            let y = if view.isFlipped() { p.y } else { b.size.height - p.y };
            if p.x >= 0.0 && y >= 0.0 && p.x <= b.size.width && y <= b.size.height {
                return Some((p.x as f32, y as f32));
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

static PLAYER: Mutex<Option<Child>> = Mutex::new(None);

/// `Some(path)`: stop the sound playing and play `path`; `None`: stop. Returns whether a sound
/// started.
pub fn preview_audio(path: Option<&str>) -> bool {
    let mut cur = PLAYER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut c) = cur.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    let Some(path) = path else { return false };
    let player = if cfg!(target_os = "macos") { "/usr/bin/afplay" } else { "ffplay" };
    let mut cmd = Command::new(player);
    if !cfg!(target_os = "macos") {
        cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]);
    }
    match cmd.arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(c) => {
            *cur = Some(c);
            true
        }
        Err(e) => {
            log::warn!("preview_audio: {player} {path}: {e}");
            false
        }
    }
}
