mod ai;
mod codegen;
mod diff;
mod export;
mod index;
mod loader;
#[cfg(target_os = "macos")]
mod macos_menu;
mod parser;
mod paste;
mod search;
mod settings;
mod theme;
mod tree;
mod update;
mod url_parse;

use std::path::PathBuf;
use std::sync::Arc;

// ─── BiDi / RTL helpers ───────────────────────────────────────────────────────

/// Reorder a logical-order string to visual (display) order using the Unicode
/// Bidirectional Algorithm.  For purely LTR text the string is returned
/// unchanged.  For text that contains RTL runs (e.g. Hebrew, Arabic) the
/// characters are reordered so that egui's left-to-right glyph painter
/// displays them in the correct visual sequence.
fn bidi_reorder(s: &str) -> std::borrow::Cow<'_, str> {
    use unicode_bidi::BidiInfo;

    // Fast path: skip the allocation when there are no RTL characters at all.
    if !contains_rtl(s) {
        return std::borrow::Cow::Borrowed(s);
    }

    let bidi = BidiInfo::new(s, None);
    if bidi.paragraphs.is_empty() {
        return std::borrow::Cow::Borrowed(s);
    }

    // We treat the whole string as a single paragraph.
    let para = &bidi.paragraphs[0];
    let line = 0..s.len();
    std::borrow::Cow::Owned(bidi.reorder_line(para, line).into_owned())
}

/// True if the string contains any strongly right-to-left character
/// (Hebrew, Arabic, or an explicit RTL control).
fn contains_rtl(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            unicode_bidi::bidi_class(c),
            unicode_bidi::BidiClass::R
                | unicode_bidi::BidiClass::AL
                | unicode_bidi::BidiClass::RLE
                | unicode_bidi::BidiClass::RLO
                | unicode_bidi::BidiClass::RLI
        )
    })
}

use loader::LoadMsg;
use settings::{Settings, show_settings_window};
use tree::TreeState;

// ─── row actions ─────────────────────────────────────────────────────────────

enum RowAction {
    Select(u32),
    Toggle(u32),
    ToggleCheck(u32),
    ExpandRecursive(u32),
    CollapseRecursive(u32),
    Export(ExportScope, ExportFormat),
    StartEditValue(u32),
    StartEditKey(u32),
    DeleteNode(u32),
    AddItem(u32),
}

/// What an export operates on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportScope {
    /// The whole document.
    File,
    /// A single node's subtree.
    Node(u32),
    /// The checked multi-selection (pruned common-ancestor subtree).
    Selection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Json,
    Csv,
}

/// Completion message from a background export/save write.
enum BgWriteDone {
    /// Plain export or save-a-copy — nothing to update on success.
    Written,
    /// Overwrite-save finished; apply the post-save state transition.
    SaveOverwrite {
        path:       PathBuf,
        json_len:   u64,
        structural: bool,
        /// Overlay as it was at save time — becomes the saved baseline.
        snapshot:   std::collections::HashMap<u32, export::NodeEdit>,
    },
}

impl ExportFormat {
    fn ext(self) -> &'static str {
        match self {
            ExportFormat::Json => "json",
            ExportFormat::Csv => "csv",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditField { Key, Value }

/// Which save operation a UI control is requesting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveAction { Overwrite, Copy }

struct EditingState {
    node_idx:        u32,
    field:           EditField,
    text:            String,
    focus_requested: bool, // auto-focus TextEdit on first render
}

/// State for the "Add Item" / "Add Property" dialog: the target container,
/// the key typed so far (only used when the target is an Object), and the
/// raw JSON value text typed so far.
struct AddingState {
    parent:          u32,
    key:             String,
    text:            String,
    focus_requested: bool,
}

/// One undoable change to `edit_overlay`: the entry's state for `node_idx`
/// before and after the edit (`None` = no overlay entry).
#[derive(Clone)]
struct UndoEntry {
    node_idx: u32,
    before:   Option<export::NodeEdit>,
    after:    Option<export::NodeEdit>,
}

/// One entry on the undo/redo stack: either an `edit_overlay` change, the
/// addition of a pending array item / object property (see `TreeState::add_item`),
/// or a batch of overlay changes applied together (an AI changeset) that
/// undoes/redoes as one unit.
#[derive(Clone)]
enum UndoAction {
    Overlay(UndoEntry),
    Add { parent: u32, key: Option<String>, raw_value: String },
    Batch(Vec<UndoEntry>),
}

/// Actions produced by a diff row, applied after the scroll-area borrow ends.
enum DiffRowAction {
    Select(u32),
    Toggle(u32),
}

// ─── app state ───────────────────────────────────────────────────────────────

/// Top-level view: explore one document, or compare two.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Viewer,
    Compare,
}

/// Which pane of the Compare view is the target for Open / Paste / drop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Clone)]
struct FileInfo {
    name:       String,
    size_bytes: u64,
    /// Source path on disk, when the document was opened from a file. `None`
    /// for pasted content — such documents can only be saved as a copy.
    path:       Option<PathBuf>,
}

/// One side of the Compare view — an independently-loaded document.
#[derive(Default)]
struct ComparePane {
    index:          Option<Arc<index::JsonIndex>>,
    load_rx:        Option<std::sync::mpsc::Receiver<LoadMsg>>,
    load_progress:  f32,
    load_error:     Option<String>,
    load_error_ctx: Option<loader::ErrorContext>,
    error_ctx_open: bool,
    file_info:      Option<FileInfo>,
}

/// State for the Compare view: the two panes, the diff options + their raw UI
/// text buffers, and the computed diff (result + view state).
#[derive(Default)]
struct CompareState {
    left:               ComparePane,
    right:              ComparePane,
    options:            diff::DiffOptions,
    ignore_keys_raw:    String,
    ignore_pattern_raw: String,
    pattern_error:      bool,
    result:             Option<diff::DiffResult>,
    tree:               Option<diff::DiffTreeState>,
    /// Set while a diff is being computed on a background thread. The UI shows
    /// a spinner instead of freezing; the result is collected in `poll_diff`.
    diff_rx:            Option<std::sync::mpsc::Receiver<diff::DiffResult>>,
    active_pane:        Side,
    needs_rediff:       bool,
    show_only_diffs:    bool,
    /// Which diff-status types are shown; toggled by clicking the counters.
    filter:             diff::StatusFilter,
}

impl Default for Side {
    fn default() -> Self { Side::Left }
}

impl Side {
    fn other(self) -> Self {
        match self { Side::Left => Side::Right, Side::Right => Side::Left }
    }
}

impl CompareState {
    fn pane(&self, side: Side) -> &ComparePane {
        match side { Side::Left => &self.left, Side::Right => &self.right }
    }
    fn pane_mut(&mut self, side: Side) -> &mut ComparePane {
        match side { Side::Left => &mut self.left, Side::Right => &mut self.right }
    }
}

struct App {
    load_rx:        Option<std::sync::mpsc::Receiver<LoadMsg>>,
    load_progress:  f32,
    load_error:     Option<String>,
    load_error_ctx: Option<loader::ErrorContext>,
    error_ctx_open: bool,
    tree:           Option<TreeState>,
    search_input:   String,
    search_pending: Option<std::thread::JoinHandle<Option<Vec<u32>>>>,
    /// Raised to abort the in-flight search thread (results would be stale).
    search_cancel:  Arc<std::sync::atomic::AtomicBool>,
    /// Debounce deadline (egui time) — typing reschedules; the search fires
    /// only after the input has been quiet briefly, instead of one full
    /// index scan per keystroke.
    search_debounce_until: Option<f64>,
    file_info:      Option<FileInfo>,
    focus_search:   bool,
    paste_pending:  bool,
    settings:       Settings,
    settings_open:  bool,
    help_open:      bool,
    search_help_open: bool,
    about_open:     bool,
    url_dialog_open:  bool,
    url_dialog_input: String,
    url_dialog_focus: bool,
    type_ahead:     String,
    type_ahead_time: f64,
    mode:           AppMode,
    compare:        CompareState,
    update_rx:            Option<std::sync::mpsc::Receiver<update::UpdateMsg>>,
    update_available:     Option<update::ReleaseInfo>,
    update_check_started: bool,
    editing_node:  Option<EditingState>,
    adding_item:   Option<AddingState>,
    edit_overlay:  std::collections::HashMap<u32, export::NodeEdit>,
    /// Snapshot of `edit_overlay` at the last overwrite-save; edits matching it
    /// are considered persisted (no dirty marker). Empty = nothing saved yet.
    saved_overlay: std::collections::HashMap<u32, export::NodeEdit>,
    undo_stack:    Vec<UndoAction>,
    redo_stack:    Vec<UndoAction>,
    /// In-flight background export/save; result polled each frame so large
    /// documents serialize + write without freezing the UI.
    bg_write_rx:   Option<std::sync::mpsc::Receiver<Result<BgWriteDone, String>>>,
    /// (theme, family, size, prefer_dark) last installed into the egui ctx.
    style_applied: Option<(settings::Theme, settings::FontFamily, f32, bool)>,
    /// Cached widths of the fixed search-bar chrome, keyed by (font size,
    /// family): (key, sum of the 5 button glyph widths, magnifier width).
    search_chrome_cache: Option<((f32, settings::FontFamily), f32, f32)>,
    install_watcher_rx:   Option<std::sync::mpsc::Receiver<update::UpdateMsg>>,
    /// BYOK AI assistant panel (chat state, in-flight turn, pending changeset).
    ai:             ai::panel::AiPanelState,
    /// Transient UI state for the settings window's AI section (key buffer).
    ai_settings_ui: ai::panel::AiSettingsUi,
    #[cfg(target_os = "macos")]
    menu_installed: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            load_rx:        None,
            load_progress:  0.0,
            load_error:     None,
            load_error_ctx: None,
            error_ctx_open: false,
            tree:           None,
            search_input:   String::new(),
            search_pending: None,
            search_cancel:  Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_debounce_until: None,
            file_info:      None,
            focus_search:    false,
            paste_pending:   false,
            settings:        Settings::default(),
            settings_open:   false,
            help_open:       false,
            search_help_open: false,
            about_open:      false,
            url_dialog_open:  false,
            url_dialog_input: String::new(),
            url_dialog_focus: false,
            type_ahead:      String::new(),
            type_ahead_time: 0.0,
            mode:            AppMode::Viewer,
            compare:         CompareState::default(),
            update_rx:            None,
            update_available:     None,
            update_check_started: false,
            editing_node:  None,
            adding_item:   None,
            edit_overlay:  std::collections::HashMap::new(),
            saved_overlay: std::collections::HashMap::new(),
            undo_stack:    Vec::new(),
            redo_stack:    Vec::new(),
            bg_write_rx:   None,
            style_applied: None,
            search_chrome_cache: None,
            install_watcher_rx:   None,
            ai:             ai::panel::AiPanelState::default(),
            ai_settings_ui: ai::panel::AiSettingsUi::default(),
            #[cfg(target_os = "macos")]
            menu_installed: false,
        }
    }
}

// ─── eframe entry point ──────────────────────────────────────────────────────

/// Renders the bytes surrounding a parse error in a code-block-style
/// container, preserving line breaks and highlighting the errored byte.
fn error_context_ui(ui: &mut egui::Ui, error: Option<&str>, before: &str, at: &str, after: &str) {
    ui.add_space(6.0);
    ui.label("Source surrounding the error (highlighting where parsing failed):");
    ui.add_space(8.0);
    if let Some(error) = error {
        ui.label(
            egui::RichText::new(error)
                .color(ui.visuals().error_fg_color)
                .strong(),
        );
        ui.add_space(8.0);
    }

    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    // A highlighted bare newline is invisible; show a visible marker before it.
    let at_display = if at == "\n" { "↵\n".to_string() } else { at.to_string() };

    let mut job = egui::text::LayoutJob::default();
    job.append(before, 0.0, egui::TextFormat {
        font_id: mono.clone(),
        color: text_color,
        ..Default::default()
    });
    job.append(&at_display, 0.0, egui::TextFormat {
        font_id: mono.clone(),
        color: egui::Color32::WHITE,
        background: egui::Color32::from_rgb(200, 50, 50),
        ..Default::default()
    });
    job.append(after, 0.0, egui::TextFormat {
        font_id: mono,
        color: text_color,
        ..Default::default()
    });

    egui::Frame::group(ui.style())
        .fill(ui.visuals().code_bg_color)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .max_width(640.0)
                .max_height(320.0)
                .show(ui, |ui| {
                    ui.label(job);
                });
        });
    ui.add_space(6.0);
}

fn setup_unicode_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Apple Symbols covers keyboard glyphs (⌥ ⌘ ⇧ …) missing from most fonts.
    if let Ok(data) = std::fs::read("/System/Library/Fonts/Apple Symbols.ttf") {
        fonts.font_data.insert(
            "apple_symbols".to_owned(),
            egui::FontData::from_owned(data).into(),
        );
        for list in fonts.families.values_mut() {
            list.push("apple_symbols".to_owned());
        }
    }

    // Hebrew / broad Unicode fallback.
    let hebrew_candidates = [
        "/System/Library/Fonts/Supplemental/Arial Unicode MS.ttf",
        "/Library/Fonts/Arial Unicode MS.ttf",
        "/System/Library/Fonts/ArialHB.ttc",
        "/System/Library/Fonts/Supplemental/Arial Hebrew.ttf",
    ];
    for path in &hebrew_candidates {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                "unicode_fallback".to_owned(),
                egui::FontData::from_owned(data).into(),
            );
            for list in fonts.families.values_mut() {
                list.push("unicode_fallback".to_owned());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}

fn app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("icon.png");
    let img   = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let img   = img.into_rgba8();
    Some(egui::IconData {
        rgba:   img.as_raw().clone(),
        width:  img.width(),
        height: img.height(),
    })
}

fn main() -> eframe::Result<()> {
    // Arrange for application:openFile: to be injected into the app delegate at
    // will-finish-launching time — after winit sets its delegate, before macOS
    // dispatches the initial open-document Apple Event from Finder.
    #[cfg(target_os = "macos")]
    macos_menu::register_open_file_handler();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Quick JSON Viewer")
        .with_inner_size([1200.0, 800.0])
        .with_min_inner_size([700.0, 400.0]);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "Quick JSON Viewer",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| {
            let settings = cc.storage
                .map(|s| Settings::load(s))
                .unwrap_or_default();
            setup_unicode_fonts(&cc.egui_ctx);
            Ok(Box::new(App { settings, ..Default::default() }))
        }),
    )
}

// ─── App impl ────────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.settings.save(storage);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // ── 0. Apply settings — only when they actually changed. Rebuilding
        // and installing the style every frame clones the whole egui style
        // and dirties downstream caches for nothing.
        let prefer_dark = ui.ctx().global_style().visuals.dark_mode;
        let style_key = (self.settings.theme, self.settings.font_family, self.settings.font_size, prefer_dark);
        if self.style_applied != Some(style_key) {
            self.style_applied = Some(style_key);
            self.settings.apply_theme(ui.ctx(), prefer_dark);
            self.settings.apply_fonts(ui.ctx());
        }
        // Settings dialog (rendered over everything)
        {
            let open = &mut self.settings_open;
            let settings = &mut self.settings;
            show_settings_window(settings, ui.ctx(), open, &mut self.ai_settings_ui);
        }
        self.show_help_window(ui.ctx());
        self.show_search_help_window(ui.ctx());
        self.show_about_window(ui.ctx());
        self.show_url_dialog(ui.ctx());
        self.show_edit_dialog(ui.ctx());
        self.show_add_item_dialog(ui.ctx());
        self.show_error_context_window(ui.ctx());
        self.show_compare_error_context_windows(ui.ctx());

        // ── macOS native menu bar (installed once, actions polled every frame) ──
        #[cfg(target_os = "macos")]
        {
            if !self.menu_installed {
                macos_menu::install(ui.ctx());
                self.menu_installed = true;
            }
            let acts = macos_menu::take_actions();
            if acts & macos_menu::ACT_OPEN_FILE    != 0 { self.open_active_dialog(); }
            if acts & macos_menu::ACT_OPEN_URL     != 0 { self.open_url_dialog(); }
            if acts & macos_menu::ACT_PASTE        != 0 { self.request_paste(ui.ctx()); }
            if acts & macos_menu::ACT_SETTINGS     != 0 { self.settings_open = true; }
            if acts & macos_menu::ACT_FOCUS_SEARCH != 0 { self.focus_search = true; }
            if acts & macos_menu::ACT_COLLAPSE_ALL != 0 { self.collapse_all_active(); }
            if acts & macos_menu::ACT_EXPAND_ALL   != 0 { self.expand_all_active(); }
            if acts & macos_menu::ACT_HELP         != 0 { self.help_open  = true; }
            if acts & macos_menu::ACT_SEARCH_SYNTAX != 0 { self.search_help_open = true; }
            if acts & macos_menu::ACT_ABOUT        != 0 { self.about_open = true; }
            if acts & macos_menu::ACT_EXPORT_JSON  != 0 { self.export(ExportScope::File, ExportFormat::Json); }
            if acts & macos_menu::ACT_EXPORT_CSV   != 0 { self.export(ExportScope::File, ExportFormat::Csv); }
            if acts & macos_menu::ACT_SAVE         != 0 { self.save_overwrite(); }
            if acts & macos_menu::ACT_SAVE_COPY    != 0 { self.save_copy(); }
            if acts & macos_menu::ACT_UNDO         != 0 { self.undo(); }
            if acts & macos_menu::ACT_REDO         != 0 { self.redo(); }
            if let Some(path) = macos_menu::take_open_file() { self.open_file(path); }
        }

        // ── Update check: fire once on launch, plus on manual request ──
        if !self.update_check_started {
            self.update_check_started = true;
            self.update_rx = Some(update::spawn_check());
        }
        if settings::take_update_check_request() {
            // A manual check is an explicit "show me" — override any prior
            // dismissal so the banner reappears even for the same release.
            self.settings.dismissed_update = None;
            self.update_available = None;
            self.update_rx = Some(update::spawn_check());
        }
        self.poll_update(ui.ctx());

        // ── 1. Poll background loader — drain everything queued so progress
        // messages don't lag one frame behind each.
        while let Some(rx) = &self.load_rx {
            match rx.try_recv() {
                Ok(LoadMsg::Progress(p)) => {
                    self.load_progress = p;
                    ui.ctx().request_repaint();
                }
                Ok(LoadMsg::Done(idx)) => {
                    self.tree = Some(TreeState::new(idx));
                    self.load_rx = None;
                }
                Ok(LoadMsg::Error(e, ctx)) => {
                    self.load_error = Some(e);
                    self.load_error_ctx = ctx;
                    self.error_ctx_open = false;
                    self.load_rx = None;
                }
                Err(_) => break,
            }
        }

        // ── 1b. Poll background export/save writer ──
        if let Some(rx) = &self.bg_write_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.bg_write_rx = None;
                    self.finish_bg_write(res);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.bg_write_rx = None,
            }
        }

        // ── 1b². Poll the AI assistant's background turn ──
        self.ai.poll(ui.ctx());

        // ── 1c. Poll the two Compare-pane loaders + (re)compute the diff ──
        self.poll_pane_loader(Side::Left, ui.ctx());
        self.poll_pane_loader(Side::Right, ui.ctx());
        self.recompute_diff_if_needed();
        self.poll_diff(ui.ctx());

        // Keep repainting while loading or diffing
        if self.load_rx.is_some()
            || self.compare.left.load_rx.is_some()
            || self.compare.right.load_rx.is_some()
            || self.compare.diff_rx.is_some()
        {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }

        // ── 2. Debounced search kick + poll background search ──
        if let Some(deadline) = self.search_debounce_until {
            let now = ui.input(|i| i.time);
            if now >= deadline {
                self.search_debounce_until = None;
                self.kick_search();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(deadline - now));
            }
        }
        let search_done = self
            .search_pending
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false);
        if search_done {
            // None = the scan was cancelled mid-flight; discard.
            let results = self.search_pending.take().unwrap().join().ok().flatten();
            if let (Some(t), Some(results)) = (&mut self.tree, results) {
                t.set_search_results(results);
            }
        }

        // ── 3. Drag-and-drop ──
        let dropped_path = ui.input(|i| {
            i.raw.dropped_files.first().and_then(|f| f.path.clone())
        });
        if let Some(path) = dropped_path {
            match self.mode {
                AppMode::Viewer  => self.open_file(path),
                AppMode::Compare => {
                    let side = self.drop_side(ui);
                    self.open_file_into_pane(side, path);
                }
            }
        }

        // ── 3b. Paste — view clipboard text as a document. Fires on ⌘V when no
        // text field is focused, or unconditionally after a menu/toolbar paste
        // request (`paste_pending`). The event is removed from the input queue
        // so the search box never sees it.
        let no_text_focus = ui.ctx().memory(|m| m.focused().is_none());
        if self.paste_pending || no_text_focus {
            let pasted = ui.input_mut(|i| {
                let mut found = None;
                i.events.retain(|e| {
                    if found.is_none() {
                        if let egui::Event::Paste(s) = e {
                            found = Some(s.clone());
                            return false;
                        }
                    }
                    true
                });
                found
            });
            if let Some(text) = pasted {
                self.paste_pending = false;
                // A file copied in Finder pastes only its display name as
                // text; the actual path lives in the pasteboard's file-url
                // type. Check that first and open the file like a drop.
                #[cfg(target_os = "macos")]
                let file = macos_menu::clipboard_file_path();
                #[cfg(not(target_os = "macos"))]
                let file: Option<PathBuf> = None;
                match (file, self.mode) {
                    (Some(path), AppMode::Viewer)  => self.open_file(path),
                    (Some(path), AppMode::Compare) => {
                        let side = self.compare.active_pane;
                        self.open_file_into_pane(side, path);
                    }
                    (None, AppMode::Viewer)  => self.open_pasted(&text),
                    (None, AppMode::Compare) => {
                        let side = self.compare.active_pane;
                        self.open_pasted_into_pane(side, &text);
                    }
                }
            }
        }

        // ── 3c. ⌘C — copy selected node value when no text field is focused ──
        // egui-winit converts Cmd+C into Event::Copy (early-return, no Key event),
        // so we must intercept Event::Copy rather than using key_pressed(Key::C).
        if no_text_focus {
            let copy_event = ui.input_mut(|i| {
                let mut found = false;
                i.events.retain(|e| {
                    if !found && matches!(e, egui::Event::Copy) {
                        found = true;
                        return false;
                    }
                    true
                });
                found
            });
            if copy_event {
                match self.mode {
                    AppMode::Viewer => {
                        if let Some(t) = &self.tree {
                            if let Some(sel) = t.selected {
                                if t.is_added(sel) {
                                    ui.ctx().copy_text(t.added_item(sel).raw_value.clone());
                                } else {
                                    let n = &t.index.nodes[sel as usize];
                                    let raw = t.index.value_bytes(n);
                                    ui.ctx().copy_text(String::from_utf8_lossy(raw).into_owned());
                                }
                            }
                        }
                    }
                    AppMode::Compare => {}
                }
            }
        }

        // ── 4. Keyboard shortcuts ──
        let (cmd_o, cmd_f, cmd_comma, cmd_l, arrow_up, arrow_down, arrow_left, arrow_right,
             cmd_g, cmd_shift_g, opt_c, opt_x,
             page_up, page_down, home, end, f2, cmd_s, delete_key, cmd_z, cmd_shift_z) =
            ui.input(|i| {
                let cmd   = i.modifiers.command;
                let shift = i.modifiers.shift;
                let alt   = i.modifiers.alt;
                let none  = !cmd && !shift && !alt;
                (
                    cmd && i.key_pressed(egui::Key::O),
                    cmd && i.key_pressed(egui::Key::F),
                    cmd && i.key_pressed(egui::Key::Comma),
                    cmd && !shift && !alt && i.key_pressed(egui::Key::L),
                    none && i.key_pressed(egui::Key::ArrowUp),
                    none && i.key_pressed(egui::Key::ArrowDown),
                    none && i.key_pressed(egui::Key::ArrowLeft),
                    none && i.key_pressed(egui::Key::ArrowRight),
                    cmd && !shift && i.key_pressed(egui::Key::G),
                    cmd &&  shift && i.key_pressed(egui::Key::G),
                    alt && !cmd && i.key_pressed(egui::Key::C),
                    alt && !cmd && i.key_pressed(egui::Key::X),
                    none && i.key_pressed(egui::Key::PageUp),
                    none && i.key_pressed(egui::Key::PageDown),
                    none && i.key_pressed(egui::Key::Home),
                    none && i.key_pressed(egui::Key::End),
                    none && i.key_pressed(egui::Key::F2),
                    cmd && !shift && i.key_pressed(egui::Key::S),
                    none && i.key_pressed(egui::Key::Delete),
                    cmd && !shift && i.key_pressed(egui::Key::Z),
                    cmd &&  shift && i.key_pressed(egui::Key::Z),
                )
            });

        if cmd_o      { self.open_active_dialog(); }
        if cmd_l      { self.open_url_dialog(); }
        if cmd_f      { self.focus_search = true; }
        if cmd_comma  { self.settings_open = true; }
        if opt_c      { self.collapse_all_active(); }
        if opt_x      { self.expand_all_active(); }
        match self.mode {
            AppMode::Viewer => {
                if let Some(t) = &mut self.tree {
                    if arrow_up    { t.select_up(); }
                    if arrow_down  { t.select_down(); }
                    if arrow_left  { t.select_left(); }
                    if arrow_right { t.select_right(); }
                    if cmd_g       { t.search_next(); }
                    if cmd_shift_g { t.search_prev(); }
                    if page_up     { t.select_page_up(20); }
                    if page_down   { t.select_page_down(20); }
                    if home        { t.select_home(); }
                    if end         { t.select_end(); }
                }
            }
            AppMode::Compare => {
                if let (Some(result), Some(t)) = (&self.compare.result, &mut self.compare.tree) {
                    if arrow_up    { t.select_up(); }
                    if arrow_down  { t.select_down(); }
                    if arrow_left  { t.select_left(result); }
                    if arrow_right { t.select_right(result); }
                    if cmd_g       { t.next_diff(result); }
                    if cmd_shift_g { t.prev_diff(result); }
                    if page_up     { t.select_page_up(20); }
                    if page_down   { t.select_page_down(20); }
                    if home        { t.select_home(); }
                    if end         { t.select_end(); }
                }
            }
        }

        // F2 → edit selected leaf value.
        if f2 && self.mode == AppMode::Viewer {
            if let Some(t) = &self.tree {
                if let Some(sel) = t.selected {
                    let editable = t.is_added(sel) || {
                        let node = &t.index.nodes[sel as usize];
                        !matches!(node.kind, index::NodeKind::Object | index::NodeKind::Array)
                    };
                    if editable {
                        self.start_edit(sel, EditField::Value);
                    }
                }
            }
        }

        // Delete → toggle delete on the selected node (skip the root).
        if delete_key && self.mode == AppMode::Viewer && ui.ctx().memory(|m| m.focused().is_none()) {
            let to_delete = self.tree.as_ref().and_then(|t| {
                t.selected.filter(|&sel| t.is_added(sel) || t.index.nodes[sel as usize].parent != u32::MAX)
            });
            if let Some(n) = to_delete {
                self.toggle_delete(n);
            }
        }

        // Cmd+Z / Cmd+Shift+Z → undo / redo the last overlay edit (rename,
        // value change, delete/restore). Gated on no text field having focus
        // so a text box's own undo (e.g. the search box, edit dialog) wins.
        if cmd_z && self.mode == AppMode::Viewer && no_text_focus {
            self.undo();
        }
        if cmd_shift_z && self.mode == AppMode::Viewer && no_text_focus {
            self.redo();
        }

        // Cmd+S → save: overwrite the original file, or save a copy when the
        // document has no path (pasted). Only when there are unsaved changes.
        if cmd_s && self.mode == AppMode::Viewer && self.is_dirty() {
            if self.can_overwrite() {
                self.save_overwrite();
            } else {
                self.save_copy();
            }
        }

        // ── 4b. Type-ahead selection ──
        // Only active when no text widget (e.g. search box) has keyboard focus.
        if self.mode == AppMode::Viewer && self.tree.is_some() && ui.ctx().memory(|m| m.focused().is_none()) {
            let now = ui.input(|i| i.time);
            let typed: String = ui.input(|i| {
                i.events.iter().filter_map(|e| {
                    if let egui::Event::Text(t) = e { Some(t.as_str()) } else { None }
                }).collect()
            });
            if !typed.is_empty() {
                if now - self.type_ahead_time > 1.0 {
                    self.type_ahead.clear();
                }
                self.type_ahead.push_str(&typed);
                self.type_ahead_time = now;
                let prefix = self.type_ahead.clone();
                if let Some(t) = &mut self.tree {
                    t.type_ahead_select(&prefix);
                }
            }
        }

        // ── 5. Layout ──
        // Chrome palette for this frame's theme. Derived from settings (not
        // `ui.visuals()`) so the panel fills set here match the visuals
        // `apply_theme` installed above, even on the first frame after a toggle.
        let pal = theme::Palette::for_dark(self.settings.is_dark(prefer_dark));

        if self.settings.show_menu_bar {
            egui::Panel::top("menubar")
                .exact_size(20.0)
                .show_inside(ui, |ui| {
                    self.menu_bar(ui);
                });
        }

        egui::Panel::top("toolbar")
            .exact_size(44.0)
            .frame(
                egui::Frame::new()
                    .fill(pal.bg_panel)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                self.toolbar(ui);
            });

        self.update_banner(ui, &pal);

        if self.mode == AppMode::Viewer && self.settings.show_breadcrumbs && self.tree.is_some() {
            egui::Panel::top("breadcrumbs")
                .exact_size(self.settings.font_size + 14.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal.bg_breadcrumbs)
                        .inner_margin(egui::Margin::symmetric(10, 0)),
                )
                .show_inside(ui, |ui| {
                    self.breadcrumbs_bar(ui);
                });
        }

        if self.mode == AppMode::Compare {
            egui::Panel::top("compareoptions")
                .exact_size(self.settings.font_size + 20.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal.bg_breadcrumbs)
                        .inner_margin(egui::Margin::symmetric(10, 0)),
                )
                .show_inside(ui, |ui| {
                    self.compare_options_bar(ui);
                });
        }

        let mut status_req: (Option<(ExportScope, ExportFormat)>, Option<SaveAction>) = (None, None);
        egui::Panel::bottom("statusbar")
            .exact_size(26.0)
            .frame(
                egui::Frame::new()
                    .fill(pal.bg_panel)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                status_req = self.status_bar(ui);
            });
        if let Some((scope, fmt)) = status_req.0 {
            self.export(scope, fmt);
        }
        match status_req.1 {
            Some(SaveAction::Overwrite) => self.save_overwrite(),
            Some(SaveAction::Copy)      => self.save_copy(),
            None => {}
        }

        // ── AI assistant side panel (Viewer mode, opt-in via Settings) ──
        if self.mode == AppMode::Viewer && self.settings.ai_enabled && self.ai.open {
            let mut apply_edits = None;
            egui::Panel::right("ai_panel")
                .resizable(true)
                .default_size(360.0)
                .size_range(260.0..=780.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal.bg_panel)
                        .inner_margin(egui::Margin::symmetric(10, 0)),
                )
                .show_inside(ui, |ui| {
                    let index = self.tree.as_ref().map(|t| &t.index);
                    let file_name = self
                        .file_info
                        .as_ref()
                        .map(|f| f.name.as_str())
                        .unwrap_or("document.json");
                    apply_edits =
                        ai::panel::show(&mut self.ai, ui, &self.settings, index, file_name);
                });
            if let Some(edits) = apply_edits {
                self.apply_ai_edits(edits);
            }
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            match self.mode {
                AppMode::Viewer  => self.tree_panel(ui),
                AppMode::Compare => self.compare_panel(ui),
            }
        });
    }
}

// ─── menu bar ────────────────────────────────────────────────────────────────

impl App {
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("Open…").shortcut_text("⌘O")).clicked() {
                    ui.close();
                    self.open_file_dialog();
                }
                if ui.add(egui::Button::new("Paste JSON / JWT").shortcut_text("⌘V")).clicked() {
                    ui.close();
                    let ctx = ui.ctx().clone();
                    self.request_paste(&ctx);
                }
                ui.separator();
                let has_tree = self.tree.is_some();
                ui.add_enabled_ui(has_tree, |ui| {
                    ui.menu_button("Export File", |ui| {
                        if ui.button("As JSON").clicked() {
                            ui.close();
                            self.export(ExportScope::File, ExportFormat::Json);
                        }
                        if ui.button("As CSV").clicked() {
                            ui.close();
                            self.export(ExportScope::File, ExportFormat::Csv);
                        }
                    });
                });
                ui.separator();
                let dirty    = self.is_dirty();
                let can_over = self.can_overwrite();
                ui.add_enabled_ui(dirty && can_over, |ui| {
                    if ui.add(egui::Button::new("Save").shortcut_text("⌘S")).clicked() {
                        ui.close();
                        self.save_overwrite();
                    }
                });
                ui.add_enabled_ui(dirty, |ui| {
                    if ui.add(egui::Button::new("Save a Copy").shortcut_text("⇧⌘S")).clicked() {
                        ui.close();
                        self.save_copy();
                    }
                    if ui.button("Discard Changes").clicked() {
                        ui.close();
                        self.discard_changes();
                    }
                });
                ui.separator();
                if ui.add(egui::Button::new("Settings").shortcut_text("⌘,")).clicked() {
                    ui.close();
                    self.settings_open = true;
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(self.can_undo(), egui::Button::new("Undo").shortcut_text("⌘Z")).clicked() {
                    ui.close();
                    self.undo();
                }
                if ui.add_enabled(self.can_redo(), egui::Button::new("Redo").shortcut_text("⇧⌘Z")).clicked() {
                    ui.close();
                    self.redo();
                }
            });
            ui.menu_button("View", |ui| {
                let has_tree = self.tree.is_some();
                if ui.add_enabled(has_tree, egui::Button::new("Collapse All").shortcut_text("⌥C")).clicked() {
                    ui.close();
                    if let Some(t) = &mut self.tree { t.collapse_all(); }
                }
                if ui.add_enabled(has_tree, egui::Button::new("Expand All").shortcut_text("⌥X")).clicked() {
                    ui.close();
                    if let Some(t) = &mut self.tree { t.expand_all(); }
                }
                ui.separator();
                if ui.add(egui::Button::new("Search").shortcut_text("⌘F")).clicked() {
                    ui.close();
                    self.focus_search = true;
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard Shortcuts").clicked() {
                    ui.close();
                    self.help_open = true;
                }
                if ui.button("Search Syntax").clicked() {
                    ui.close();
                    self.search_help_open = true;
                }
                ui.separator();
                if ui.button("About JSON Viewer").clicked() {
                    ui.close();
                    self.about_open = true;
                }
            });
        });
    }
}

// ─── help & about dialogs ────────────────────────────────────────────────────

impl App {
    fn show_help_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("⌨  Keyboard Shortcuts")
            .open(&mut self.help_open)
            .collapsible(false)
            .resizable(false)
            .min_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(6.0);

                let pal = theme::Palette::for_dark(ui.visuals().dark_mode);

                let row = |ui: &mut egui::Ui, key: &str, desc: &str| {
                    ui.horizontal(|ui| {
                        for (i, cap) in key.split(" / ").enumerate() {
                            if i > 0 {
                                ui.label(egui::RichText::new("/").color(pal.text_muted));
                            }
                            Self::keycap(ui, &pal, cap);
                        }
                    });
                    ui.label(desc);
                    ui.end_row();
                };

                let section = |ui: &mut egui::Ui, title: &str| {
                    // add_space is invalid inside a grid; emit a blank row instead
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                    ui.label(egui::RichText::new(title).strong());
                    ui.label("");
                    ui.end_row();
                };

                egui::Grid::new("shortcuts_grid")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        section(ui, "File");
                        row(ui, "⌘ O",       "Open file");
                        row(ui, "⌘ L",       "Open URL");
                        row(ui, "⌘ V",       "Paste JSON / JWT / curl from clipboard");
                        row(ui, "⌘ ,",       "Settings");

                        section(ui, "Navigation");
                        row(ui, "↑ / ↓",     "Select previous / next row");
                        row(ui, "← / →",     "Collapse / expand node");
                        row(ui, "Page Up/Dn","Jump 20 rows");
                        row(ui, "Home / End","Jump to first / last row");

                        section(ui, "Tree");
                        row(ui, "⌥ C",       "Collapse all");
                        row(ui, "⌥ X",       "Expand all");

                        section(ui, "Search");
                        row(ui, "⌘ F",       "Focus search box");
                        row(ui, "Enter",      "Next result");
                        row(ui, "⌘ G",       "Next result");
                        row(ui, "⌘ ⇧ G",     "Previous result");

                        section(ui, "Editing");
                        row(ui, "Double-click",  "Edit leaf value");
                        row(ui, "F2",            "Edit selected value");
                        row(ui, "⌘ Z",           "Undo");
                        row(ui, "⇧ ⌘ Z",         "Redo");
                        row(ui, "⌘ S",           "Save (overwrite original)");
                        row(ui, "⇧ ⌘ S",         "Save a Copy");
                    });

                ui.add_space(8.0);
            });
    }

    fn show_search_help_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("🔍  Search Syntax")
            .open(&mut self.search_help_open)
            .collapsible(false)
            .resizable(false)
            .min_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(6.0);

                let row = |ui: &mut egui::Ui, syntax: &str, desc: &str| {
                    ui.label(egui::RichText::new(syntax).monospace().strong());
                    ui.label(desc);
                    ui.end_row();
                };

                let section = |ui: &mut egui::Ui, title: &str| {
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                    ui.label(egui::RichText::new(title).strong());
                    ui.label("");
                    ui.end_row();
                };

                egui::Grid::new("search_syntax_grid")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        section(ui, "Text");
                        row(ui, "error",          "Keys or values containing \"error\"");
                        row(ui, "\"foo bar\"",    "Quote to match text with spaces");

                        section(ui, "Target");
                        row(ui, "key:name",       "Keys containing \"name\"");
                        row(ui, "value:err",      "Values containing \"err\"");

                        section(ui, "Comparison");
                        row(ui, "age > 30",       "Key \"age\" with numeric value > 30");
                        row(ui, "price <= 9.99",  "Operators:  =  !=  <  <=  >  >=");
                        row(ui, "status = active","Exact string equality");
                        row(ui, "value > 100",    "Any key with numeric value > 100");
                        row(ui, "date >= 2024-01-01", "Non-numbers compare alphabetically");

                        section(ui, "Combining");
                        row(ui, "key:user value > 1000", "Space-separated parts — all must match");

                        section(ui, "Regex");
                        row(ui, ".* toggle",      "Regex on keys and values (disables the above)");
                    });

                ui.add_space(8.0);
            });
    }

    /// Whether an update banner/badge should currently be shown — there is a
    /// newer release and the user hasn't dismissed that particular version.
    fn pending_update(&self) -> Option<&update::ReleaseInfo> {
        let info = self.update_available.as_ref()?;
        if self.settings.dismissed_update.as_deref() == Some(info.version.as_str()) {
            return None;
        }
        Some(info)
    }

    /// Thin top strip shown when a newer release is available. Notify-only: it
    /// links to the release page and offers the `brew upgrade` command — it
    /// never downloads or replaces the binary.
    fn update_banner(&mut self, ui: &mut egui::Ui, pal: &theme::Palette) {
        let Some(info) = self.pending_update() else { return };
        let version = info.version.clone();
        let html_url = info.html_url.clone();
        // Show the first few lines of the release notes as a hover hint.
        let notes_hint: String = info.notes.lines().take(12).collect::<Vec<_>>().join("\n");

        let upgrading = self.install_watcher_rx.is_some();
        let mut dismiss = false;
        egui::Panel::top("update_banner")
            .exact_size(self.settings.font_size + 16.0)
            .frame(
                egui::Frame::new()
                    .fill(pal.accent)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let label = if upgrading {
                        format!("⬆  Installing v{version}…")
                    } else {
                        format!("⬆  Update available — v{version}")
                    };
                    ui.label(
                        egui::RichText::new(label)
                            .color(egui::Color32::WHITE)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !upgrading {
                            if ui.button("X").on_hover_text("Dismiss").clicked() {
                                dismiss = true;
                            }
                        }
                        if ui
                            .button(if upgrading { "Upgrading…" } else { "Upgrade now" })
                            .on_hover_text(format!("Runs in the background:\n{}", update::BREW_UPGRADE_CMD))
                            .clicked()
                            && !upgrading
                        {
                            self.install_watcher_rx = Some(update::launch_brew_upgrade());
                            ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
                        }
                        if !upgrading {
                            if ui.button("Copy command").clicked() {
                                ui.ctx().copy_text(update::BREW_UPGRADE_CMD.to_string());
                            }
                            let view = ui.button("View release");
                            let view = if notes_hint.trim().is_empty() {
                                view
                            } else {
                                view.on_hover_text(&notes_hint)
                            };
                            if view.clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(&html_url));
                            }
                        }
                    });
                });
            });

        if dismiss {
            self.settings.dismissed_update = Some(version);
        }
    }

    fn show_about_window(&mut self, ctx: &egui::Context) {
        // Snapshot update state before borrowing `self.about_open` mutably.
        let update_badge = self
            .pending_update()
            .map(|info| (info.version.clone(), info.html_url.clone()));
        egui::Window::new("About JSON Viewer")
            .open(&mut self.about_open)
            .collapsible(false)
            .resizable(false)
            .min_width(300.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("JSON Viewer").heading().strong());
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION"))).small());
                    ui.add_space(6.0);
                    match &update_badge {
                        Some((version, url)) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 159, 10),
                                format!("Update available: v{version}"),
                            );
                            if ui.link("View release").clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            }
                        }
                        None => {
                            ui.colored_label(
                                egui::Color32::from_rgb(52, 199, 89),
                                "Up to date",
                            );
                        }
                    }
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.label("A fast, lightweight JSON tree viewer with advanced search, BiDi text support, and more.");
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("Created by").weak());
                    ui.label(egui::RichText::new("Evyatar Shalom").strong());
                    ui.add_space(12.0);
                });
            });
    }

    fn show_error_context_window(&mut self, ctx: &egui::Context) {
        let Some(ec) = &self.load_error_ctx else { return };
        let before = ec.before.clone();
        let at     = ec.at.clone();
        let after  = ec.after.clone();
        let error  = self.load_error.clone();
        egui::Window::new("Parse Error Context")
            .open(&mut self.error_ctx_open)
            .collapsible(false)
            .resizable(false)
            .min_width(640.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                error_context_ui(ui, error.as_deref(), &before, &at, &after);
            });
    }

    fn show_compare_error_context_windows(&mut self, ctx: &egui::Context) {
        for side in [Side::Left, Side::Right] {
            let pane = self.compare.pane(side);
            if !pane.error_ctx_open { continue; }
            let Some(ec) = &pane.load_error_ctx else { continue };
            let before = ec.before.clone();
            let at     = ec.at.clone();
            let after  = ec.after.clone();
            let error  = pane.load_error.clone();
            let label  = match side { Side::Left => "Left", Side::Right => "Right" };
            let title  = format!("Parse Error Context — {label}");
            let open   = &mut self.compare.pane_mut(side).error_ctx_open;
            egui::Window::new(title)
                .open(open)
                .collapsible(false)
                .resizable(false)
                .min_width(640.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    error_context_ui(ui, error.as_deref(), &before, &at, &after);
                });
        }
    }

    fn show_url_dialog(&mut self, ctx: &egui::Context) {
        if !self.url_dialog_open { return; }

        let mut window_open   = true;
        let mut do_open       = false;
        let mut do_cancel     = false;
        let focus_this_frame  = self.url_dialog_focus;

        egui::Window::new("Open URL")
            .open(&mut window_open)
            .resizable(false)
            .collapsible(false)
            .min_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label("Paste a URL, curl command, or fetch() call:");
                ui.add_space(6.0);

                let resp = ui.add(
                    egui::TextEdit::multiline(&mut self.url_dialog_input)
                        .hint_text(
                            "https://api.example.com/data\n\
                             — or —\n\
                             curl -H \"Authorization: Bearer …\" https://api.example.com/data",
                        )
                        .desired_width(f32::INFINITY)
                        .desired_rows(4),
                );
                if focus_this_frame {
                    resp.request_focus();
                }

                ui.add_space(6.0);

                let parsed = url_parse::parse_request(&self.url_dialog_input);
                if let Some(ref req) = parsed {
                    let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("→").color(pal.text_muted).small());
                        ui.label(
                            egui::RichText::new(&req.url)
                                .small()
                                .color(pal.accent),
                        );
                    });
                    if !req.headers.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("{} header(s) detected", req.headers.len()))
                                .small()
                                .color(pal.text_muted),
                        );
                    }
                    ui.add_space(4.0);
                }

                let can_open = parsed.is_some();
                // ⌘↵ submits without inserting a newline into the multiline field
                let enter_submitted = ui.input(|i| {
                    i.key_pressed(egui::Key::Enter) && i.modifiers.command
                }) && can_open;

                ui.horizontal(|ui| {
                    if ui.add_enabled(can_open, egui::Button::new("Open")).clicked()
                        || enter_submitted
                    {
                        do_open = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
                ui.add_space(4.0);
            });

        if !window_open || do_cancel {
            self.url_dialog_open = false;
        }
        if focus_this_frame {
            self.url_dialog_focus = false;
        }
        if do_open {
            if let Some(req) = url_parse::parse_request(&self.url_dialog_input) {
                self.url_dialog_open  = false;
                self.url_dialog_input.clear();
                self.open_url_request(req);
            }
        }
    }
}

// ─── toolbar ─────────────────────────────────────────────────────────────────

/// A tab / toggle rendered as a pill. When `active` it gets a filled
/// background with high-contrast text; when inactive it is plain (frameless)
/// muted text. Used for the Viewer/Compare tabs and the toolbar toggles so the
/// active state stays readable in both light and dark themes (an accent-on-
/// accent `selectable_label` did not).
fn tab_button(
    ui:     &mut egui::Ui,
    pal:    &theme::Palette,
    label:  egui::RichText,
    active: bool,
) -> egui::Response {
    let fg = if active { pal.tab_active_fg } else { pal.tab_inactive_fg };
    let fill = if active { pal.tab_active_bg } else { egui::Color32::TRANSPARENT };
    let stroke = egui::Stroke::new(1.0_f32, if active { pal.tab_active_fg } else { pal.border });
    let button = egui::Button::new(label.color(fg))
        .frame(true)
        .fill(fill)
        .stroke(stroke);
    ui.add(button)
}

impl App {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        ui.horizontal_centered(|ui| {
            self.mode_tabs(ui);
            ui.add_space(10.0);

            match self.mode {
                AppMode::Viewer  => self.viewer_toolbar(ui),
                AppMode::Compare => self.compare_toolbar(ui),
            }

            // Settings button (right-aligned, both modes)
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚙").clicked() {
                    self.settings_open = !self.settings_open;
                }
            });
        });

        // 1 px bottom border under the header
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0_f32, pal.border));
    }

    fn mode_tabs(&mut self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        for (label, mode) in [("Viewer", AppMode::Viewer), ("Compare", AppMode::Compare)] {
            let active = self.mode == mode;
            if tab_button(ui, &pal, egui::RichText::new(label).strong(), active).clicked() {
                self.set_mode(mode);
            }
        }
    }

    fn viewer_toolbar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        if ui.button("Open File").clicked() {
            self.open_file_dialog();
        }
        if let Some(tree) = &mut self.tree {
            let mut on = tree.multi_select;
            let resp = ui.selectable_label(on, "☑ Select");
            if resp.clicked() {
                on = !on;
                tree.set_multi_select(on);
            }
            resp.on_hover_text("Multi-select mode — check rows, then right-click → Export");
        }
        if self.settings.ai_enabled {
            let resp = tab_button(ui, &pal, egui::RichText::new("✨ AI").strong(), self.ai.open);
            if resp.on_hover_text("AI assistant — query and edit with your own API key").clicked() {
                self.ai.open = !self.ai.open;
            }
        }
        ui.add_space(8.0);

            // Shrink the search field on narrow windows so the controls to its
            // right (.*  ?  ▲  ▼  counter  ⚙) stay visible instead of being
            // pushed off-screen.
            let search_width = {
                let font = egui::TextStyle::Body.resolve(ui.style());
                let text_w = |s: &str| {
                    ui.painter()
                        .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                        .size()
                        .x
                };
                let pad = 2.0 * ui.spacing().button_padding.x;
                let gap = ui.spacing().item_spacing.x;
                let counter = self
                    .tree
                    .as_ref()
                    .filter(|t| !t.search_results.is_empty())
                    .map(|t| {
                        text_w(&format!("{}/{}", t.search_cursor + 1, t.search_results.len())) + gap
                    })
                    .unwrap_or(0.0);
                // The chrome strings are constant — measure them only when the
                // font changes instead of 6 layouts per frame.
                let chrome_key = (font.size, self.settings.font_family);
                let (buttons_raw, magnifier_w) = match self.search_chrome_cache {
                    Some((key, b, m)) if key == chrome_key => (b, m),
                    _ => {
                        let b: f32 = [".*", "?", "▲", "▼", "⚙"].iter().map(|s| text_w(s)).sum();
                        let m = text_w("🔍");
                        self.search_chrome_cache = Some((chrome_key, b, m));
                        (b, m)
                    }
                };
                let buttons = buttons_raw + 5.0 * (pad + gap);
                // pill chrome: inner margins, icon, clear button, item gaps, stroke
                let pill = 16.0 + magnifier_w + 4.0 + 16.0 + 4.0 + 2.0;
                (ui.available_width() - buttons - counter - pill - gap).clamp(80.0, 260.0)
            };

            // Search pill — rounded container holding the search field
            egui::Frame::new()
                .fill(pal.bg_search)
                .stroke(egui::Stroke::new(1.0_f32, pal.border))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;

                        ui.label(egui::RichText::new("🔍").color(pal.text_muted));

                        let resp = {
                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                            let color   = ui.visuals().text_color();
                            let mut layouter = move |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
                                let display = bidi_reorder(text.as_str());
                                let job = egui::text::LayoutJob::simple_singleline(
                                    display.into_owned(),
                                    font_id.clone(),
                                    color,
                                );
                                ui.painter().layout_job(job)
                            };
                            let te = egui::TextEdit::singleline(&mut self.search_input)
                                .hint_text("Search… (age > 30)")
                                .desired_width(search_width)
                                .frame(egui::Frame::NONE)
                                .layouter(&mut layouter);
                            ui.add(te)
                        };
                        if resp.changed() {
                            // Debounce: don't scan the whole index per keystroke.
                            self.search_debounce_until = Some(ui.input(|i| i.time) + 0.15);
                        }
                        if self.focus_search {
                            resp.request_focus();
                            self.focus_search = false;
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if let Some(t) = &mut self.tree {
                                t.search_next();
                            }
                        }

                        // Clear button — only visible when the field has content
                        if !self.search_input.is_empty() {
                            let size = egui::Vec2::splat(16.0);
                            let (rect, clear) = ui.allocate_exact_size(size, egui::Sense::click());
                            let color = if clear.hovered() {
                                ui.visuals().widgets.hovered.text_color()
                            } else {
                                ui.visuals().widgets.inactive.text_color()
                            };
                            if clear.hovered() {
                                ui.painter().circle_filled(rect.center(), 7.0, ui.visuals().widgets.hovered.bg_fill);
                            }
                            let stroke = egui::Stroke::new(1.5_f32, color);
                            let d = 3.5;
                            let c = rect.center();
                            ui.painter().line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
                            ui.painter().line_segment([c + egui::vec2(d, -d), c + egui::vec2(-d, d)], stroke);
                            if clear.clicked() {
                                self.search_input.clear();
                                self.kick_search();
                                resp.request_focus();
                            }
                        }

                    });
                });

            // Regex toggle
            let use_re = self.tree.as_ref().map(|t| t.search_use_regex).unwrap_or(false);
            if tab_button(ui, &pal, egui::RichText::new(".*").monospace(), use_re).clicked() {
                if let Some(t) = &mut self.tree {
                    t.search_use_regex = !use_re;
                }
                self.kick_search();
            }

            // Search syntax help
            if ui.button("?").on_hover_text("Search syntax help").clicked() {
                self.search_help_open = true;
            }

            let has_results = !self.search_input.is_empty();
            if ui.add_enabled(has_results, egui::Button::new("▲")).clicked() {
                if let Some(t) = &mut self.tree { t.search_prev(); }
            }
            if ui.add_enabled(has_results, egui::Button::new("▼")).clicked() {
                if let Some(t) = &mut self.tree { t.search_next(); }
            }

            // Result counter
            if let Some(t) = &self.tree {
                if !t.search_results.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("{}/{}", t.search_cursor + 1, t.search_results.len()))
                            .color(pal.text_muted),
                    );
                }
            }
    }

    fn compare_toolbar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        // Summary of the current diff. Each counter is a toggle: clicking it
        // shows/hides that type of change; a muted color means it's turned off.
        if let Some(result) = &self.compare.result {
            let counts = (result.changed, result.added, result.removed);
            // All-zero counters mean identical files — say so instead of
            // showing a row of "0 …" badges.
            if counts == (0, 0, 0) {
                ui.label(
                    egui::RichText::new("identical files")
                        .color(theme::NEW),
                );
                return;
            }
            let mut filter = self.compare.filter;
            let badge = |ui: &mut egui::Ui, n: usize, label: &str, color: egui::Color32, on: &mut bool| {
                let text = egui::RichText::new(format!("{n} {label}"))
                    .color(if *on { color } else { pal.text_faint });
                let resp = ui.add(egui::Button::new(text).frame(false))
                    .on_hover_text(if *on { format!("Hide {label} nodes") } else { format!("Show {label} nodes") });
                if resp.clicked() { *on = !*on; }
            };
            badge(ui, counts.0, "changed", theme::CHANGED, &mut filter.changed);
            badge(ui, counts.1, "added",   theme::NEW,     &mut filter.added);
            badge(ui, counts.2, "removed", theme::DELETED, &mut filter.removed);
            if filter != self.compare.filter {
                self.set_diff_filter(filter);
            }
        } else {
            ui.label(egui::RichText::new("Load both panes to compare").color(pal.text_muted));
        }

        ui.add_space(6.0);

        // Previous / next difference.
        let has_diffs = self.compare.result.as_ref()
            .map_or(false, |r| !r.diff_positions.is_empty());
        if ui.add_enabled(has_diffs, egui::Button::new("▲")).on_hover_text("Previous difference").clicked() {
            self.compare_prev_diff();
        }
        if ui.add_enabled(has_diffs, egui::Button::new("▼")).on_hover_text("Next difference").clicked() {
            self.compare_next_diff();
        }

        // "diffs only" filter.
        let only = self.compare.show_only_diffs;
        if tab_button(ui, &pal, egui::RichText::new("diffs only"), only)
            .on_hover_text("Hide unchanged nodes")
            .clicked()
        {
            self.set_only_diffs(!only);
        }
    }
}

// ─── breadcrumbs bar ─────────────────────────────────────────────────────────

impl App {
    fn breadcrumbs_bar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        // 1 px bottom border under the strip
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0_f32, pal.border));

        let font_size = self.settings.font_size - 1.0;

        // Borrow tree state directly — the closures below never touch
        // `self.tree` mutably, so no per-frame clone of `added_items` needed.
        let Some(tree) = &self.tree else { return };
        let Some(sel) = tree.selected else { return };
        let index: &index::JsonIndex = &tree.index;
        let added_items: &[export::AddedItem] = &tree.added_items;
        let nodes_len = index.nodes.len();

        // Ancestor chain, root first. A pending (not-yet-saved) added item has
        // no real node — walk up from its real parent instead.
        let mut chain: Vec<u32> = Vec::new();
        let mut cur = sel;
        if export::is_added(nodes_len, cur) {
            chain.push(cur);
            cur = added_items[cur as usize - nodes_len].parent;
        }
        loop {
            chain.push(cur);
            let parent = index.nodes[cur as usize].parent;
            if parent == u32::MAX {
                break;
            }
            cur = parent;
        }
        chain.reverse();

        let mut jump_to: Option<u32> = None;

        egui::ScrollArea::horizontal()
            .id_salt("breadcrumbs_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, &node_idx) in chain.iter().enumerate() {
                        if i > 0 {
                            ui.label(
                                egui::RichText::new("›")
                                    .monospace()
                                    .size(font_size)
                                    .color(pal.text_faint),
                            );
                        }
                        let is_added_row = export::is_added(nodes_len, node_idx);
                        let label: String = if is_added_row {
                            let item = &added_items[node_idx as usize - nodes_len];
                            match &item.key {
                                Some(k) => k.clone(),
                                None => format!("[{}]", export::added_display_index(&index.nodes, &added_items, node_idx)),
                            }
                        } else {
                            let node = &index.nodes[node_idx as usize];
                            if node.parent == u32::MAX {
                                "root".to_owned()
                            } else if node.key_len > 0 {
                                index.key_of(node).to_owned()
                            } else if node.array_index != u32::MAX {
                                format!("[{}]", node.array_index)
                            } else {
                                "\"\"".to_owned()
                            }
                        };
                        let display = bidi_reorder(&label).into_owned();
                        let is_last = i + 1 == chain.len();
                        let text = egui::RichText::new(display)
                            .monospace()
                            .size(font_size)
                            .color(if is_added_row { theme::NEW } else if is_last { pal.key } else { pal.text_muted });
                        let resp = ui
                            .selectable_label(false, text)
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            jump_to = Some(node_idx);
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Copy Path").clicked() {
                                let path = if is_added_row {
                                    let item = &added_items[node_idx as usize - nodes_len];
                                    let parent_path = build_path(&index.nodes, &index, item.parent);
                                    let segment = match &item.key {
                                        Some(k) => path_key_segment(k),
                                        None    => format!("[{}]", export::added_display_index(&index.nodes, &added_items, node_idx)),
                                    };
                                    format!("{parent_path}{segment}")
                                } else {
                                    build_path(&index.nodes, &index, node_idx)
                                };
                                ui.ctx().copy_text(path);
                                ui.close();
                            }
                        });
                    }
                });
            });

        if let Some(node_idx) = jump_to {
            if let Some(t) = &mut self.tree {
                t.selected = Some(node_idx);
                t.ensure_visible(node_idx);
            }
        }
    }
}

// ─── tree panel ──────────────────────────────────────────────────────────────

/// One feature tip is shown per launch, rotating with each start.
const EMPTY_STATE_TIPS: &[&str] = &[
    "Search understands filters — try  status = active  or  price > 100  in the search box.",
    "Paste a curl command with ⌘V and the response opens as a tree.",
    "Paste a JWT with ⌘V to decode its header and payload instantly.",
    "Double-click any value to edit it in place, then ⌘S to save.",
    "Right-click a row to copy its path, key, value — or export it as code.",
    "⌥C collapses the whole tree, ⌥X expands it back.",
    "The Compare view diffs two JSON documents side by side.",
    "Search with ⌘F, then Enter / ⌘G jumps between results.",
];

impl App {
    /// Keycap-styled chip for a keyboard shortcut, so the ⌘ glyph and the
    /// letter read as one unit instead of mismatched font sizes.
    fn keycap(ui: &mut egui::Ui, pal: &theme::Palette, text: &str) {
        egui::Frame::new()
            .fill(pal.bg_search)
            .stroke(egui::Stroke::new(1.0_f32, pal.border))
            .corner_radius(4.0)
            .inner_margin(egui::Margin::symmetric(7, 2))
            .show(ui, |ui| {
                // The ⌘/⇧/⌥… glyphs come from the Apple Symbols fallback,
                // which draws larger than the monospace letters at equal point
                // size — size the symbol runs down a notch to visually match.
                let is_symbol = |c: char| !c.is_ascii();
                let mut job = egui::text::LayoutJob::default();
                let chars: Vec<char> = text.chars().collect();
                for chunk in chars.chunk_by(|a, b| is_symbol(*a) == is_symbol(*b)) {
                    job.append(
                        &chunk.iter().collect::<String>(),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::monospace(if is_symbol(chunk[0]) { 10.5 } else { 12.0 }),
                            color: pal.text_primary,
                            valign: egui::Align::Center,
                            ..Default::default()
                        },
                    );
                }
                ui.label(job);
            });
    }

    fn empty_state(ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);

        // Pick a tip once per launch so it doesn't flicker between frames.
        static TIP_IDX: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let tip = EMPTY_STATE_TIPS[*TIP_IDX.get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as usize)
                .unwrap_or(0)
                % EMPTY_STATE_TIPS.len()
        })];

        let avail = ui.available_height();
        ui.add_space((avail / 2.0 - 130.0).max(24.0));

        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("{ }").monospace().size(34.0).color(pal.text_faint));
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Open a JSON file to get started").size(17.0).color(pal.text_primary));
            ui.add_space(20.0);

            // A Grid always sits at the left edge of its parent, so indent it
            // by hand to keep the option list visually centered.
            let grid_w = 400.0;
            let indent = ((ui.available_width() - grid_w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(indent);
            egui::Grid::new("empty_state_options")
                .num_columns(2)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    let mut row = |cap: &str, desc: &str| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            Self::keycap(ui, &pal, cap);
                        });
                        ui.label(egui::RichText::new(desc).color(pal.text_muted));
                        ui.end_row();
                    };
                    row("⌘ O", "Open a file");
                    row("⌘ L", "Fetch JSON from a URL");
                    row("⌘ V", "Paste JSON, a JWT, or a curl command");
                    row("drop", "Drag a file anywhere in this window");
                });
            });

            ui.add_space(26.0);
            ui.label(
                egui::RichText::new(format!("Tip:  {tip}"))
                    .size(12.0)
                    .color(pal.text_faint),
            );
        });
    }

    fn tree_panel(&mut self, ui: &mut egui::Ui) {
        if self.tree.is_none() {
            if self.load_rx.is_some() {
                ui.centered_and_justified(|ui| { ui.spinner(); });
            } else {
                Self::empty_state(ui);
            }
            return;
        }

        let row_h    = self.settings.row_height();
        let key_font = self.settings.key_font();
        let val_font = self.settings.val_font();
        let copy_compact = self.settings.copy_compact_json;

        let edit_overlay  = &self.edit_overlay;  // field-disjoint borrow from self.tree
        let saved_overlay = &self.saved_overlay;
        let tree = self.tree.as_mut().unwrap();
        let num_rows = tree.visible.len();
        let scroll_to_row = tree.scroll_to_row.take();
        let reveal_row = tree.reveal_row.take();

        let mut actions: Vec<RowAction> = Vec::new();

        // Borrow individual fields so the closure can hold them immutably
        // while `actions` is mutably extended outside.
        {
            let index          = &*tree.index;
            let added_items    = &tree.added_items;
            let expanded       = &tree.expanded;
            let search_res_set = &tree.search_result_set;
            let visible        = &tree.visible;
            let selected       = tree.selected;
            let multi_select   = tree.multi_select;
            let checked        = &tree.checked;

            let avail_h   = ui.available_height();
            let row_pitch = row_h + ui.spacing().item_spacing.y;

            let mut scroll_area = egui::ScrollArea::both().auto_shrink([false; 2]);
            if let Some(row) = scroll_to_row {
                let y = (row as f32 * row_pitch - avail_h / 2.0 + row_h / 2.0).max(0.0);
                scroll_area = scroll_area.vertical_scroll_offset(y);
            }
            scroll_area.show_rows(ui, row_h, num_rows, |ui, row_range| {
                for row_idx in row_range {
                    let node_idx = visible[row_idx];
                    let row_actions = render_row(
                        ui, edit_overlay, saved_overlay, index, added_items, expanded, selected, search_res_set, node_idx,
                        row_h, key_font.clone(), val_font.clone(),
                        multi_select, checked.contains(&node_idx), !checked.is_empty(),
                        reveal_row == Some(row_idx), copy_compact,
                    );
                    actions.extend(row_actions);
                }
            });
        }

        // Apply actions after borrows released
        let mut export_req: Option<(ExportScope, ExportFormat)> = None;
        let mut start_edit_req: Option<(u32, EditField)> = None;
        let mut delete_req: Option<u32> = None;
        let mut add_item_req: Option<u32> = None;
        for action in actions {
            match action {
                RowAction::Select(n)           => { tree.selected = Some(n); }
                RowAction::Toggle(n)           => { tree.toggle(n); }
                RowAction::ToggleCheck(n)       => { tree.toggle_check(n); }
                RowAction::ExpandRecursive(n)   => { tree.expand_recursive(n); }
                RowAction::CollapseRecursive(n) => { tree.collapse_recursive(n); }
                RowAction::Export(scope, fmt)   => { export_req = Some((scope, fmt)); }
                RowAction::StartEditValue(n)    => { start_edit_req = Some((n, EditField::Value)); }
                RowAction::StartEditKey(n)      => { start_edit_req = Some((n, EditField::Key)); }
                RowAction::DeleteNode(n)        => { delete_req = Some(n); }
                RowAction::AddItem(n)           => { add_item_req = Some(n); }
            }
        }
        // `tree` borrow ends here; export/edit/delete/add need &mut self.
        if let Some((scope, fmt)) = export_req {
            self.export(scope, fmt);
        }
        if let Some((n, field)) = start_edit_req {
            self.start_edit(n, field);
        }
        if let Some(n) = delete_req {
            self.toggle_delete(n);
        }
        if let Some(parent) = add_item_req {
            self.start_add_item(parent);
        }
    }
}

// ─── row renderer ────────────────────────────────────────────────────────────

/// Key (or array-index) display text + color for a node. Shared by the viewer
/// and diff renderers.
fn key_parts(index: &index::JsonIndex, node: &index::Node, dark: bool) -> (String, egui::Color32) {
    if node.key_len > 0 {
        (
            format!("\"{}\"", index.key_of(node)),
            if dark { theme::KEY } else { egui::Color32::from_rgb(0, 90, 158) },
        )
    } else if node.array_index != u32::MAX {
        (
            format!("{}", node.array_index),
            if dark { theme::ARRAY_INDEX } else { egui::Color32::from_rgb(40, 120, 40) },
        )
    } else {
        (String::new(), egui::Color32::TRANSPARENT)
    }
}

/// Value display text + color for a node (containers show their child count;
/// long strings are truncated to 500 chars). Shared by the viewer and diff
/// renderers.
fn value_parts(index: &index::JsonIndex, node: &index::Node, dark: bool) -> (String, egui::Color32) {
    use index::NodeKind;
    let str_color       = if dark { theme::STRING }    else { egui::Color32::from_rgb(163, 21, 21) };
    let container_color = if dark { theme::CONTAINER } else { egui::Color32::from_rgb(100, 100, 100) };
    match node.kind {
        NodeKind::Object => (format!("{{ {} }}", node.child_count), container_color),
        NodeKind::Array  => (format!("[ {} ]",   node.child_count), container_color),
        NodeKind::String => {
            let raw = index.value_bytes(node);
            let inner = if raw.len() >= 2 { &raw[1..raw.len() - 1] } else { raw };
            let text = String::from_utf8_lossy(inner);
            // Truncate to 500 chars by byte position — no Vec<char> collect.
            let s = match text.char_indices().nth(500) {
                Some((cut, _)) => format!("\"{}…\"", &text[..cut]),
                None           => format!("\"{}\"", text),
            };
            (s, str_color)
        }
        NodeKind::Number => {
            let raw = index.value_bytes(node);
            (String::from_utf8_lossy(raw).into_owned(), if dark { theme::NUMBER } else { egui::Color32::from_rgb(9, 134, 88) })
        }
        NodeKind::Bool => {
            let raw = index.value_bytes(node);
            (String::from_utf8_lossy(raw).into_owned(), if dark { theme::BOOL } else { egui::Color32::from_rgb(0, 0, 210) })
        }
        NodeKind::Null => ("null".to_owned(), if dark { theme::NULL } else { egui::Color32::from_rgb(100, 100, 100) }),
    }
}

fn render_row(
    ui:               &mut egui::Ui,
    edit_overlay:     &std::collections::HashMap<u32, export::NodeEdit>,
    saved_overlay:    &std::collections::HashMap<u32, export::NodeEdit>,
    index:            &index::JsonIndex,
    added_items:      &[export::AddedItem],
    expanded:         &crate::index::NodeSet,
    selected:         Option<u32>,
    search_result_set:&crate::index::NodeSet,
    node_idx:         u32,
    row_h:            f32,
    key_font:         egui::FontId,
    val_font:         egui::FontId,
    multi_select:     bool,
    is_checked:       bool,
    any_checked:      bool,
    reveal:           bool,
    copy_compact:     bool,
) -> Vec<RowAction> {
    use index::NodeKind;

    // A pending (not-yet-saved) added item has no real node — fabricate one
    // with just enough fields for the geometry/rendering code below. Its
    // `value_start`/`value_end`/`key_start` stay zeroed, which makes
    // `index.value_bytes`/`index.key_of` safely return empty/"" if ever
    // called on it (they never should be, since key_len == 0 and the value
    // text is taken from `added_items` directly, not from `value_parts`).
    let is_new = export::is_added(index.nodes.len(), node_idx);
    let fallback_node;
    let node: &index::Node = if is_new {
        let item = &added_items[node_idx as usize - index.nodes.len()];
        fallback_node = index::Node {
            kind:         NodeKind::String,
            depth:        index.nodes[item.parent as usize].depth + 1,
            value_start:  0,
            value_end:    0,
            key_start:    0,
            key_len:      0,
            next_sibling: u32::MAX,
            child_count:  0,
            parent:       item.parent,
            array_index:  export::added_display_index(&index.nodes, added_items, node_idx),
        };
        &fallback_node
    } else {
        &index.nodes[node_idx as usize]
    };
    let depth        = node.depth;
    let kind         = node.kind;
    let child_count  = node.child_count;
    let is_expanded  = expanded.contains(&node_idx);
    let is_selected  = selected == Some(node_idx);
    let is_match     = search_result_set.contains(&node_idx);
    let is_container = matches!(kind, NodeKind::Object | NodeKind::Array);
    let has_children = child_count > 0;
    // The root is always expanded — no caret, no toggling.
    let can_toggle   = is_container && has_children && node_idx != index.root;

    let dark = ui.visuals().dark_mode;
    // Key text + color (the " : " separator is painted separately, in PUNCT).
    // A pending added item's fabricated node has no real key bytes, so a
    // property's typed key (`AddedItem::key`) is substituted in directly.
    let (mut key_text, mut key_color) = key_parts(index, node, dark);
    if is_new {
        if let Some(k) = &added_items[node_idx as usize - index.nodes.len()].key {
            key_text = format!("\"{}\"", k);
        }
    }
    let sep_text  = " : ";
    let sep_color = if dark { theme::PUNCT } else { egui::Color32::from_rgb(120, 120, 120) };

    // Value text + color
    let (mut value_text, mut value_color) = if is_new {
        (added_items[node_idx as usize - index.nodes.len()].raw_value.clone(), theme::NEW)
    } else {
        value_parts(index, node, dark)
    };

    // Overlay: edited nodes show their edited text. A node whose edit differs
    // from the last saved baseline is "pending" — rendered in the accent color
    // with a dirty dot; already-saved edits render normally.
    let pending = edit_overlay.get(&node_idx) != saved_overlay.get(&node_idx);
    let is_deleted = edit_overlay.get(&node_idx).map_or(false, |e| e.deleted);
    if let Some(ov) = edit_overlay.get(&node_idx) {
        if let Some(k) = &ov.key_override {
            key_text = format!("\"{}\"", k); // re-add display quotes
            if pending { key_color = theme::ACCENT; }
        }
        if let Some(v) = &ov.value_override {
            // `value_override` is stored as raw JSON text (string literals keep
            // their quotes), matching how `value_parts` renders unedited nodes.
            value_text = v.clone();
            if pending && !is_new { value_color = theme::ACCENT; }
        }
    }
    if is_new && !is_deleted {
        key_color   = theme::NEW;
        value_color = theme::NEW;
    }
    if is_deleted {
        key_color   = theme::DELETED;
        value_color = theme::DELETED;
    }
    let has_edit = pending && !is_new;

    // In multi-select mode a fixed left gutter holds the per-row checkbox; the
    // whole tree (indent guides included) shifts right by this amount.
    let checkbox_w = if multi_select { 20.0 } else { 0.0 };
    // Horizontal offset of a node at nesting level `d`. Used for both the row's
    // own indent and the per-ancestor indent guides, so they can't drift apart.
    let indent_at = |d: u16| checkbox_w + 4.0 + d as f32 * 16.0;
    let indent  = indent_at(depth);

    // Pre-compute display strings and key width (needed before allocation in both modes).
    let key_display   = bidi_reorder(&key_text);
    let value_display = bidi_reorder(&value_text);
    // Lay out each text once, with its final color, and reuse the galley for
    // both width measurement and painting (previously each string was laid
    // out twice: a throwaway measure pass plus painter.text()).
    let (key_galley, sep_galley) = if !key_text.is_empty() {
        (
            Some(ui.painter().layout_no_wrap(key_display.as_ref().to_owned(), key_font.clone(), key_color)),
            Some(ui.painter().layout_no_wrap(sep_text.to_owned(), key_font.clone(), sep_color)),
        )
    } else {
        (None, None)
    };
    let key_w = key_galley.as_ref().map_or(0.0, |g| g.rect.width());
    let sep_w = sep_galley.as_ref().map_or(0.0, |g| g.rect.width());

    // Widen the row so ScrollArea::both() can scroll horizontally.
    let val_galley = ui.painter()
        .layout_no_wrap(value_display.as_ref().to_owned(), val_font.clone(), value_color);
    let val_w = val_galley.rect.width();
    let content_w = indent + 18.0 + key_w + sep_w + val_w + 8.0;
    let row_w = content_w.max(ui.available_width());
    let (id, rect) = ui.allocate_space(egui::vec2(row_w, row_h));

    let response = ui.interact(rect, id, egui::Sense::click());
    if reveal {
        let x = ui.clip_rect().left();
        ui.scroll_to_rect(
            egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())),
            None,
        );
    }

    // Background
    if is_match {
        ui.painter().rect_filled(
            rect, 0.0,
            if dark { theme::MATCH_BG } else { egui::Color32::from_rgba_unmultiplied(255, 200, 0, 140) },
        );
    }
    if is_selected {
        if dark {
            ui.painter().rect_filled(rect, 0.0, theme::SELECTION_BG);
            // 2 px accent bar flush against the left edge of the row.
            let bar = egui::Rect::from_min_max(
                rect.left_top(),
                egui::pos2(rect.left() + 2.0, rect.bottom()),
            );
            ui.painter().rect_filled(bar, 0.0, theme::ACCENT);
        } else {
            ui.painter().rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
        }
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme::Palette::for_dark(dark).hover_bg);
    }

    let painter  = ui.painter();
    let text_col = if is_selected { ui.visuals().selection.stroke.color } else { ui.visuals().text_color() };

    // Indent guides — one 1 px vertical line per ancestor level, aligned under
    // the parent chevrons.
    if dark {
        for d in 0..depth {
            let gx = rect.left() + indent_at(d) + 8.0;
            painter.vline(gx, rect.y_range(), egui::Stroke::new(1.0_f32, theme::INDENT_GUIDE));
        }
    }

    // y position for single-line elements: centred in the first row_h band.
    let y1 = rect.top() + row_h / 2.0;

    // Checkbox gutter (multi-select mode) — fixed at the far left, not indented.
    let check_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 2.0, rect.top()),
        egui::vec2(checkbox_w, row_h),
    );
    // Leaf ("edge") nodes hold no subtree, so they get no checkbox — only
    // containers are selectable for export. The gutter width is unchanged so
    // every row stays aligned.
    let can_check = multi_select && is_container;
    if can_check {
        let glyph = if is_checked { "☑" } else { "☐" };
        let col   = if is_checked { theme::ACCENT } else if dark { theme::TEXT_FAINT } else { text_col };
        painter.text(egui::pos2(rect.left() + 4.0, y1), egui::Align2::LEFT_CENTER, glyph, val_font.clone(), col);
    }

    let mut x = rect.left() + indent;

    // Triangle region (always 16 px wide, in the first line band).
    let tri_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + indent, rect.top()),
        egui::vec2(16.0, row_h),
    );
    if can_toggle {
        let tri      = if is_expanded { "▼" } else { "▶" };
        let tri_font = egui::FontId::new((val_font.size - 3.0).max(8.0), val_font.family.clone());
        let tri_col  = if dark { theme::TEXT_FAINT } else { text_col };
        painter.text(egui::pos2(x + 2.0, y1), egui::Align2::LEFT_CENTER, tri, tri_font, tri_col);
    }
    x += 18.0;

    // Key + " : " separator (always single-line, vertically centred in the first band).
    if let Some(g) = key_galley {
        painter.galley(egui::pos2(x, y1 - g.size().y * 0.5), g, key_color);
        x += key_w;
    }
    if let Some(g) = sep_galley {
        painter.galley(egui::pos2(x, y1 - g.size().y * 0.5), g, sep_color);
        x += sep_w;
    }

    // Value — single line, vertically centred.
    painter.galley(egui::pos2(x, y1 - val_galley.size().y * 0.5), val_galley, value_color);
    if has_edit {
        let dot_x = rect.right() - 6.0;
        let dot_y = rect.top() + row_h / 2.0;
        painter.circle_filled(egui::pos2(dot_x, dot_y), 3.0, theme::ACCENT);
    }
    if is_deleted {
        let strike_x0 = rect.left() + indent + 18.0;
        let strike_x1 = strike_x0 + key_w + sep_w + val_w;
        painter.hline(strike_x0..=strike_x1, y1, egui::Stroke::new(1.5_f32, theme::DELETED));
    }

    // Collect actions
    let mut actions: Vec<RowAction> = Vec::new();
    if response.clicked() {
        let click_pos = response.interact_pointer_pos();
        // A click in the checkbox gutter toggles the multi-selection only.
        if can_check && click_pos.is_some_and(|p| check_rect.contains(p)) {
            actions.push(RowAction::ToggleCheck(node_idx));
        } else {
            actions.push(RowAction::Select(node_idx));
            // Toggle if click was on triangle
            if can_toggle {
                if let Some(click_pos) = click_pos {
                    if tri_rect.contains(click_pos) {
                        actions.push(RowAction::Toggle(node_idx));
                    }
                }
            }
        }
    }
    if response.double_clicked() {
        if can_toggle {
            // Double-click anywhere on a container toggles it.
            actions.push(RowAction::Toggle(node_idx));
        } else if !is_container {
            // Double-click on the key text edits the key; anywhere else
            // (value, separator, padding) edits the value.
            let key_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left() + indent + 18.0, rect.top()),
                egui::vec2(key_w, row_h),
            );
            let click_pos = response.interact_pointer_pos();
            if key_w > 0.0 && click_pos.is_some_and(|p| key_rect.contains(p)) {
                actions.push(RowAction::StartEditKey(node_idx));
            } else {
                actions.push(RowAction::StartEditValue(node_idx));
            }
        }
    }

    // Context menu (right-click)
    response.context_menu(|ui| {
        let n = node; // already-resolved (real or fabricated) node from above
        let is_deleted = edit_overlay.get(&node_idx).map_or(false, |e| e.deleted);
        let is_root = n.parent == u32::MAX;

        // An added item has a key only when it's a pending Object property.
        let added_key: Option<&str> = if is_new {
            added_items[node_idx as usize - index.nodes.len()].key.as_deref()
        } else {
            None
        };

        // Edit items — hidden for deleted nodes.
        if !is_deleted {
            if !is_container {
                if ui.button("Edit Value").clicked() {
                    actions.push(RowAction::StartEditValue(node_idx));
                    ui.close();
                }
            }
            if n.key_len > 0 || added_key.is_some() {
                if ui.button("Edit Key").clicked() {
                    actions.push(RowAction::StartEditKey(node_idx));
                    ui.close();
                }
            }
        }
        if kind == NodeKind::Array || kind == NodeKind::Object {
            let label = if kind == NodeKind::Array { "Add Item" } else { "Add Property" };
            if ui.button(label).clicked() {
                actions.push(RowAction::AddItem(node_idx));
                ui.close();
            }
        }
        if !is_root {
            let label = if is_deleted { "Restore" } else { "Delete" };
            if ui.button(label).clicked() {
                actions.push(RowAction::DeleteNode(node_idx));
                ui.close();
            }
        }
        if (!is_container || n.key_len > 0) || !is_root || kind == NodeKind::Array || kind == NodeKind::Object {
            ui.separator();
        }

        if ui.button("Copy Path").clicked() {
            let path = if is_new {
                let parent_path = build_path(&index.nodes, index, n.parent);
                let segment = match added_key {
                    Some(k) => path_key_segment(k),
                    None    => format!("[{}]", n.array_index),
                };
                format!("{parent_path}{segment}")
            } else {
                build_path(&index.nodes, index, node_idx)
            };
            ui.ctx().copy_text(path);
            ui.close();
        }

        // "Copy Key" only when the node actually has a key or array index
        let key_str: Option<String> = if let Some(k) = added_key {
            Some(k.to_owned())
        } else if n.key_len > 0 {
            Some(index.key_of(n).to_owned())
        } else if n.array_index != u32::MAX {
            Some(n.array_index.to_string())
        } else {
            None
        };
        if let Some(key) = key_str {
            if ui.button("Copy Key").clicked() {
                ui.ctx().copy_text(key);
                ui.close();
            }
        }

        if ui.button("Copy Value").clicked() {
            let text = if is_new {
                added_items[node_idx as usize - index.nodes.len()].raw_value.clone()
            } else if copy_compact {
                export::json_compact(index, node_idx)
            } else {
                String::from_utf8_lossy(index.value_bytes(n)).into_owned()
            };
            ui.ctx().copy_text(text);
            ui.close();
        }

        // Only while there are unsaved edits (the same condition that shows the
        // Save button): copy the value with the edit overlay applied — edited
        // keys/values substituted, deleted items excluded, and pending adds included.
        // Pending adds don't show up in `edit_overlay`/`saved_overlay` (they live
        // in `added_items`), so a document whose only unsaved change is a new
        // item must still be treated as dirty here.
        if !is_deleted && (edit_overlay != saved_overlay || !added_items.is_empty()) {
            if ui.button("Copy Modified Value").clicked() {
                let text = if copy_compact {
                    export::json_compact_with_edits(index, node_idx, edit_overlay, added_items)
                } else {
                    export::json_with_edits(index, node_idx, edit_overlay, added_items)
                        .trim_end()
                        .to_owned()
                };
                ui.ctx().copy_text(text);
                ui.close();
            }
        }

        if is_container {
            ui.menu_button("Copy as Code", |ui| {
                let root_name = if n.key_len > 0 {
                    codegen::to_pascal_case(index.key_of(n))
                } else {
                    "RootObject".to_owned()
                };
                let raw = index.value_bytes(n);
                for &lang in codegen::LANGUAGES {
                    if ui.button(lang.label()).clicked() {
                        ui.ctx().copy_text(codegen::generate(raw, lang, &root_name));
                        ui.close();
                    }
                }
            });
        }

        if is_container && has_children {
            ui.separator();
            if ui.button("Expand All").clicked() {
                actions.push(RowAction::ExpandRecursive(node_idx));
                ui.close();
            }
            if ui.button("Collapse All").clicked() {
                actions.push(RowAction::CollapseRecursive(node_idx));
                ui.close();
            }
        }

        ui.separator();
        ui.menu_button("Export", |ui| {
            // When the multi-selection is non-empty, offer to export it (pruned
            // to the closest common ancestor); otherwise export this node.
            if any_checked {
                if ui.button("Selected nodes as JSON").clicked() {
                    actions.push(RowAction::Export(ExportScope::Selection, ExportFormat::Json));
                    ui.close();
                }
                if ui.button("Selected nodes as CSV").clicked() {
                    actions.push(RowAction::Export(ExportScope::Selection, ExportFormat::Csv));
                    ui.close();
                }
                ui.separator();
            }
            // Pending added items aren't real tree nodes, so per-node export
            // doesn't apply to them.
            if !is_new {
                if ui.button("This node as JSON").clicked() {
                    actions.push(RowAction::Export(ExportScope::Node(node_idx), ExportFormat::Json));
                    ui.close();
                }
                if ui.button("This node as CSV").clicked() {
                    actions.push(RowAction::Export(ExportScope::Node(node_idx), ExportFormat::Csv));
                    ui.close();
                }
                ui.separator();
            }
            if ui.button("Whole file as JSON").clicked() {
                actions.push(RowAction::Export(ExportScope::File, ExportFormat::Json));
                ui.close();
            }
            if ui.button("Whole file as CSV").clicked() {
                actions.push(RowAction::Export(ExportScope::File, ExportFormat::Csv));
                ui.close();
            }
        });
    });

    actions
}

/// JSONPath segment for an object key: dot notation for simple identifiers,
/// bracket+quote otherwise. Shared by `build_path` and the path built for a
/// pending added object property (which has no real node to walk).
fn path_key_segment(key: &str) -> String {
    if !key.is_empty()
        && key.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        format!(".{key}")
    } else {
        format!(".[\"{key}\"]")
    }
}

/// Builds a JSONPath string like `$.store.books[2].title` for `node_idx`.
fn build_path(nodes: &[index::Node], idx_obj: &index::JsonIndex, node_idx: u32) -> String {
    let mut segments: Vec<String> = Vec::new();
    let mut cur = node_idx;
    loop {
        let node = &nodes[cur as usize];
        if node.parent == u32::MAX {
            break; // root — no segment for it
        }
        if node.key_len > 0 {
            segments.push(path_key_segment(idx_obj.key_of(node)));
        } else if node.array_index != u32::MAX {
            segments.push(format!("[{}]", node.array_index));
        }
        cur = node.parent;
    }
    segments.reverse();
    format!("${}", segments.join(""))
}

// ─── status bar ──────────────────────────────────────────────────────────────

impl App {
    fn status_bar(&mut self, ui: &mut egui::Ui) -> (Option<(ExportScope, ExportFormat)>, Option<SaveAction>) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        // 1 px top border above the bar
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0_f32, pal.border));

        if self.mode == AppMode::Compare {
            self.compare_status_bar(ui);
            return (None, None);
        }

        let mut export_req = None;
        let mut save_req: Option<SaveAction> = None;
        let mut discard_req = false;
        let mut clear_req   = false;
        let dirty    = self.is_dirty();
        let can_over = self.can_overwrite();
        ui.horizontal_centered(|ui| {
            if let Some(info) = &self.file_info {
                ui.label(
                    egui::RichText::new(format!("📄 {}", info.name)).color(pal.text_primary),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format_size(info.size_bytes)).color(pal.text_muted),
                );
                if let Some(t) = &self.tree {
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} nodes",
                            format_count(t.index.nodes.len().saturating_sub(1))
                        ))
                            .color(pal.text_faint),
                    );
                }
            }

            if self.load_rx.is_some() {
                ui.add_space(10.0);
                ui.label(format!("Loading… {:.0}%", self.load_progress * 100.0));
                ui.spinner();
            }

            if let Some(e) = &self.load_error {
                ui.add_space(10.0);
                ui.colored_label(egui::Color32::RED, format!("Error: {}", e));
                if self.load_error_ctx.is_some() {
                    if ui.small_button("Show context").clicked() {
                        self.error_ctx_open = !self.error_ctx_open;
                    }
                }
            }

            // Right-aligned: clear action, encoding, format badge, root-type badge.
            if self.file_info.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(" Clear ")
                        .on_hover_text("Unload the current document (discards unsaved changes)")
                        .clicked()
                    {
                        clear_req = true;
                    }
                    if self.tree.is_none() { return; }
                    ui.add_space(8.0);
                    let t = self.tree.as_ref().unwrap();
                    if dirty {
                        let label = if can_over { " Save Changes " } else { "● Save a Copy…" };
                        let hover = if can_over {
                            "Overwrite the original file and clear changes"
                        } else {
                            "Save the edited JSON to a new file"
                        };
                        let save_btn = egui::Button::new(
                            egui::RichText::new(label).color(egui::Color32::WHITE),
                        )
                            .fill(pal.accent);
                        if ui.add(save_btn).on_hover_text(hover).clicked() {
                            save_req = Some(if can_over { SaveAction::Overwrite } else { SaveAction::Copy });
                        }
                        // When overwrite is available, also offer a copy.
                        if can_over {
                            ui.add_space(6.0);
                            if ui
                                .button(" Save a Copy ")
                                .on_hover_text("Save the edited JSON to a new file")
                                .clicked()
                            {
                                save_req = Some(SaveAction::Copy);
                            }
                        }
                        ui.add_space(6.0);
                        if ui
                            .button(" Discard Changes ")
                            .on_hover_text("Discard all unsaved changes")
                            .clicked()
                        {
                            discard_req = true;
                        }
                        ui.add_space(8.0);
                    }
                    ui.label(egui::RichText::new("UTF-8").small().color(pal.text_faint));
                    ui.add_space(8.0);

                    let fmt = if t.index.is_ndjson { "NDJSON" } else { "JSON" };
                    ui.label(egui::RichText::new(fmt).small().color(pal.text_faint));


                    // When select mode is active and rows are checked, surface
                    // an export action for the selection right in the footer.
                    if t.multi_select && !t.checked.is_empty() {
                        ui.add_space(12.0);
                        let n = t.checked.len();
                        let json_btn = egui::Button::new(
                            egui::RichText::new("Export JSON").color(egui::Color32::WHITE),
                        )
                            .fill(pal.accent);
                        if ui.add(json_btn).clicked() {
                            export_req = Some((ExportScope::Selection, ExportFormat::Json));
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} selected",
                                format_count(n)
                            ))
                                .color(pal.text_muted),
                        );
                    }
                });
            }
        });
        if discard_req {
            self.discard_changes();
        }
        if clear_req {
            self.clear_document();
        }
        (export_req, save_req)
    }
}

// ─── export ──────────────────────────────────────────────────────────────────

impl App {
    /// Prompt for a save location, then serialize and write the export on a
    /// background thread (large documents would otherwise freeze the UI).
    /// Errors surface in `load_error` via `bg_write_rx`.
    fn export(&mut self, scope: ExportScope, fmt: ExportFormat) {
        let Some(tree) = &self.tree else { return };
        let checked: Vec<u32> = tree.checked.iter().copied().collect();
        if matches!(scope, ExportScope::Selection) && checked.is_empty() {
            return;
        }
        let index = Arc::clone(&tree.index);

        let scope_tag = match scope {
            ExportScope::File         => "",
            ExportScope::Node(_)      => "-node",
            ExportScope::Selection    => "-selection",
        };
        let stem = self
            .file_info
            .as_ref()
            .map(|f| {
                std::path::Path::new(&f.name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| f.name.clone())
            })
            .unwrap_or_else(|| "export".to_owned());
        let default_name = format!("{stem}{scope_tag}.{}", fmt.ext());

        let (filter_name, exts): (&str, &[&str]) = match fmt {
            ExportFormat::Json => ("JSON", &["json"]),
            ExportFormat::Csv => ("CSV", &["csv"]),
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter(filter_name, exts)
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };

        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_write_rx = Some(rx);
        std::thread::spawn(move || {
            let index = &*index;
            let empty = std::collections::HashSet::new();
            // Cow: the verbatim-JSON path borrows the mmap'd source directly
            // instead of copying the whole file into memory.
            let bytes: std::borrow::Cow<'_, [u8]> = match scope {
                ExportScope::File => match fmt {
                    // NDJSON has no enclosing array, so reconstruct one.
                    ExportFormat::Json if index.is_ndjson => {
                        export::json_pretty(index, index.root, &empty, None).into_bytes().into()
                    }
                    ExportFormat::Json => export::json_verbatim(index, index.root).into(),
                    ExportFormat::Csv => export::csv(index, index.root, &empty, None).into_bytes().into(),
                },
                ExportScope::Node(idx) => match fmt {
                    ExportFormat::Json => export::json_verbatim(index, idx).into(),
                    ExportFormat::Csv => export::csv(index, idx, &empty, None).into_bytes().into(),
                },
                ExportScope::Selection => {
                    let (lca, keep) = export::build_keep_set(index, &checked);
                    let sel: std::collections::HashSet<u32> = checked.into_iter().collect();
                    match fmt {
                        ExportFormat::Json => {
                            export::json_pretty(index, lca, &sel, Some(&keep)).into_bytes().into()
                        }
                        ExportFormat::Csv => export::csv(index, lca, &sel, Some(&keep)).into_bytes().into(),
                    }
                }
            };
            let res = std::fs::write(&path, &bytes)
                .map(|_| BgWriteDone::Written)
                .map_err(|e| format!("Export failed: {e}"));
            let _ = tx.send(res);
        });
    }
}

// ─── file helpers ────────────────────────────────────────────────────────────

impl App {
    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json", "jsonl", "ndjson"])
            .pick_file()
        {
            self.open_file(path);
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.file_info    = Some(FileInfo { name, size_bytes: size, path: Some(path.clone()) });
        self.tree         = None;
        self.load_error   = None;
        self.load_progress = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.edit_overlay.clear();
        self.saved_overlay.clear();
        self.editing_node = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.load_rx      = Some(loader::spawn_load(path));
    }

    /// Unload the current document and reset the viewer to its empty state.
    fn clear_document(&mut self) {
        self.file_info      = None;
        self.tree           = None;
        self.load_rx        = None;
        self.load_error     = None;
        self.load_error_ctx = None;
        self.error_ctx_open = false;
        self.load_progress  = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.search_debounce_until = None;
        self.edit_overlay.clear();
        self.saved_overlay.clear();
        self.editing_node = None;
        self.adding_item  = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Unload one Compare pane and drop the diff computed against it.
    fn clear_pane(&mut self, side: Side) {
        *self.compare.pane_mut(side) = ComparePane::default();
        self.compare.result       = None;
        self.compare.tree         = None;
        self.compare.diff_rx      = None;
        self.compare.needs_rediff = false;
    }

    /// Ask the windowing backend for the clipboard contents; they arrive as an
    /// `Event::Paste` on a following frame, which `paste_pending` routes to
    /// `open_pasted` even if a text field has focus.
    fn request_paste(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        self.paste_pending = true;
    }

    fn open_pasted(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // A file copied in Finder pastes as its path — open it like a drop.
        if let Some(path) = paste::detect_file_path(text) {
            self.open_file(path);
            return;
        }
        // Auto-detect URLs, curl commands, and fetch() calls
        if let Some(req) = url_parse::parse_request(text) {
            self.open_url_in_viewer(req);
            return;
        }
        let (data, name) = match paste::decode_jwt(text) {
            Some(decoded) => (decoded, "Pasted JWT"),
            None          => (text.as_bytes().to_vec(), "Pasted JSON"),
        };
        self.file_info    = Some(FileInfo { name: name.to_owned(), size_bytes: data.len() as u64, path: None });
        self.tree         = None;
        self.load_error   = None;
        self.load_progress = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.edit_overlay.clear();
        self.saved_overlay.clear();
        self.editing_node = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.load_rx      = Some(loader::spawn_parse(data));
    }

    fn open_url_dialog(&mut self) {
        self.url_dialog_open  = true;
        self.url_dialog_focus = true;
    }

    fn open_url_request(&mut self, req: url_parse::HttpRequest) {
        match self.mode {
            AppMode::Viewer  => self.open_url_in_viewer(req),
            AppMode::Compare => {
                let side = self.compare.active_pane;
                self.open_url_request_into_pane(side, req);
            }
        }
    }

    fn open_url_in_viewer(&mut self, req: url_parse::HttpRequest) {
        let name = url_parse::url_display_name(&req.url);
        self.file_info     = Some(FileInfo { name, size_bytes: 0, path: None });
        self.tree          = None;
        self.load_error    = None;
        self.load_progress = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.edit_overlay.clear();
        self.saved_overlay.clear();
        self.editing_node  = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.load_rx = Some(match req.curl_args {
            Some(args) => loader::spawn_exec_curl(args),
            None       => loader::spawn_fetch_url(req.url, req.method, req.headers, req.body),
        });
    }

    fn open_url_request_into_pane(&mut self, side: Side, req: url_parse::HttpRequest) {
        let name = url_parse::url_display_name(&req.url);
        let pane = self.compare.pane_mut(side);
        pane.file_info      = Some(FileInfo { name, size_bytes: 0, path: None });
        pane.index          = None;
        pane.load_error     = None;
        pane.load_error_ctx = None;
        pane.error_ctx_open = false;
        pane.load_progress  = 0.0;
        pane.load_rx = Some(match req.curl_args {
            Some(args) => loader::spawn_exec_curl(args),
            None       => loader::spawn_fetch_url(req.url, req.method, req.headers, req.body),
        });
        self.compare.active_pane = side.other();
    }

    fn kick_search(&mut self) {
        // Abort any in-flight scan — its results are stale either way.
        self.search_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        self.search_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.search_pending = None;
        self.search_debounce_until = None;
        if self.search_input.is_empty() {
            if let Some(t) = &mut self.tree {
                t.set_search_results(Vec::new());
            }
            return;
        }
        if let Some(t) = &self.tree {
            let index     = Arc::clone(&t.index);
            let query     = self.search_input.clone();
            let use_regex = t.search_use_regex;
            let cancel    = Arc::clone(&self.search_cancel);
            self.search_pending =
                Some(std::thread::spawn(move || search::search(&index, &query, use_regex, &cancel)));
        }
    }
}

// ─── editing ─────────────────────────────────────────────────────────────────

impl App {
    /// True when there are edits not yet persisted by an overwrite-save.
    /// Discard all unsaved changes: restore the overlay to its last-saved
    /// state, drop added items, and clear the edit session and undo history.
    fn discard_changes(&mut self) {
        self.edit_overlay = self.saved_overlay.clone();
        self.editing_node = None;
        if let Some(t) = &mut self.tree {
            // Dropping pending adds invalidates their synthetic ids: rebuild
            // the visible-row cache and evict them from selection/checked.
            let real_len = t.index.nodes.len() as u32;
            t.added_items.clear();
            if t.selected.is_some_and(|s| s >= real_len) {
                t.selected = Some(t.index.root);
            }
            t.checked.retain(|&id| id < real_len);
            t.refresh_visible();
        }
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    fn is_dirty(&self) -> bool {
        self.edit_overlay != self.saved_overlay
            || self.tree.as_ref().is_some_and(|t| !t.added_items.is_empty())
    }

    /// True when the open document came from a file we can overwrite in place.
    fn can_overwrite(&self) -> bool {
        self.file_info.as_ref().and_then(|f| f.path.as_ref()).is_some()
    }

    /// Toggle the deleted flag on `node_idx`. Removes the overlay entry when it
    /// becomes fully default (no key/value override, not deleted).
    fn toggle_delete(&mut self, node_idx: u32) {
        let before = self.edit_overlay.get(&node_idx).cloned();
        {
            let entry = self.edit_overlay.entry(node_idx).or_default();
            entry.deleted = !entry.deleted;
        }
        if let Some(e) = self.edit_overlay.get(&node_idx) {
            if !e.deleted && e.key_override.is_none() && e.value_override.is_none() {
                self.edit_overlay.remove(&node_idx);
            }
        }
        let after = self.edit_overlay.get(&node_idx).cloned();
        self.push_undo(node_idx, before, after);
    }

    /// Record an undoable overlay change and clear the redo stack (a fresh
    /// edit invalidates any previously undone redo history).
    fn push_undo(&mut self, node_idx: u32, before: Option<export::NodeEdit>, after: Option<export::NodeEdit>) {
        self.undo_stack.push(UndoAction::Overlay(UndoEntry { node_idx, before, after }));
        cap_undo(&mut self.undo_stack);
        self.redo_stack.clear();
    }

    /// Record an undoable "add item" action. Undo removes the item again;
    /// this relies on strict LIFO ordering (see `TreeState::remove_last_added_item`).
    fn push_undo_add(&mut self, parent: u32, key: Option<String>, raw_value: String) {
        self.undo_stack.push(UndoAction::Add { parent, key, raw_value });
        cap_undo(&mut self.undo_stack);
        self.redo_stack.clear();
    }

    fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Revert the most recent action and move it to the redo stack.
    fn undo(&mut self) {
        let Some(action) = self.undo_stack.pop() else { return };
        match &action {
            UndoAction::Overlay(entry) => {
                match &entry.before {
                    Some(edit) => { self.edit_overlay.insert(entry.node_idx, edit.clone()); }
                    None       => { self.edit_overlay.remove(&entry.node_idx); }
                }
            }
            UndoAction::Add { .. } => {
                if let Some(tree) = &mut self.tree {
                    tree.remove_last_added_item();
                }
            }
            UndoAction::Batch(entries) => {
                for entry in entries.iter().rev() {
                    match &entry.before {
                        Some(edit) => { self.edit_overlay.insert(entry.node_idx, edit.clone()); }
                        None       => { self.edit_overlay.remove(&entry.node_idx); }
                    }
                }
            }
        }
        self.editing_node = None;
        self.redo_stack.push(action);
    }

    /// Re-apply the most recently undone action.
    fn redo(&mut self) {
        let Some(action) = self.redo_stack.pop() else { return };
        match &action {
            UndoAction::Overlay(entry) => {
                match &entry.after {
                    Some(edit) => { self.edit_overlay.insert(entry.node_idx, edit.clone()); }
                    None       => { self.edit_overlay.remove(&entry.node_idx); }
                }
            }
            UndoAction::Add { parent, key, raw_value } => {
                if let Some(tree) = &mut self.tree {
                    tree.add_item(*parent, key.clone(), raw_value.clone());
                }
            }
            UndoAction::Batch(entries) => {
                for entry in entries {
                    match &entry.after {
                        Some(edit) => { self.edit_overlay.insert(entry.node_idx, edit.clone()); }
                        None       => { self.edit_overlay.remove(&entry.node_idx); }
                    }
                }
            }
        }
        self.editing_node = None;
        self.undo_stack.push(action);
    }

    /// Open the edit dialog for `node_idx`, pre-populating the buffer with the
    /// current display text (unquoted for strings).
    fn start_edit(&mut self, node_idx: u32, field: EditField) {
        let Some(tree) = &self.tree else { return };
        use index::NodeKind;

        if tree.is_added(node_idx) {
            // A pending item's value is always raw JSON text (typed via the
            // Add dialog), so editing it again is a plain passthrough — no
            // string quote-stripping. Its key (if it's an Object property) is
            // plain text, not stored in the byte arena, so it's read straight
            // from `AddedItem::key`.
            let text = match field {
                EditField::Value => self
                    .edit_overlay
                    .get(&node_idx)
                    .and_then(|e| e.value_override.clone())
                    .unwrap_or_else(|| tree.added_item(node_idx).raw_value.clone()),
                EditField::Key => self
                    .edit_overlay
                    .get(&node_idx)
                    .and_then(|e| e.key_override.clone())
                    .or_else(|| tree.added_item(node_idx).key.clone())
                    .unwrap_or_default(),
            };
            self.editing_node = Some(EditingState {
                node_idx,
                field,
                text,
                focus_requested: true,
            });
            return;
        }

        let node = &tree.index.nodes[node_idx as usize];

        let text = match field {
            EditField::Key => self
                .edit_overlay
                .get(&node_idx)
                .and_then(|e| e.key_override.clone())
                .unwrap_or_else(|| tree.index.key_of(node).to_owned()),
            EditField::Value => {
                if let Some(v) = self
                    .edit_overlay
                    .get(&node_idx)
                    .and_then(|e| e.value_override.as_deref())
                {
                    // For strings, strip the JSON quotes for the edit box.
                    if node.kind == NodeKind::String {
                        serde_json::from_str::<String>(v).unwrap_or_else(|_| v.to_owned())
                    } else {
                        v.to_owned()
                    }
                } else {
                    let raw = String::from_utf8_lossy(tree.index.value_bytes(node));
                    if node.kind == NodeKind::String {
                        // Strip outer quotes for display ("hello" → hello).
                        serde_json::from_str::<String>(&raw).unwrap_or_else(|_| raw.into_owned())
                    } else {
                        raw.into_owned()
                    }
                }
            }
        };

        self.editing_node = Some(EditingState {
            node_idx,
            field,
            text,
            focus_requested: true,
        });
    }

    /// Store the committed edit into `edit_overlay` and clear `editing_node`.
    fn commit_edit(&mut self) {
        let Some(state) = self.editing_node.take() else { return };
        let Some(tree) = &self.tree else { return };
        use index::NodeKind;

        if tree.is_added(state.node_idx) {
            let before = self.edit_overlay.get(&state.node_idx).cloned();
            let entry = self
                .edit_overlay
                .entry(state.node_idx)
                .or_insert_with(export::NodeEdit::default);
            match state.field {
                EditField::Value => entry.value_override = Some(state.text),
                EditField::Key   => entry.key_override   = Some(state.text),
            }
            let after = self.edit_overlay.get(&state.node_idx).cloned();
            self.push_undo(state.node_idx, before, after);
            return;
        }

        let node = &tree.index.nodes[state.node_idx as usize];

        let before = self.edit_overlay.get(&state.node_idx).cloned();
        let entry = self
            .edit_overlay
            .entry(state.node_idx)
            .or_insert_with(export::NodeEdit::default);
        match state.field {
            EditField::Value => {
                let raw = if node.kind == NodeKind::String {
                    // Re-encode as a JSON string literal.
                    serde_json::to_string(&state.text).unwrap_or_else(|_| {
                        format!(
                            "\"{}\"",
                            state.text.replace('\\', "\\\\").replace('"', "\\\"")
                        )
                    })
                } else {
                    state.text
                };
                entry.value_override = Some(raw);
            }
            EditField::Key => {
                entry.key_override = Some(state.text);
            }
        }
        let after = self.edit_overlay.get(&state.node_idx).cloned();
        self.push_undo(state.node_idx, before, after);
    }

    /// Apply an AI-reviewed changeset through the edit overlay so dirty
    /// tracking, saving, and undo/redo all work exactly as for manual edits.
    /// Paths are re-resolved at apply time (the index may have been reloaded
    /// since the proposal was made); the whole set is one undo unit.
    fn apply_ai_edits(&mut self, edits: Vec<ai::ProposedEdit>) {
        let Some(tree) = &self.tree else { return };
        let index = Arc::clone(&tree.index);
        let mut entries: Vec<UndoEntry> = Vec::new();
        let mut failed = 0usize;
        for edit in &edits {
            let node_idx = match ai::tools::resolve_path(&index, &edit.path) {
                Ok(n) => n,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let before = self.edit_overlay.get(&node_idx).cloned();
            let entry = self.edit_overlay.entry(node_idx).or_default();
            match &edit.action {
                ai::EditAction::SetValue(v)  => entry.value_override = Some(v.clone()),
                ai::EditAction::RenameKey(k) => entry.key_override = Some(k.clone()),
                ai::EditAction::Delete       => entry.deleted = true,
            }
            let after = self.edit_overlay.get(&node_idx).cloned();
            entries.push(UndoEntry { node_idx, before, after });
        }
        if !entries.is_empty() {
            self.undo_stack.push(UndoAction::Batch(entries));
            cap_undo(&mut self.undo_stack);
            self.redo_stack.clear();
        }
        if failed > 0 {
            self.ai.note(format!(
                "{failed} edit(s) could not be applied — their paths no longer resolve."
            ));
        }
    }

    /// Save the edited document to a new file chosen via the platform dialog.
    /// Serialization + write happen on a background thread.
    /// Does not change which file is open or clear the dirty state.
    fn save_copy(&mut self) {
        let Some(tree) = &self.tree else { return };
        let stem = self
            .file_info
            .as_ref()
            .map(|f| {
                std::path::Path::new(&f.name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| f.name.clone())
            })
            .unwrap_or_else(|| "export".to_owned());
        let default_name = format!("{stem}-copy.json");
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        let index       = Arc::clone(&tree.index);
        let overlay     = self.edit_overlay.clone();
        let added_items = tree.added_items.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_write_rx = Some(rx);
        std::thread::spawn(move || {
            let json = export::json_with_edits(&index, index.root, &overlay, &added_items);
            let res = std::fs::write(&path, json.as_bytes())
                .map(|_| BgWriteDone::Written)
                .map_err(|e| format!("Save failed: {e}"));
            let _ = tx.send(res);
        });
    }

    /// Overwrite the original file with the edited document. When the overlay
    /// contains deletions, reloads from the new file so deleted nodes
    /// disappear from the tree. For pure key/value edits, keeps the tree in
    /// place and just advances the saved baseline. Only valid for file-backed
    /// documents (those with a known path). Serialization + atomic write run
    /// on a background thread; the post-save transition happens when
    /// `bg_write_rx` reports completion.
    fn save_overwrite(&mut self) {
        let Some(path) = self.file_info.as_ref().and_then(|f| f.path.clone()) else { return };
        let Some(tree) = &self.tree else { return };
        // Deletions and pending adds both change the node structure, which
        // only a reparse (on completion) can reconcile with `edit_overlay`/`selected`/etc.
        let structural =
            self.edit_overlay.values().any(|e| e.deleted) || !tree.added_items.is_empty();
        let index       = Arc::clone(&tree.index);
        let overlay     = self.edit_overlay.clone();
        let added_items = tree.added_items.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_write_rx = Some(rx);
        std::thread::spawn(move || {
            let json = export::json_with_edits(&index, index.root, &overlay, &added_items);
            // Atomic write (temp + rename): never truncate the file the current
            // index may still be mmap'd against.
            let res = write_atomic(&path, json.as_bytes())
                .map(|_| BgWriteDone::SaveOverwrite {
                    path,
                    json_len: json.len() as u64,
                    structural,
                    snapshot: overlay,
                })
                .map_err(|e| format!("Save failed: {e}"));
            let _ = tx.send(res);
        });
    }

    /// Apply the state transition for a completed background export/save.
    fn finish_bg_write(&mut self, res: Result<BgWriteDone, String>) {
        match res {
            Ok(BgWriteDone::Written) => {}
            Ok(BgWriteDone::SaveOverwrite { path, json_len, structural, snapshot }) => {
                if structural {
                    // Reload so deleted nodes disappear and added items
                    // become real.
                    self.open_file(path);
                } else {
                    self.saved_overlay = snapshot;
                    if let Some(f) = &mut self.file_info {
                        f.size_bytes = json_len;
                    }
                }
            }
            Err(e) => self.load_error = Some(e),
        }
    }

    /// Test-only: block until an in-flight background write finishes and
    /// apply its result (the UI does this by polling in `update`).
    #[cfg(test)]
    fn wait_bg_write(&mut self) {
        if let Some(rx) = self.bg_write_rx.take() {
            let res = rx.recv().expect("background write thread died");
            self.finish_bg_write(res);
        }
    }

    /// Modal dialog for editing a single key or value. Commits to `edit_overlay`
    /// on OK/Enter; discards on Cancel/Escape/close.
    fn show_edit_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.editing_node else { return };
        let Some(tree) = &self.tree else { return };

        let title = match state.field {
            EditField::Key   => "Edit Key",
            EditField::Value => "Edit Value",
        };
        let path = if tree.is_added(state.node_idx) {
            let item = tree.added_item(state.node_idx);
            let parent_path = build_path(&tree.index.nodes, &*tree.index, item.parent);
            let segment = match &item.key {
                Some(k) => path_key_segment(k),
                None    => format!("[{}]", export::added_display_index(&tree.index.nodes, &tree.added_items, state.node_idx)),
            };
            format!("{parent_path}{segment}")
        } else {
            build_path(&tree.index.nodes, &*tree.index, state.node_idx)
        };

        let mut commit = false;
        let mut cancel = false;
        let mut open   = true;

        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .min_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
                ui.add_space(4.0);
                ui.label(egui::RichText::new(path.as_str()).monospace().small().color(pal.text_muted));
                ui.add_space(4.0);

                // egui does not reorder bidi text, so Hebrew would render
                // backwards and left-aligned. When (and only when) the text
                // contains RTL characters, lay it out in visual order and
                // right-align it; pure-LTR values take the default path.
                let has_rtl = contains_rtl(&state.text);
                let mut rtl_layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                    let visual = bidi_reorder(buf.as_str()).into_owned();
                    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        visual,
                        font_id,
                        ui.visuals().text_color(),
                    );
                    job.halign = egui::Align::RIGHT;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let mut te = egui::TextEdit::singleline(&mut state.text)
                    .desired_width(340.0)
                    .font(egui::TextStyle::Monospace);
                if has_rtl {
                    te = te.horizontal_align(egui::Align::RIGHT).layouter(&mut rtl_layouter);
                }
                let resp = ui.add(te);

                if state.focus_requested {
                    resp.request_focus();
                    state.focus_requested = false;
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked()     { commit = true; }
                    if ui.button("Cancel").clicked() { cancel = true; }
                });
            });

        // Borrows on `self.editing_node` / `self.tree` end here.
        if commit {
            self.commit_edit();
        } else if cancel || !open {
            self.editing_node = None;
        }
    }

    /// Open the "Add Item" / "Add Property" dialog for appending a new child
    /// to `parent` (an Array or Object node).
    fn start_add_item(&mut self, parent: u32) {
        self.adding_item = Some(AddingState { parent, key: String::new(), text: String::new(), focus_requested: true });
    }

    /// Append the typed value (and, for an Object parent, key) to
    /// `added_items`, select it, and record the undoable action.
    fn commit_add_item(&mut self) {
        let Some(state) = self.adding_item.take() else { return };
        let Some(tree) = &mut self.tree else { return };
        let is_object = tree.index.nodes[state.parent as usize].kind == index::NodeKind::Object;
        let key = if is_object { Some(state.key.clone()) } else { None };
        let new_id = tree.add_item(state.parent, key.clone(), state.text.clone());
        tree.selected = Some(new_id);
        tree.ensure_visible(new_id);
        self.push_undo_add(state.parent, key, state.text);
    }

    /// Modal dialog for appending a new array item or object property. The
    /// typed value must be valid JSON (e.g. `"text"`, `42`, `true`, `null`)
    /// and, for an object property, the key must be non-empty — the Add
    /// button is disabled until both hold.
    fn show_add_item_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.adding_item else { return };
        let Some(tree) = &self.tree else { return };

        let is_object = tree.index.nodes[state.parent as usize].kind == index::NodeKind::Object;
        let path = build_path(&tree.index.nodes, &*tree.index, state.parent);
        let value_valid = serde_json::from_str::<serde_json::Value>(&state.text).is_ok();
        let key_valid = !is_object || !state.key.trim().is_empty();
        let valid = value_valid && key_valid;

        let mut commit = false;
        let mut cancel = false;
        let mut open   = true;

        let title = if is_object { "Add Property" } else { "Add Item" };

        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .min_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
                ui.add_space(4.0);
                let hint = if is_object { format!("{path}.…") } else { format!("{path}[…]") };
                ui.label(egui::RichText::new(hint).monospace().small().color(pal.text_muted));
                ui.add_space(4.0);

                let mut key_lost_focus = false;
                if is_object {
                    ui.label(egui::RichText::new("Key").small().color(pal.text_muted));
                    let ke = egui::TextEdit::singleline(&mut state.key)
                        .desired_width(340.0)
                        .hint_text("property name")
                        .font(egui::TextStyle::Monospace);
                    let key_resp = ui.add(ke);
                    if state.focus_requested {
                        key_resp.request_focus();
                    }
                    key_lost_focus = key_resp.lost_focus();
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Value").small().color(pal.text_muted));
                }

                let te = egui::TextEdit::singleline(&mut state.text)
                    .desired_width(340.0)
                    .hint_text(r#"e.g. "text", 42, true, null"#)
                    .font(egui::TextStyle::Monospace);
                let resp = ui.add(te);

                if state.focus_requested && !is_object {
                    resp.request_focus();
                }
                state.focus_requested = false;

                if !state.text.is_empty() && !value_valid {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Not valid JSON").small().color(theme::DELETED));
                }
                let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (resp.lost_focus() || key_lost_focus) && enter_pressed && valid {
                    commit = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(valid, egui::Button::new("Add")).clicked() { commit = true; }
                    if ui.button("Cancel").clicked() { cancel = true; }
                });
            });

        // Borrows on `self.adding_item` / `self.tree` end here.
        if commit {
            self.commit_add_item();
        } else if cancel || !open {
            self.adding_item = None;
        }
    }
}

/// Write `bytes` to `path` atomically (sibling temp file + rename) so that an
/// existing memory map of `path` is never truncated out from under the app.
/// Bound undo history so marathon editing sessions can't grow memory without
/// limit. Oldest entries fall off first.
fn cap_undo(stack: &mut Vec<UndoAction>) {
    const UNDO_CAP: usize = 1000;
    if stack.len() > UNDO_CAP {
        let excess = stack.len() - UNDO_CAP;
        stack.drain(..excess);
    }
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document.json".to_owned());
    let tmp = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join(format!(".{file_name}.jsonviewer.tmp")),
        None      => std::path::PathBuf::from(format!(".{file_name}.jsonviewer.tmp")),
    };
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

// ─── compare mode ────────────────────────────────────────────────────────────

impl App {
    /// Switch view modes. When entering Compare for the first time, seed the
    /// left pane from the document already open in the viewer.
    fn set_mode(&mut self, mode: AppMode) {
        if mode == AppMode::Compare && self.compare.left.index.is_none() {
            if let Some(t) = &self.tree {
                self.compare.left.index     = Some(Arc::clone(&t.index));
                self.compare.left.file_info = self.file_info.clone();
                self.compare.needs_rediff   = true;
            }
        }
        self.mode = mode;
    }

    /// ⌘O / menu Open — targets the viewer or the active compare pane.
    fn open_active_dialog(&mut self) {
        match self.mode {
            AppMode::Viewer  => self.open_file_dialog(),
            AppMode::Compare => self.open_into_pane_dialog(self.compare.active_pane),
        }
    }

    fn open_into_pane_dialog(&mut self, side: Side) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json", "jsonl", "ndjson"])
            .pick_file()
        {
            self.open_file_into_pane(side, path);
        }
    }

    fn open_file_into_pane(&mut self, side: Side, path: PathBuf) {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let pane = self.compare.pane_mut(side);
        pane.file_info      = Some(FileInfo { name, size_bytes: size, path: None });
        pane.index          = None;
        pane.load_error     = None;
        pane.load_error_ctx = None;
        pane.error_ctx_open = false;
        pane.load_progress  = 0.0;
        pane.load_rx        = Some(loader::spawn_load(path));
        self.compare.active_pane = side.other();
    }

    fn open_pasted_into_pane(&mut self, side: Side, text: &str) {
        let text = text.trim();
        if text.is_empty() { return; }
        // A file copied in Finder pastes as its path — open it like a drop.
        if let Some(path) = paste::detect_file_path(text) {
            self.open_file_into_pane(side, path);
            return;
        }
        // Auto-detect URLs, curl commands, and fetch() calls
        if let Some(req) = url_parse::parse_request(text) {
            self.open_url_request_into_pane(side, req);
            return;
        }
        let (data, name) = match paste::decode_jwt(text) {
            Some(d) => (d, "Pasted JWT"),
            None    => (text.as_bytes().to_vec(), "Pasted JSON"),
        };
        let pane = self.compare.pane_mut(side);
        pane.file_info      = Some(FileInfo { name: name.to_owned(), size_bytes: data.len() as u64, path: None });
        pane.index          = None;
        pane.load_error     = None;
        pane.load_error_ctx = None;
        pane.error_ctx_open = false;
        pane.load_progress  = 0.0;
        pane.load_rx        = Some(loader::spawn_parse(data));
        self.compare.active_pane = side.other();
    }

    fn poll_pane_loader(&mut self, side: Side, ctx: &egui::Context) {
        let msg = match &self.compare.pane(side).load_rx {
            Some(rx) => rx.try_recv().ok(),
            None     => None,
        };
        let Some(msg) = msg else { return };
        let mut did_load = false;
        {
            let pane = self.compare.pane_mut(side);
            match msg {
                LoadMsg::Progress(p) => { pane.load_progress = p; }
                LoadMsg::Done(idx)   => { pane.index = Some(idx); pane.load_rx = None; did_load = true; }
                LoadMsg::Error(e, ctx) => {
                    pane.load_error     = Some(e);
                    pane.load_error_ctx = ctx;
                    pane.error_ctx_open = false;
                    pane.load_rx        = None;
                }
            }
        }
        if did_load { self.compare.needs_rediff = true; }
        ctx.request_repaint();
    }

    /// Kick off a diff on a background thread when a pane changed or an option
    /// toggled. Computing inline would block the UI thread and make the window
    /// stop responding on large documents; instead we spawn and poll for the
    /// result in `poll_diff`, showing a spinner meanwhile.
    fn recompute_diff_if_needed(&mut self) {
        if !self.compare.needs_rediff {
            return;
        }
        let (l, r) = match (&self.compare.left.index, &self.compare.right.index) {
            (Some(l), Some(r)) => (Arc::clone(l), Arc::clone(r)),
            _ => return,
        };
        self.compare.needs_rediff = false;

        let opts = self.compare.options.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(diff::diff(l, r, &opts));
        });
        // Replacing any in-flight receiver drops the stale one, so a superseded
        // diff's result is never collected.
        self.compare.diff_rx = Some(rx);
    }

    /// Collect a finished background diff and build its view tree.
    fn poll_diff(&mut self, ctx: &egui::Context) {
        let result = match &self.compare.diff_rx {
            Some(rx) => match rx.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.compare.diff_rx = None;
                    return;
                }
            },
            None => return,
        };
        let mut tree = diff::DiffTreeState::new(&result);
        tree.only_diffs = self.compare.show_only_diffs;
        tree.filter = self.compare.filter;
        tree.refresh_visible(&result);
        self.compare.result  = Some(result);
        self.compare.tree    = Some(tree);
        self.compare.diff_rx = None;
        ctx.request_repaint();
    }

    /// Collect the result of a background update check.
    fn poll_update(&mut self, ctx: &egui::Context) {
        // Poll the version-check receiver.
        if let Some(rx) = &self.update_rx {
            match rx.try_recv() {
                Ok(msg) => {
                    self.update_rx = None;
                    match msg {
                        update::UpdateMsg::Available(info) => {
                            self.update_available = Some(info);
                            ctx.request_repaint();
                        }
                        update::UpdateMsg::UpToDate => {}
                        update::UpdateMsg::Error(e) => {
                            eprintln!("update check failed: {e}");
                        }
                        update::UpdateMsg::Installed => {}
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.update_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        // Poll the install watcher receiver.
        if let Some(rx) = &self.install_watcher_rx {
            match rx.try_recv() {
                Ok(update::UpdateMsg::Installed) => {
                    update::restart_app();
                }
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.install_watcher_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Keep polling; repaint so we check again next frame.
                    ctx.request_repaint_after(std::time::Duration::from_secs(5));
                }
            }
        }
    }

    /// Parse the option text buffers (ignore-keys list, regex) into the live
    /// `DiffOptions`.
    fn recompute_options_from_raw(&mut self) {
        let c = &mut self.compare;
        c.options.ignore_keys = c.ignore_keys_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let pat = c.ignore_pattern_raw.trim();
        if pat.is_empty() {
            c.options.ignore_key_pattern = None;
            c.pattern_error = false;
        } else {
            match regex::Regex::new(pat) {
                Ok(re) => { c.options.ignore_key_pattern = Some(re); c.pattern_error = false; }
                Err(_) => { c.options.ignore_key_pattern = None; c.pattern_error = true; }
            }
        }
    }

    fn collapse_all_active(&mut self) {
        match self.mode {
            AppMode::Viewer  => { if let Some(t) = &mut self.tree { t.collapse_all(); } }
            AppMode::Compare => {
                if let (Some(r), Some(t)) = (&self.compare.result, &mut self.compare.tree) {
                    t.collapse_all(r);
                }
            }
        }
    }

    fn expand_all_active(&mut self) {
        match self.mode {
            AppMode::Viewer  => { if let Some(t) = &mut self.tree { t.expand_all(); } }
            AppMode::Compare => {
                if let (Some(r), Some(t)) = (&self.compare.result, &mut self.compare.tree) {
                    t.expand_all(r);
                }
            }
        }
    }

    fn compare_next_diff(&mut self) {
        if let (Some(r), Some(t)) = (&self.compare.result, &mut self.compare.tree) { t.next_diff(r); }
    }
    fn compare_prev_diff(&mut self) {
        if let (Some(r), Some(t)) = (&self.compare.result, &mut self.compare.tree) { t.prev_diff(r); }
    }

    fn set_diff_filter(&mut self, filter: diff::StatusFilter) {
        self.compare.filter = filter;
        if let (Some(r), Some(t)) = (&self.compare.result, &mut self.compare.tree) {
            t.set_filter(filter, r);
        }
    }

    fn set_only_diffs(&mut self, only: bool) {
        self.compare.show_only_diffs = only;
        if let (Some(r), Some(t)) = (&self.compare.result, &mut self.compare.tree) {
            t.set_only_diffs(only, r);
        }
    }

    /// Pick the drop target pane from the pointer's horizontal position.
    fn drop_side(&self, ui: &egui::Ui) -> Side {
        let center = ui.ctx().content_rect().center().x;
        let x = ui
            .input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()).map(|p| p.x))
            .unwrap_or(center);
        if x < center { Side::Left } else { Side::Right }
    }

    fn compare_options_bar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0_f32, pal.border));

        let mut changed = false;
        egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                changed |= diff_option_toggle(ui, &pal, "Aa",    "Ignore case (values & keys)", &mut self.compare.options.ignore_case);
                changed |= diff_option_toggle(ui, &pal, "[≈]",   "Ignore array order",          &mut self.compare.options.ignore_array_order);
                changed |= diff_option_toggle(ui, &pal, "∅=–",   "Treat null as missing",       &mut self.compare.options.null_equals_missing);
                changed |= diff_option_toggle(ui, &pal, "1≈\"1\"", "Type coercion",             &mut self.compare.options.type_coercion);
                changed |= diff_option_toggle(ui, &pal, "␣",     "Trim whitespace in strings",  &mut self.compare.options.trim_whitespace);

                ui.separator();
                ui.label(egui::RichText::new("ignore keys").color(pal.text_muted));
                if ui.add(egui::TextEdit::singleline(&mut self.compare.ignore_keys_raw).desired_width(130.0).hint_text("id, ts")).changed() {
                    changed = true;
                }
                ui.label(egui::RichText::new("regex").color(pal.text_muted));
                if ui.add(egui::TextEdit::singleline(&mut self.compare.ignore_pattern_raw).desired_width(110.0).hint_text("^_")).changed() {
                    changed = true;
                }
                if self.compare.pattern_error {
                    ui.colored_label(theme::DELETED, "⚠").on_hover_text("Invalid regex");
                }
            });
        });

        if changed {
            self.recompute_options_from_raw();
            self.compare.needs_rediff = true;
        }
    }

    fn compare_status_bar(&self, ui: &mut egui::Ui) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        ui.horizontal_centered(|ui| {
            fn name(p: &ComparePane) -> &str {
                p.file_info.as_ref().map(|f| f.name.as_str()).unwrap_or("—")
            }
            ui.label(egui::RichText::new(format!("◧ {}", name(&self.compare.left))).color(pal.text_primary));
            ui.label(egui::RichText::new("vs").color(pal.text_faint));
            ui.label(egui::RichText::new(format!("{} ◨", name(&self.compare.right))).color(pal.text_primary));

            if self.compare.left.load_rx.is_some() || self.compare.right.load_rx.is_some() {
                ui.spinner();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.compare.diff_rx.is_some() {
                    ui.spinner();
                    ui.label(egui::RichText::new("Comparing…").small().color(pal.text_faint));
                } else if let Some(result) = &self.compare.result {
                    let total = result.changed + result.added + result.removed;
                    if total > 0 {
                        ui.label(format!("{total} differences"));
                    }
                }
            });
        });
    }

    fn compare_headers(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |cols| {
            self.pane_header(&mut cols[0], Side::Left);
            self.pane_header(&mut cols[1], Side::Right);
        });
        // 1 px divider beneath the headers.
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), ui.min_rect().bottom(), egui::Stroke::new(1.0_f32, pal.border));
        let _ = r;
    }

    fn pane_header(&mut self, ui: &mut egui::Ui, side: Side) {
        let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
        let active = self.compare.active_pane == side;
        let (name, loading, error, has_ctx) = {
            let pane = self.compare.pane(side);
            (
                pane.file_info.as_ref().map(|f| f.name.clone()),
                pane.load_rx.is_some(),
                pane.load_error.clone(),
                pane.load_error_ctx.is_some(),
            )
        };
        let loaded = name.is_some();
        let title  = name.unwrap_or_else(|| "— no document —".to_string());

        // Reserve the whole header rect up-front and sense clicks on it, so a
        // click anywhere on the header (the area not covered by the Open / Paste
        // buttons, which are drawn on top and keep their own clicks) activates
        // the pane. The buttons are laid out inside via a child UI.
        let margin = egui::vec2(12.0, 10.0);
        let height = ui.spacing().interact_size.y + 2.0 * margin.y;
        let (rect, bg) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
        if bg.clicked() {
            self.compare.active_pane = side;
        }

        ui.painter().rect_filled(
            rect, 0.0,
            if active { pal.selection_bg } else { pal.bg_panel },
        );

        let content_rect = egui::Rect::from_min_max(rect.min + margin, rect.max - margin);
        let mut content_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        {
            let ui = &mut content_ui;
            ui.label(egui::RichText::new(format!("📄 {title}")).color(pal.text_primary));
            if loading { ui.spinner(); }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if loaded || loading {
                    if ui
                        .small_button("Clear")
                        .on_hover_text("Unload this pane")
                        .clicked()
                    {
                        self.clear_pane(side);
                    }
                } else {
                    if ui.small_button("Paste").clicked() {
                        self.compare.active_pane = side;
                        let ctx = ui.ctx().clone();
                        self.request_paste(&ctx);
                    }
                    if ui.small_button("Open").clicked() {
                        self.compare.active_pane = side;
                        self.open_into_pane_dialog(side);
                    }
                }
            });
        }

        // Error row — rendered as a normal widget after the header rect so
        // buttons inside it are properly interactive.
        if let Some(e) = error {
            ui.horizontal(|ui| {
                ui.add_space(margin.x);
                ui.colored_label(egui::Color32::RED, format!("Error: {e}"));
                if has_ctx && ui.small_button("Show context").clicked() {
                    let pane = self.compare.pane_mut(side);
                    pane.error_ctx_open = !pane.error_ctx_open;
                }
            });
        }

        // Pointer cursor across the entire header (set last so it wins over the
        // buttons' default cursor).
        if ui.rect_contains_pointer(rect) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    fn compare_panel(&mut self, ui: &mut egui::Ui) {
        let row_h    = self.settings.row_height();
        let key_font = self.settings.key_font();
        let val_font = self.settings.val_font();

        self.compare_headers(ui);

        let both = self.compare.left.index.is_some() && self.compare.right.index.is_some();
        if !both {
            let pal = theme::Palette::for_dark(ui.visuals().dark_mode);

            // Clicking the empty area below a header selects that pane, same
            // as clicking the header itself.
            let area = ui.available_rect_before_wrap();
            let (left_rect, right_rect) = area.split_left_right_at_x(area.center().x);
            for (rect, side) in [(left_rect, Side::Left), (right_rect, Side::Right)] {
                if ui.interact(rect, ui.id().with(("compare_empty_click", side as u8)), egui::Sense::click()).clicked() {
                    self.compare.active_pane = side;
                }
            }

            let avail = ui.available_height();
            ui.add_space((avail / 2.0 - 90.0).max(24.0));
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("{ } ⇄ { }").monospace().size(28.0).color(pal.text_faint));
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Load JSON into both panes to compare").size(17.0).color(pal.text_primary));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Click a pane, then:").color(pal.text_muted));
                ui.add_space(14.0);

                // A Grid always sits at the left edge of its parent, so indent
                // it by hand to keep the option list visually centered.
                let grid_w = 300.0;
                let indent = ((ui.available_width() - grid_w) / 2.0).max(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    egui::Grid::new("compare_empty_options")
                        .num_columns(2)
                        .spacing([14.0, 10.0])
                        .show(ui, |ui| {
                            let mut row = |cap: &str, desc: &str| {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    Self::keycap(ui, &pal, cap);
                                });
                                ui.label(egui::RichText::new(desc).color(pal.text_muted));
                                ui.end_row();
                            };
                            row("⌘ O", "Open a file");
                            row("⌘ V", "Paste from clipboard");
                            row("drop", "Drag a file onto the pane");
                        });
                });
            });
            return;
        }

        let compare = &mut self.compare;
        let (Some(result), Some(tree)) = (&compare.result, &mut compare.tree) else { return };

        let left  = &*result.left;
        let right = &*result.right;
        let root  = result.root;
        let num_rows = tree.visible.len();
        let scroll_to_row = tree.scroll_to_row.take();
        let reveal_row = tree.reveal_row.take();

        let avail_h   = ui.available_height();
        let row_pitch = row_h + ui.spacing().item_spacing.y;
        let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false; 2]);
        if let Some(row) = scroll_to_row {
            let y = (row as f32 * row_pitch - avail_h / 2.0 + row_h / 2.0).max(0.0);
            scroll_area = scroll_area.vertical_scroll_offset(y);
        }

        let copy_compact = self.settings.copy_compact_json;
        let mut actions: Vec<DiffRowAction> = Vec::new();
        {
            let expanded = &tree.expanded;
            let selected = tree.selected;
            let visible  = &tree.visible;
            scroll_area.show_rows(ui, row_h, num_rows, |ui, row_range| {
                for r in row_range {
                    let node_idx = visible[r];
                    let row_actions = render_diff_row(
                        ui, left, right, &result.nodes, expanded, selected, node_idx,
                        row_h, key_font.clone(), val_font.clone(), root,
                        reveal_row == Some(r), copy_compact,
                    );
                    actions.extend(row_actions);
                }
            });
        }

        for action in actions {
            match action {
                DiffRowAction::Select(n) => tree.selected = Some(n),
                DiffRowAction::Toggle(n) => tree.toggle(n, result),
            }
        }
    }
}

// ─── diff row renderer ───────────────────────────────────────────────────────

/// A small toggle button for the compare options bar. Returns `true` when
/// toggled this frame.
fn diff_option_toggle(ui: &mut egui::Ui, pal: &theme::Palette, label: &str, hover: &str, value: &mut bool) -> bool {
    let active = *value;
    let fg = if active { pal.tab_active_fg } else { pal.text_muted };
    let fill   = if active { pal.tab_active_bg } else { egui::Color32::TRANSPARENT };
    let stroke = egui::Stroke::new(1.0_f32, if active { pal.tab_active_fg } else { pal.border });
    let button = egui::Button::new(egui::RichText::new(label).color(fg))
        .frame(true)
        .fill(fill)
        .stroke(stroke);
    let resp = ui.add(button).on_hover_text(hover);
    if resp.clicked() { *value = !*value; true } else { false }
}

/// Paints one merged diff row as two columns (left | right) with a center
/// divider. Returns the actions to apply after the scroll-area borrow ends.
#[allow(clippy::too_many_arguments)]
fn render_diff_row(
    ui:        &mut egui::Ui,
    left:      &index::JsonIndex,
    right:     &index::JsonIndex,
    nodes:     &[diff::DiffNode],
    expanded:  &crate::index::NodeSet,
    selected:  Option<u32>,
    node_idx:  u32,
    row_h:     f32,
    key_font:  egui::FontId,
    val_font:  egui::FontId,
    root:      u32,
    reveal:    bool,
    copy_compact: bool,
) -> Vec<DiffRowAction> {
    use diff::DiffStatus;

    let dn          = &nodes[node_idx as usize];
    let depth       = dn.depth;
    let status      = dn.status;
    let is_expanded = expanded.contains(&node_idx);
    let is_selected = selected == Some(node_idx);
    let can_toggle  = dn.child_count > 0 && node_idx != root;
    let dark        = ui.visuals().dark_mode;

    let full_w = ui.available_width().max(1.0);
    let (id, rect) = ui.allocate_space(egui::vec2(full_w, row_h));
    let response = ui.interact(rect, id, egui::Sense::click());
    if reveal {
        let x = ui.clip_rect().left();
        ui.scroll_to_rect(
            egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())),
            None,
        );
    }

    let mid_x = rect.center().x;
    let left_cell  = egui::Rect::from_min_max(rect.left_top(), egui::pos2(mid_x, rect.bottom()));
    let right_cell = egui::Rect::from_min_max(egui::pos2(mid_x, rect.top()), rect.right_bottom());

    // Hover first, so status tints layer over it.
    if !is_selected && response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme::Palette::for_dark(dark).hover_bg);
    }

    // Per-cell status tints — skip on expanded containers (diffs are visible inside).
    let tint_status = if is_expanded && dn.child_count > 0 { DiffStatus::Unchanged } else { status };
    let (lt, rt) = match tint_status {
        DiffStatus::Removed   => (Some(theme::DIFF_REMOVED_BG), Some(theme::DIFF_EMPTY_BG)),
        DiffStatus::Added     => (Some(theme::DIFF_EMPTY_BG),   Some(theme::DIFF_ADDED_BG)),
        DiffStatus::Changed   => (Some(theme::DIFF_CHANGED_BG), Some(theme::DIFF_CHANGED_BG)),
        DiffStatus::Unchanged => (None, None),
    };
    if let Some(c) = lt { ui.painter().rect_filled(left_cell,  0.0, c); }
    if let Some(c) = rt { ui.painter().rect_filled(right_cell, 0.0, c); }

    // Selection: translucent overlay + solid accent bar (keeps tints visible).
    if is_selected {
        ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(0x3D, 0x7E, 0xFF, 36));
        let bar = egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + 2.0, rect.bottom()));
        ui.painter().rect_filled(bar, 0.0, theme::ACCENT);
    }

    // Center divider.
    ui.painter().vline(mid_x, rect.y_range(), egui::Stroke::new(1.0_f32, theme::Palette::for_dark(dark).border));

    let text_col = if is_selected { ui.visuals().selection.stroke.color } else { ui.visuals().text_color() };

    let left_caret = dn.left_idx().and_then(|li| {
        let p = ui.painter().with_clip_rect(left_cell);
        draw_diff_cell(&p, left_cell, left, &left.nodes[li as usize], depth, can_toggle, is_expanded, row_h, &key_font, &val_font, dark, text_col)
    });
    let right_caret = dn.right_idx().and_then(|ri| {
        let p = ui.painter().with_clip_rect(right_cell);
        draw_diff_cell(&p, right_cell, right, &right.nodes[ri as usize], depth, can_toggle, is_expanded, row_h, &key_font, &val_font, dark, text_col)
    });

    let mut actions = Vec::new();
    if response.clicked() {
        actions.push(DiffRowAction::Select(node_idx));
        if can_toggle {
            if let Some(p) = response.interact_pointer_pos() {
                let on_caret = left_caret.map_or(false, |r| r.contains(p))
                    || right_caret.map_or(false, |r| r.contains(p));
                if on_caret { actions.push(DiffRowAction::Toggle(node_idx)); }
            }
        }
    }
    if response.double_clicked() && can_toggle {
        actions.push(DiffRowAction::Toggle(node_idx));
    }

    response.context_menu(|ui| {
        if let Some(li) = dn.left_idx() {
            if ui.button("Copy Left Value").clicked() {
                let text = if copy_compact {
                    export::json_compact(left, li)
                } else {
                    String::from_utf8_lossy(left.value_bytes(&left.nodes[li as usize])).into_owned()
                };
                ui.ctx().copy_text(text);
                ui.close();
            }
        }
        if let Some(ri) = dn.right_idx() {
            if ui.button("Copy Right Value").clicked() {
                let text = if copy_compact {
                    export::json_compact(right, ri)
                } else {
                    String::from_utf8_lossy(right.value_bytes(&right.nodes[ri as usize])).into_owned()
                };
                ui.ctx().copy_text(text);
                ui.close();
            }
        }
        if ui.button("Copy Path").clicked() {
            let (idx, n) = match (dn.left_idx(), dn.right_idx()) {
                (Some(li), _) => (left, li),
                (_, Some(ri)) => (right, ri),
                _             => (left, 0),
            };
            ui.ctx().copy_text(build_path(&idx.nodes, idx, n));
            ui.close();
        }
    });

    actions
}

/// Draws the [indent][caret][key][value] content of one diff cell, clipped to
/// `cell`. Returns the caret hit-rect when a toggle caret was drawn.
#[allow(clippy::too_many_arguments)]
fn draw_diff_cell(
    painter:     &egui::Painter,
    cell:        egui::Rect,
    index:       &index::JsonIndex,
    node:        &index::Node,
    depth:       u16,
    show_caret:  bool,
    is_expanded: bool,
    row_h:       f32,
    key_font:    &egui::FontId,
    val_font:    &egui::FontId,
    dark:        bool,
    text_col:    egui::Color32,
) -> Option<egui::Rect> {
    let (key_text, key_color)     = key_parts(index, node, dark);
    let (value_text, value_color) = value_parts(index, node, dark);
    let sep_text  = " : ";
    let sep_color = if dark { theme::PUNCT } else { egui::Color32::from_rgb(120, 120, 120) };
    let key_display   = bidi_reorder(&key_text);
    let value_display = bidi_reorder(&value_text);

    let indent = 4.0 + depth as f32 * 16.0;
    let y1 = cell.top() + row_h / 2.0;

    if dark {
        for d in 0..depth {
            let gx = cell.left() + 4.0 + d as f32 * 16.0 + 8.0;
            painter.vline(gx, cell.y_range(), egui::Stroke::new(1.0_f32, theme::INDENT_GUIDE));
        }
    }

    let mut x = cell.left() + indent;
    let mut caret_rect = None;
    if show_caret {
        let tri      = if is_expanded { "▼" } else { "▶" };
        let tri_font = egui::FontId::new((val_font.size - 3.0).max(8.0), val_font.family.clone());
        let tri_col  = if dark { theme::TEXT_FAINT } else { text_col };
        painter.text(egui::pos2(x + 2.0, y1), egui::Align2::LEFT_CENTER, tri, tri_font, tri_col);
        caret_rect = Some(egui::Rect::from_min_size(egui::pos2(x, cell.top()), egui::vec2(16.0, row_h)));
    }
    x += 18.0;

    if !key_text.is_empty() {
        // Single layout per text: the galley measures and paints.
        let kg = painter.layout_no_wrap(key_display.as_ref().to_owned(), key_font.clone(), key_color);
        let kw = kg.rect.width();
        painter.galley(egui::pos2(x, y1 - kg.size().y * 0.5), kg, key_color);
        x += kw;
        let sg = painter.layout_no_wrap(sep_text.to_owned(), key_font.clone(), sep_color);
        let sw = sg.rect.width();
        painter.galley(egui::pos2(x, y1 - sg.size().y * 0.5), sg, sep_color);
        x += sw;
    }
    let vg = painter.layout_no_wrap(value_display.as_ref().to_owned(), val_font.clone(), value_color);
    painter.galley(egui::pos2(x, y1 - vg.size().y * 0.5), vg, value_color);

    caret_rect
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn format_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_size(n: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    const KB: u64 = 1 << 10;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use crate::index::{JsonData, JsonIndex};
    use std::sync::Arc;

    fn make_tree(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let (nodes, root, is_ndjson) =
            crate::parser::parse_bytes(&data, &mut |_| {}).unwrap();
        Arc::new(JsonIndex {
            data: JsonData::Memory(data),
            nodes,
            root,
            is_ndjson,
        })
    }

    /// Walk to the node at a sequence of object keys / array indices from root.
    fn nav(index: &JsonIndex, path: &[&str]) -> u32 {
        let mut cur = index.root;
        for seg in path {
            let mut c = index.first_child(cur);
            let mut found = None;
            while c != u32::MAX {
                let cn = &index.nodes[c as usize];
                let matches = if let Ok(i) = seg.parse::<u32>() {
                    cn.array_index == i
                } else {
                    index.key_of(cn) == *seg
                };
                if matches {
                    found = Some(c);
                    break;
                }
                c = cn.next_sibling;
            }
            cur = found.unwrap_or_else(|| panic!("path segment {seg:?} not found"));
        }
        cur
    }

    fn app_with(json: &str) -> App {
        let mut app = App::default();
        app.tree = Some(TreeState::new(make_tree(json)));
        app
    }

    #[test]
    fn start_edit_strips_quotes_for_string_value() {
        let mut app = app_with(r#"{"name": "Alice"}"#);
        let name = nav(&app.tree.as_ref().unwrap().index, &["name"]);
        app.start_edit(name, EditField::Value);
        // The edit buffer shows the decoded string, without JSON quotes.
        assert_eq!(app.editing_node.as_ref().unwrap().text, "Alice");
    }

    #[test]
    fn commit_string_value_reencodes_and_serializes() {
        let mut app = app_with(r#"{"name": "Alice", "age": 30}"#);
        let name = nav(&app.tree.as_ref().unwrap().index, &["name"]);
        app.start_edit(name, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "Bob".to_owned();
        app.commit_edit();

        assert!(app.editing_node.is_none());
        assert_eq!(
            app.edit_overlay.get(&name).unwrap().value_override.as_deref(),
            Some("\"Bob\"")
        );

        let t = app.tree.as_ref().unwrap();
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"name": "Bob", "age": 30}));
    }

    #[test]
    fn commit_string_value_escapes_embedded_quote() {
        let mut app = app_with(r#"{"s": "x"}"#);
        let s = nav(&app.tree.as_ref().unwrap().index, &["s"]);
        app.start_edit(s, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "a\"b".to_owned();
        app.commit_edit();

        let t = app.tree.as_ref().unwrap();
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"s": "a\"b"}));
    }

    #[test]
    fn commit_number_value_is_emitted_verbatim() {
        let mut app = app_with(r#"{"age": 30}"#);
        let age = nav(&app.tree.as_ref().unwrap().index, &["age"]);
        app.start_edit(age, EditField::Value);
        // Numbers are edited as their raw text (no quote stripping).
        assert_eq!(app.editing_node.as_ref().unwrap().text, "30");
        app.editing_node.as_mut().unwrap().text = "99".to_owned();
        app.commit_edit();

        let t = app.tree.as_ref().unwrap();
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"age": 99}));
    }

    #[test]
    fn commit_key_edit_serializes() {
        let mut app = app_with(r#"{"age": 30}"#);
        let age = nav(&app.tree.as_ref().unwrap().index, &["age"]);
        app.start_edit(age, EditField::Key);
        assert_eq!(app.editing_node.as_ref().unwrap().text, "age");
        app.editing_node.as_mut().unwrap().text = "years".to_owned();
        app.commit_edit();

        let t = app.tree.as_ref().unwrap();
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"years": 30}));
    }

    #[test]
    fn reediting_a_value_reads_back_the_override() {
        let mut app = app_with(r#"{"name": "Alice"}"#);
        let name = nav(&app.tree.as_ref().unwrap().index, &["name"]);
        app.start_edit(name, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "Bob".to_owned();
        app.commit_edit();
        // Re-open: the buffer should show the previously committed value, unquoted.
        app.start_edit(name, EditField::Value);
        assert_eq!(app.editing_node.as_ref().unwrap().text, "Bob");
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("jsonviewer-test-{tag}-{nanos}.json"));
        p
    }

    fn app_with_file(json: &str) -> (App, std::path::PathBuf) {
        let path = temp_path("doc");
        std::fs::write(&path, json).unwrap();
        let mut app = App::default();
        app.tree = Some(TreeState::new(make_tree(json)));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        app.file_info = Some(FileInfo {
            name,
            size_bytes: json.len() as u64,
            path: Some(path.clone()),
        });
        (app, path)
    }

    #[test]
    fn overwrite_writes_edits_and_clears_dirty() {
        let (mut app, path) = app_with_file(r#"{"name": "Alice", "age": 30}"#);
        assert!(!app.is_dirty());
        assert!(app.can_overwrite());

        let name = nav(&app.tree.as_ref().unwrap().index, &["name"]);
        app.start_edit(name, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "Bob".to_owned();
        app.commit_edit();
        assert!(app.is_dirty());

        app.save_overwrite();
        app.wait_bg_write();
        assert!(!app.is_dirty(), "overwrite must clear the dirty state");

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!({"name": "Bob", "age": 30}));
        // Overlay retained (still displayed) but matches the saved baseline.
        assert_eq!(app.edit_overlay.get(&name), app.saved_overlay.get(&name));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reedit_after_overwrite_is_dirty_again() {
        let (mut app, path) = app_with_file(r#"{"name": "Alice", "age": 30}"#);
        let name = nav(&app.tree.as_ref().unwrap().index, &["name"]);
        let age  = nav(&app.tree.as_ref().unwrap().index, &["age"]);

        app.start_edit(name, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "Bob".to_owned();
        app.commit_edit();
        app.save_overwrite();
        app.wait_bg_write();
        assert!(!app.is_dirty());

        app.start_edit(age, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "31".to_owned();
        app.commit_edit();
        assert!(app.is_dirty(), "a new edit after save is dirty again");
        // The previously-saved node is no longer pending; the new one is.
        assert_eq!(app.edit_overlay.get(&name), app.saved_overlay.get(&name));
        assert_ne!(app.edit_overlay.get(&age), app.saved_overlay.get(&age));

        app.save_overwrite();
        app.wait_bg_write();
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!({"name": "Bob", "age": 31}));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn revert_restores_saved_baseline() {
        let (mut app, path) = app_with_file(r#"{"name": "Alice"}"#);
        let name = nav(&app.tree.as_ref().unwrap().index, &["name"]);

        app.start_edit(name, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "Bob".to_owned();
        app.commit_edit();
        app.save_overwrite(); // baseline = {name: "Bob"}
        app.wait_bg_write();

        app.start_edit(name, EditField::Value);
        app.editing_node.as_mut().unwrap().text = "Carol".to_owned();
        app.commit_edit();
        assert!(app.is_dirty());

        // Revert (as the menu does) discards unsaved changes to the baseline.
        app.edit_overlay = app.saved_overlay.clone();
        assert!(!app.is_dirty());
        assert_eq!(
            app.edit_overlay.get(&name).unwrap().value_override.as_deref(),
            Some("\"Bob\"")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn discard_after_add_drops_stale_added_ids() {
        let (mut app, path) = app_with_file(r#"{"items": [1, 2]}"#);
        let items = nav(&app.tree.as_ref().unwrap().index, &["items"]);

        let t = app.tree.as_mut().unwrap();
        let new_id = t.add_item(items, None, "3".to_owned());
        t.selected = Some(new_id);
        t.checked.insert(new_id);
        assert!(app.is_dirty());

        app.discard_changes();
        assert!(!app.is_dirty());

        let t = app.tree.as_ref().unwrap();
        let real_len = t.index.nodes.len() as u32;
        assert!(t.added_items.is_empty());
        assert!(t.visible.iter().all(|&id| id < real_len),
                "visible cache must not retain synthetic added ids");
        assert!(t.selected.is_some_and(|s| s < real_len));
        assert!(t.checked.iter().all(|&id| id < real_len));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pasted_document_cannot_overwrite() {
        let app = app_with(r#"{"a": 1}"#); // no file path
        assert!(!app.can_overwrite());

        let (app2, path) = app_with_file(r#"{"a": 1}"#);
        assert!(app2.can_overwrite());
        let _ = std::fs::remove_file(&path);
    }

    // ── add item ─────────────────────────────────────────────────────────────

    #[test]
    fn add_item_marks_dirty_and_selects_new_row() {
        let mut app = app_with(r#"[1, 2]"#);
        assert!(!app.is_dirty());
        let root = app.tree.as_ref().unwrap().index.root;

        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().text = "3".to_owned();
        app.commit_add_item();

        assert!(app.adding_item.is_none());
        assert!(app.is_dirty());
        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items.len(), 1);
        assert_eq!(t.added_items[0].raw_value, "3");
        assert!(t.selected.is_some_and(|s| t.is_added(s)));
    }

    #[test]
    fn undo_add_item_removes_it_and_redo_restores_it() {
        let mut app = app_with(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().text = "2".to_owned();
        app.commit_add_item();
        assert!(app.is_dirty());

        app.undo();
        assert!(app.tree.as_ref().unwrap().added_items.is_empty());
        assert!(!app.is_dirty());

        app.redo();
        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items.len(), 1);
        assert_eq!(t.added_items[0].raw_value, "2");
    }

    #[test]
    fn added_item_appears_in_saved_output() {
        let (mut app, path) = app_with_file(r#"[1, 2]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().text = "3".to_owned();
        app.commit_add_item();

        app.save_overwrite();
        app.wait_bg_write();
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!([1, 2, 3]));
        // A structural change (add) triggers `open_file`, which kicks off an
        // async reload (unpolled here) so the pending item becomes a real,
        // saved node — no longer "unsaved".
        assert!(app.tree.is_none());
        assert!(app.load_rx.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn add_property_appends_keyed_item_to_object() {
        let mut app = app_with(r#"{"a": 1}"#);
        let root = app.tree.as_ref().unwrap().index.root;

        app.start_add_item(root);
        {
            let state = app.adding_item.as_mut().unwrap();
            state.key = "b".to_owned();
            state.text = "2".to_owned();
        }
        app.commit_add_item();

        assert!(app.adding_item.is_none());
        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items.len(), 1);
        assert_eq!(t.added_items[0].key.as_deref(), Some("b"));
        assert_eq!(t.added_items[0].raw_value, "2");

        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &t.added_items);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1, "b": 2}));
    }

    #[test]
    fn undo_add_property_removes_it_and_redo_restores_key() {
        let mut app = app_with(r#"{"a": 1}"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        {
            let state = app.adding_item.as_mut().unwrap();
            state.key = "b".to_owned();
            state.text = "2".to_owned();
        }
        app.commit_add_item();

        app.undo();
        assert!(app.tree.as_ref().unwrap().added_items.is_empty());

        app.redo();
        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items[0].key.as_deref(), Some("b"));
        assert_eq!(t.added_items[0].raw_value, "2");
    }

    #[test]
    fn deleting_an_added_item_excludes_it_from_export() {
        let mut app = app_with(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().text = "2".to_owned();
        app.commit_add_item();
        let new_id = app.tree.as_ref().unwrap().selected.unwrap();

        app.toggle_delete(new_id);
        let t = app.tree.as_ref().unwrap();
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &t.added_items);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([1]));
    }
}
