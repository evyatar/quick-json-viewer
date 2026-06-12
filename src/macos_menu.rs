#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject as RtNSObject, Sel};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, ffi};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

// ─── action flags ────────────────────────────────────────────────────────────

pub const ACT_OPEN_FILE:    u32 = 1 << 0;
pub const ACT_SETTINGS:     u32 = 1 << 1;
pub const ACT_FOCUS_SEARCH: u32 = 1 << 2;
pub const ACT_COLLAPSE_ALL: u32 = 1 << 3;
pub const ACT_EXPAND_ALL:   u32 = 1 << 4;
pub const ACT_HELP:         u32 = 1 << 5;
pub const ACT_ABOUT:        u32 = 1 << 6;
pub const ACT_PASTE:        u32 = 1 << 7;

static PENDING: AtomicU32 = AtomicU32::new(0);
static CTX: OnceLock<egui::Context> = OnceLock::new();
static PENDING_OPEN_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn take_actions() -> u32 {
    PENDING.swap(0, Ordering::AcqRel)
}

pub fn take_open_file() -> Option<PathBuf> {
    PENDING_OPEN_FILE.lock().ok()?.take()
}

/// Add `application:openFile:` to the NSApp delegate's class so that macOS
/// delivers Finder file-opens to the app (winit 0.30 doesn't implement it).
/// Safe to call repeatedly — `class_addMethod` is a no-op once the method exists.
unsafe fn inject_open_file_method() {
    extern "C" {
        static NSApp: Option<&'static NSApplication>;
    }
    let Some(app) = NSApp else { return };
    let delegate_ptr: *mut AnyObject = msg_send![app, delegate];
    if delegate_ptr.is_null() {
        return;
    }
    let cls = (*delegate_ptr).class() as *const AnyClass as *mut AnyClass;
    let sel = objc2::sel!(application:openFile:);
    let imp: Imp = std::mem::transmute(
        open_file_imp
            as unsafe extern "C-unwind" fn(
                *mut AnyObject,
                Sel,
                *mut AnyObject,
                *mut AnyObject,
            ) -> bool,
    );
    // Encoding B@:@@ — BOOL return, self, _cmd, NSApplication*, NSString*
    ffi::class_addMethod(cls, sel, imp, b"B@:@@\0".as_ptr().cast());
}

unsafe extern "C-unwind" fn open_file_imp(
    _this: *mut AnyObject,
    _sel: Sel,
    _app: *mut AnyObject,
    filename: *mut AnyObject,
) -> bool {
    if filename.is_null() {
        return false;
    }
    let ns_str = &*(filename as *const NSString);
    let path = PathBuf::from(ns_str.to_string());
    if let Ok(mut lock) = PENDING_OPEN_FILE.lock() {
        *lock = Some(path);
    }
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
    true
}

// Observes NSApplicationWillFinishLaunching: at that point winit has already
// set its delegate on NSApp, but macOS has not yet dispatched the initial
// open-document Apple Event (which arrives before didFinishLaunching).
// That is the only reliable window for injecting application:openFile:.
define_class!(
    #[unsafe(super(RtNSObject))]
    struct LaunchObserver;

    impl LaunchObserver {
        #[unsafe(method(appWillFinishLaunching:))]
        fn app_will_finish_launching(&self, _note: &AnyObject) {
            unsafe { inject_open_file_method() };
        }
    }
);

/// Call from `main()` before `eframe::run_native()`.
pub fn register_open_file_handler() {
    unsafe {
        let nc_cls = ffi::objc_getClass(b"NSNotificationCenter\0".as_ptr().cast());
        if nc_cls.is_null() {
            return;
        }
        let center: *mut AnyObject = msg_send![&*nc_cls, defaultCenter];
        if center.is_null() {
            return;
        }
        let observer: Retained<LaunchObserver> = msg_send![LaunchObserver::class(), new];
        let observer_ptr = observer.as_ref() as *const LaunchObserver as *const AnyObject;
        let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
        let _: () = msg_send![
            center,
            addObserver: observer_ptr,
            selector: objc2::sel!(appWillFinishLaunching:),
            name: &*name,
            object: std::ptr::null::<AnyObject>()
        ];
        // NSNotificationCenter holds the observer unretained; keep it alive.
        std::mem::forget(observer);
    }
}

// ─── ObjC action handler ─────────────────────────────────────────────────────

define_class!(
    #[unsafe(super(RtNSObject))]
    struct MenuHandler;

    impl MenuHandler {
        #[unsafe(method(handleOpenFile:))]
        fn handle_open_file(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_OPEN_FILE, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handlePaste:))]
        fn handle_paste(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_PASTE, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handleSettings:))]
        fn handle_settings(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_SETTINGS, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handleFocusSearch:))]
        fn handle_focus_search(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_FOCUS_SEARCH, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handleCollapseAll:))]
        fn handle_collapse_all(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_COLLAPSE_ALL, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handleExpandAll:))]
        fn handle_expand_all(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_EXPAND_ALL, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handleHelp:))]
        fn handle_help(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_HELP, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
        #[unsafe(method(handleAbout:))]
        fn handle_about(&self, _sender: &AnyObject) {
            PENDING.fetch_or(ACT_ABOUT, Ordering::Relaxed);
            if let Some(c) = CTX.get() { c.request_repaint(); }
        }
    }
);

// ─── menu builder ────────────────────────────────────────────────────────────

unsafe fn add_item(
    menu: &NSMenu,
    title: &str,
    key: &str,
    mods: NSEventModifierFlags,
    sel: objc2::runtime::Sel,
    target: &AnyObject,
) {
    let item = menu.addItemWithTitle_action_keyEquivalent(
        &NSString::from_str(title),
        Some(sel),
        &NSString::from_str(key),
    );
    item.setKeyEquivalentModifierMask(mods);
    item.setTarget(Some(target));
}

// ─── public entry point ──────────────────────────────────────────────────────

pub fn install(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());

    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    unsafe {
        extern "C" {
            static NSApp: Option<&'static NSApplication>;
        }
        let Some(app) = NSApp else { return };

        // Fallback in case the will-finish-launching notification was missed.
        inject_open_file_method();

        let handler: Retained<MenuHandler> = msg_send![MenuHandler::class(), new];
        let handler_ref: &AnyObject = &*(handler.as_ref() as *const MenuHandler as *const AnyObject);

        let cmd  = NSEventModifierFlags::Command;
        let opt  = NSEventModifierFlags::Option;
        let none = NSEventModifierFlags(0);

        // ── File ─────────────────────────────────────────────────────────────
        let file_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("File"));
        add_item(&file_menu, "Open…",    "o", cmd,  objc2::sel!(handleOpenFile:),   handler_ref);
        // ⇧⌘V — a plain ⌘V key equivalent here would be swallowed by the menu
        // and never reach the search box; bare ⌘V is handled in the egui layer.
        add_item(&file_menu, "Paste JSON / JWT", "v", cmd | NSEventModifierFlags::Shift,
                 objc2::sel!(handlePaste:), handler_ref);
        file_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add_item(&file_menu, "Settings", ",", cmd,  objc2::sel!(handleSettings:),   handler_ref);

        // ── View ─────────────────────────────────────────────────────────────
        let view_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("View"));
        add_item(&view_menu, "Collapse All", "c", opt, objc2::sel!(handleCollapseAll:),  handler_ref);
        add_item(&view_menu, "Expand All",   "x", opt, objc2::sel!(handleExpandAll:),    handler_ref);
        view_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add_item(&view_menu, "Search",       "f", cmd, objc2::sel!(handleFocusSearch:),  handler_ref);

        // ── Help ─────────────────────────────────────────────────────────────
        let help_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Help"));
        add_item(&help_menu, "Keyboard Shortcuts", "", none, objc2::sel!(handleHelp:),  handler_ref);
        help_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add_item(&help_menu, "About JSON Viewer",  "", none, objc2::sel!(handleAbout:), handler_ref);

        // ── Attach to the main menu, preserving the app (first) item ─────────
        if let Some(main_menu) = app.mainMenu() {
            while main_menu.numberOfItems() > 1 {
                main_menu.removeItemAtIndex(1);
            }
            for (label, submenu) in [
                ("File", &*file_menu),
                ("View", &*view_menu),
                ("Help", &*help_menu),
            ] {
                let top = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(label),
                    None,
                    &NSString::from_str(""),
                );
                top.setSubmenu(Some(submenu));
                main_menu.addItem(&top);
            }
        }

        // Leak the handler — NSMenuItem keeps only a weak (unretained) target
        // reference, so we must keep this object alive for the app's lifetime.
        std::mem::forget(handler);
    }
}
