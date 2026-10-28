# ADR 0002: `unsafe` for the macOS open-documents event, in one module of the app

- **Status:** accepted for the `ext-all` fork (2026-10-11); a deviation from ADR 0001 to record, not
  a change of the rule
- **Issue:** Finder / Dock / `open -a FilmCraft x.fcproj` could not open a project

## Context

The macOS bundle now declares its document type (`CFBundleDocumentTypes`, `packaging/macos/Info.plist.in`),
so Launch Services sends the *open documents* Apple Event to the app. AppKit turns it into
`application:openFiles:` on the application delegate. winit 0.30's delegate answers neither
`application:openFile:` nor `application:openFiles:`, and the delegate is created inside
`eframe::run_native`, after which no code of ours runs before the event loop starts; the event sent at
launch arrives before `applicationDidFinishLaunching:`. No safe crate in the dependency tree exposes the
event (`muda` covers menus only; `tao` is another event loop).

ADR 0001 confines `unsafe` to `crates/platform`, for OS media FFI, and says that dialogs, menus and
other OS integration do not go in it.

## Decision

One module of the desktop app, `apps/filmcraft/src/open_files.rs`, carries `#[allow(unsafe_code)]`
for Objective-C runtime calls: an `NSApplication` subclass whose `setDelegate:` adds
`application:openFiles:` to the delegate's class (`class_addMethod`), the method itself (queue the
paths, wake the window, `replyToOpenOrPrint:`), and the `sharedApplication` call that makes `NSApp`
that subclass before the event loop exists. `apps/filmcraft/Cargo.toml` copies the workspace lints
with `unsafe_code = "deny"` and `clippy::undocumented_unsafe_blocks = "deny"`, like `crates/platform`;
every `unsafe` block has a `// SAFETY:` comment. The rest of the app keeps `unsafe` denied.

Containment: the module has no dependency on the engine or the UI beyond `egui::Context` (to request a
repaint); the paths are handed over through a queue (`take_pending`) that the start-up code and the
window poll. Removing the module restores the previous behaviour (the file must be opened through
File ▸ Open Project… or given on the command line).

## Consequences

- A double-click on a `.fcproj`, `open -a FilmCraft`, and a drop on the Dock icon open the project; media
  files dropped on the icon are imported.
- When winit gains the event (an open issue upstream), the module goes away and the lint table of the
  app returns to `workspace = true`.
