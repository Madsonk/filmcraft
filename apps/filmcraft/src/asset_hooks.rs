//! Host side of Window ▸ Resources and resource drops (assets-dnd).
//!
//! - [`pointer_in_window`]: where the pointer is while files are dragged in from the Finder. winit
//!   0.30 reports the hovered and dropped files but no position on macOS (AppKit sends
//!   `draggingUpdated:` to the window, not mouse moves), so a drop could not tell a shot from an
//!   empty track. AppKit's `mouseLocationOutsideOfEventStream` gives it, in the window's content
//!   view, flipped to egui's top-left origin.
//!
//! (The audition of sounds used to be `afplay` here; it now plays inside the app through the
//! output device, `ui-egui` `assets::audition`, ressources-v2.)

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
