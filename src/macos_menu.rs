#![cfg(target_os = "macos")]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject as RtNSObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly};
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

static PENDING: AtomicU32 = AtomicU32::new(0);
static CTX: OnceLock<egui::Context> = OnceLock::new();

pub fn take_actions() -> u32 {
    PENDING.swap(0, Ordering::AcqRel)
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

        let handler: Retained<MenuHandler> = msg_send![MenuHandler::class(), new];
        let handler_ref: &AnyObject = &*(handler.as_ref() as *const MenuHandler as *const AnyObject);

        let cmd  = NSEventModifierFlags::Command;
        let opt  = NSEventModifierFlags::Option;
        let none = NSEventModifierFlags(0);

        // ── File ─────────────────────────────────────────────────────────────
        let file_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("File"));
        add_item(&file_menu, "Open…",    "o", cmd,  objc2::sel!(handleOpenFile:),   handler_ref);
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
