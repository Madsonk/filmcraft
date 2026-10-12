//! macOS: files the system asks the app to open — a double-click on a `.fcproj`, `open -a FilmCraft
//! x.fcproj`, a drop on the Dock icon — arrive as the *open documents* Apple Event, which AppKit turns
//! into `application:openFiles:` on the application delegate. The bundle lists the project type in
//! `CFBundleDocumentTypes` (packaging/macos/Info.plist.in); without it Launch Services refuses the file.
//!
//! winit's application delegate (`WinitApplicationDelegate`) answers neither `application:openFile:`
//! nor `application:openFiles:`, and it is created inside `eframe::run_native`, after which no code of
//! ours runs before the event loop starts. The event sent at launch is delivered before
//! `applicationDidFinishLaunching:`, so the method has to be there before the delegate is set. This
//! module makes `NSApp` an instance of a subclass of `NSApplication` ([`install`], called before
//! `run_native`) whose `setDelegate:` adds `application:openFiles:` to the delegate's class before
//! handing the delegate to AppKit. The paths are queued in [`take_pending`]: the start-up code reads
//! them like command-line files, and the window polls them every frame (`HostHooks::take_opened_files`).
#![cfg(target_os = "macos")]
// The one module of the app with `unsafe`: Objective-C runtime calls (docs/adr/0002-macos-open-documents.md).
#![allow(unsafe_code)]

use std::ffi::c_char;
use std::sync::{Mutex, OnceLock, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Imp, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationDelegateReply};
use objc2_foundation::{NSArray, NSString};

/// Paths received and not yet taken.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The window's context, to redraw (and so poll) when files arrive while the app idles.
static WAKE: OnceLock<egui::Context> = OnceLock::new();

define_class!(
    /// `NSApp`: an `NSApplication` whose delegate learns `application:openFiles:`.
    #[unsafe(super(NSApplication))]
    #[thread_kind = MainThreadOnly]
    #[name = "FilmCraftApplication"]
    struct FilmCraftApplication;

    impl FilmCraftApplication {
        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&ProtocolObject<dyn NSApplicationDelegate>>) {
            if let Some(d) = delegate {
                add_open_files(d);
            }
            // SAFETY: the superclass method with the arguments it declares (`setDelegate:` takes a nullable delegate).
            let _: () = unsafe { msg_send![super(self), setDelegate: delegate] };
        }
    }
);

/// `application:openFiles:` — queue the paths, wake the window and tell AppKit the open succeeded (it
/// would otherwise show "The document could not be opened").
unsafe extern "C-unwind" fn open_files(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, files: *mut NSArray<NSString>) {
    let mut paths = Vec::new();
    if !files.is_null() {
        // SAFETY: AppKit passes a live NSArray<NSString> for the duration of the call.
        let files = unsafe { &*files };
        for i in 0..files.len() {
            paths.push(files.objectAtIndex(i).to_string());
        }
    }
    log::info!("open documents event: {paths:?}");
    PENDING.lock().unwrap_or_else(PoisonError::into_inner).extend(paths);
    if let Some(ctx) = WAKE.get() {
        ctx.request_repaint();
    }
    if let Some(mtm) = MainThreadMarker::new() {
        // on the main thread, inside AppKit's delivery of the open event (the reply it waits for)
        NSApplication::sharedApplication(mtm).replyToOpenOrPrint(NSApplicationDelegateReply::Success);
    }
}

/// Add `application:openFiles:` to the class of `delegate` unless it already answers it.
fn add_open_files(delegate: &ProtocolObject<dyn NSApplicationDelegate>) {
    let selector = sel!(application:openFiles:);
    if delegate.respondsToSelector(selector) {
        return;
    }
    let object: *const AnyObject = (delegate as *const ProtocolObject<dyn NSApplicationDelegate>).cast();
    // SAFETY: plain runtime calls on a live object and its class; the IMP has the signature `v@:@@` declares
    // (void return, self, _cmd, the application, the array of file names).
    let added = unsafe {
        let class = objc2::ffi::object_getClass(object);
        let imp: Imp = std::mem::transmute(open_files as unsafe extern "C-unwind" fn(_, _, _, _));
        objc2::ffi::class_addMethod(class.cast_mut(), selector, imp, c"v@:@@".as_ptr().cast::<c_char>())
    };
    if !added.as_bool() {
        log::warn!("could not add application:openFiles: to the application delegate; Finder opens are off");
    }
}

/// Make `NSApp` our subclass. Must run before the event loop is created (`eframe::run_native`).
pub fn install() {
    let Some(_mtm) = MainThreadMarker::new() else { return };
    // SAFETY: `sharedApplication` on the subclass creates NSApp as an instance of it (what a
    // bundle's NSPrincipalClass does); winit's later `sharedApplication` returns this one.
    let _app: Retained<FilmCraftApplication> = unsafe { msg_send![FilmCraftApplication::class(), sharedApplication] };
}

/// Redraw the window when files arrive.
pub fn set_waker(ctx: egui::Context) {
    let _ = WAKE.set(ctx);
}

/// The paths received since the last call.
pub fn take_pending() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(PoisonError::into_inner))
}
