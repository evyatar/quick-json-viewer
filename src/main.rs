mod diff;
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
    if s.chars().all(|c| {
        let cat = unicode_bidi::bidi_class(c);
        !matches!(
            cat,
            unicode_bidi::BidiClass::R
                | unicode_bidi::BidiClass::AL
                | unicode_bidi::BidiClass::RLE
                | unicode_bidi::BidiClass::RLO
                | unicode_bidi::BidiClass::RLI
        )
    }) {
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

use loader::LoadMsg;
use settings::{Settings, show_settings_window};
use tree::TreeState;

// ─── row actions ─────────────────────────────────────────────────────────────

enum RowAction {
    Select(u32),
    Toggle(u32),
    ExpandRecursive(u32),
    CollapseRecursive(u32),
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
}

/// One side of the Compare view — an independently-loaded document.
#[derive(Default)]
struct ComparePane {
    index:         Option<Arc<index::JsonIndex>>,
    load_rx:       Option<std::sync::mpsc::Receiver<LoadMsg>>,
    load_progress: f32,
    load_error:    Option<String>,
    file_info:     Option<FileInfo>,
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
}

impl Default for Side {
    fn default() -> Self { Side::Left }
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
    tree:           Option<TreeState>,
    search_input:   String,
    search_pending: Option<std::thread::JoinHandle<Vec<u32>>>,
    file_info:      Option<FileInfo>,
    focus_search:   bool,
    paste_pending:  bool,
    settings:       Settings,
    settings_open:  bool,
    help_open:      bool,
    search_help_open: bool,
    about_open:     bool,
    type_ahead:     String,
    type_ahead_time: f64,
    mode:           AppMode,
    compare:        CompareState,
    #[cfg(target_os = "macos")]
    menu_installed: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            load_rx:        None,
            load_progress:  0.0,
            load_error:     None,
            tree:           None,
            search_input:   String::new(),
            search_pending: None,
            file_info:      None,
            focus_search:    false,
            paste_pending:   false,
            settings:        Settings::default(),
            settings_open:   false,
            help_open:       false,
            search_help_open: false,
            about_open:      false,
            type_ahead:      String::new(),
            type_ahead_time: 0.0,
            mode:            AppMode::Viewer,
            compare:         CompareState::default(),
            #[cfg(target_os = "macos")]
            menu_installed: false,
        }
    }
}

// ─── eframe entry point ──────────────────────────────────────────────────────

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
        // ── 0. Apply settings ──
        let prefer_dark = ui.ctx().global_style().visuals.dark_mode;
        self.settings.apply_theme(ui.ctx(), prefer_dark);
        self.settings.apply_fonts(ui.ctx());
        // Settings dialog (rendered over everything)
        {
            let open = &mut self.settings_open;
            let settings = &mut self.settings;
            show_settings_window(settings, ui.ctx(), open);
        }
        self.show_help_window(ui.ctx());
        self.show_search_help_window(ui.ctx());
        self.show_about_window(ui.ctx());

        // ── macOS native menu bar (installed once, actions polled every frame) ──
        #[cfg(target_os = "macos")]
        {
            if !self.menu_installed {
                macos_menu::install(ui.ctx());
                self.menu_installed = true;
            }
            let acts = macos_menu::take_actions();
            if acts & macos_menu::ACT_OPEN_FILE    != 0 { self.open_active_dialog(); }
            if acts & macos_menu::ACT_PASTE        != 0 { self.request_paste(ui.ctx()); }
            if acts & macos_menu::ACT_SETTINGS     != 0 { self.settings_open = true; }
            if acts & macos_menu::ACT_FOCUS_SEARCH != 0 { self.focus_search = true; }
            if acts & macos_menu::ACT_COLLAPSE_ALL != 0 { self.collapse_all_active(); }
            if acts & macos_menu::ACT_EXPAND_ALL   != 0 { self.expand_all_active(); }
            if acts & macos_menu::ACT_HELP         != 0 { self.help_open  = true; }
            if acts & macos_menu::ACT_SEARCH_SYNTAX != 0 { self.search_help_open = true; }
            if acts & macos_menu::ACT_ABOUT        != 0 { self.about_open = true; }
            if let Some(path) = macos_menu::take_open_file() { self.open_file(path); }
        }

        // ── 1. Poll background loader ──
        if let Some(rx) = &self.load_rx {
            match rx.try_recv() {
                Ok(LoadMsg::Progress(p)) => {
                    self.load_progress = p;
                    ui.ctx().request_repaint();
                }
                Ok(LoadMsg::Done(idx)) => {
                    self.tree = Some(TreeState::new(idx));
                    self.load_rx = None;
                }
                Ok(LoadMsg::Error(e)) => {
                    self.load_error = Some(e);
                    self.load_rx = None;
                }
                Err(_) => {}
            }
        }

        // ── 1b. Poll the two Compare-pane loaders + (re)compute the diff ──
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

        // ── 2. Poll background search ──
        let search_done = self
            .search_pending
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false);
        if search_done {
            let results = self.search_pending.take().unwrap().join().unwrap_or_default();
            if let Some(t) = &mut self.tree {
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
                match self.mode {
                    AppMode::Viewer  => self.open_pasted(&text),
                    AppMode::Compare => {
                        let side = self.compare.active_pane;
                        self.open_pasted_into_pane(side, &text);
                    }
                }
            }
        }

        // ── 4. Keyboard shortcuts ──
        let (cmd_o, cmd_f, cmd_comma, arrow_up, arrow_down, arrow_left, arrow_right,
             cmd_g, cmd_shift_g, opt_c, opt_x,
             page_up, page_down, home, end) =
            ui.input(|i| {
                let cmd   = i.modifiers.command;
                let shift = i.modifiers.shift;
                let alt   = i.modifiers.alt;
                let none  = !cmd && !shift && !alt;
                (
                    cmd && i.key_pressed(egui::Key::O),
                    cmd && i.key_pressed(egui::Key::F),
                    cmd && i.key_pressed(egui::Key::Comma),
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
                )
            });

        if cmd_o      { self.open_active_dialog(); }
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
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                self.toolbar(ui);
            });

        if self.mode == AppMode::Viewer && self.settings.show_breadcrumbs && self.tree.is_some() {
            egui::Panel::top("breadcrumbs")
                .exact_size(self.settings.font_size + 14.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::BG_BREADCRUMBS)
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
                        .fill(theme::BG_BREADCRUMBS)
                        .inner_margin(egui::Margin::symmetric(10, 0)),
                )
                .show_inside(ui, |ui| {
                    self.compare_options_bar(ui);
                });
        }

        egui::Panel::bottom("statusbar")
            .exact_size(26.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                self.status_bar(ui);
            });

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
                if ui.add(egui::Button::new("Settings").shortcut_text("⌘,")).clicked() {
                    ui.close();
                    self.settings_open = true;
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

                let row = |ui: &mut egui::Ui, key: &str, desc: &str| {
                    ui.label(egui::RichText::new(key).monospace().strong());
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
                        row(ui, "⌘ V",       "Paste JSON / JWT from clipboard");
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

    fn show_about_window(&mut self, ctx: &egui::Context) {
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
}

// ─── toolbar ─────────────────────────────────────────────────────────────────

impl App {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
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
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::BORDER));
    }

    fn mode_tabs(&mut self, ui: &mut egui::Ui) {
        for (label, mode) in [("Viewer", AppMode::Viewer), ("Compare", AppMode::Compare)] {
            let active = self.mode == mode;
            let color = if active { theme::ACCENT } else { theme::TEXT_MUTED };
            if ui
                .selectable_label(active, egui::RichText::new(label).strong().color(color))
                .clicked()
            {
                self.set_mode(mode);
            }
        }
    }

    fn viewer_toolbar(&mut self, ui: &mut egui::Ui) {
        if ui.button("Open File  ⌘O").clicked() {
            self.open_file_dialog();
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
                let buttons: f32 = [".*", "?", "▲", "▼", "⚙"]
                    .iter()
                    .map(|s| text_w(s) + pad + gap)
                    .sum();
                // pill chrome: inner margins, icon, clear button, item gaps, stroke
                let pill = 16.0 + text_w("🔍") + 4.0 + 16.0 + 4.0 + 2.0;
                (ui.available_width() - buttons - counter - pill - gap).clamp(80.0, 260.0)
            };

            // Search pill — rounded container holding the search field
            egui::Frame::new()
                .fill(theme::BG_SEARCH)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;

                        ui.label(egui::RichText::new("🔍").color(theme::TEXT_MUTED));

                        let resp = {
                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                            let color   = ui.visuals().text_color();
                            let mut layouter = move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                                let display = bidi_reorder(text.as_str());
                                let mut job = egui::text::LayoutJob::simple(
                                    display.into_owned(),
                                    font_id.clone(),
                                    color,
                                    wrap_width,
                                );
                                job.wrap.max_rows = 1;
                                ui.painter().layout_job(job)
                            };
                            let te = egui::TextEdit::singleline(&mut self.search_input)
                                .hint_text("Search…  (key:id, age > 30)")
                                .desired_width(search_width)
                                .frame(egui::Frame::NONE)
                                .layouter(&mut layouter);
                            ui.add(te)
                        };
                        if resp.changed() {
                            self.kick_search();
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
                            let stroke = egui::Stroke::new(1.5, color);
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
            let re_color = if use_re { theme::ACCENT } else { theme::TEXT_MUTED };
            let mut re = use_re;
            if ui
                .selectable_label(re, egui::RichText::new(".*").monospace().color(re_color))
                .clicked()
            {
                re = !re;
                if let Some(t) = &mut self.tree {
                    t.search_use_regex = re;
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
                            .color(theme::TEXT_MUTED),
                    );
                }
            }
    }

    fn compare_toolbar(&mut self, ui: &mut egui::Ui) {
        // Summary of the current diff.
        if let Some(result) = &self.compare.result {
            let badge = |ui: &mut egui::Ui, n: usize, label: &str, color: egui::Color32| {
                ui.label(egui::RichText::new(format!("{n} {label}")).color(color));
            };
            badge(ui, result.changed, "changed", egui::Color32::from_rgb(0xE3, 0xB3, 0x41));
            badge(ui, result.added,   "added",   egui::Color32::from_rgb(0x3F, 0xB9, 0x50));
            badge(ui, result.removed, "removed", egui::Color32::from_rgb(0xE5, 0x53, 0x4B));
        } else {
            ui.label(egui::RichText::new("Load both panes to compare").color(theme::TEXT_MUTED));
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
        let color = if only { theme::ACCENT } else { theme::TEXT_MUTED };
        if ui
            .selectable_label(only, egui::RichText::new("diffs only").color(color))
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
        // 1 px bottom border under the strip
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::BORDER));

        let font_size = self.settings.font_size - 1.0;

        let (index, sel) = {
            let Some(tree) = &self.tree else { return };
            let Some(sel) = tree.selected else { return };
            (Arc::clone(&tree.index), sel)
        };

        // Ancestor chain, root first.
        let mut chain: Vec<u32> = Vec::new();
        let mut cur = sel;
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
                                    .color(theme::TEXT_FAINT),
                            );
                        }
                        let node = &index.nodes[node_idx as usize];
                        let label: String = if node.parent == u32::MAX {
                            "root".to_owned()
                        } else if node.key_len > 0 {
                            index.key_of(node).to_owned()
                        } else if node.array_index != u32::MAX {
                            format!("[{}]", node.array_index)
                        } else {
                            "\"\"".to_owned()
                        };
                        let display = bidi_reorder(&label).into_owned();
                        let is_last = i + 1 == chain.len();
                        let text = egui::RichText::new(display)
                            .monospace()
                            .size(font_size)
                            .color(if is_last { theme::KEY } else { theme::TEXT_MUTED });
                        let resp = ui
                            .selectable_label(false, text)
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            jump_to = Some(node_idx);
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Copy Path").clicked() {
                                ui.ctx().copy_text(build_path(&index.nodes, &index, node_idx));
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

impl App {
    fn tree_panel(&mut self, ui: &mut egui::Ui) {
        if self.tree.is_none() {
            ui.centered_and_justified(|ui| {
                if self.load_rx.is_some() {
                    ui.spinner();
                } else {
                    ui.label(
                        egui::RichText::new("Open a JSON file to get started\n(⌘O, drag-and-drop, or ⌘V to paste JSON / JWT)")
                            .color(theme::TEXT_MUTED),
                    );
                }
            });
            return;
        }

        let row_h    = self.settings.row_height();
        let key_font = self.settings.key_font();
        let val_font = self.settings.val_font();

        let tree = self.tree.as_mut().unwrap();
        let num_rows = tree.visible.len();
        let scroll_to_row = tree.scroll_to_row.take();

        let mut actions: Vec<RowAction> = Vec::new();

        // Borrow individual fields so the closure can hold them immutably
        // while `actions` is mutably extended outside.
        {
            let index          = &*tree.index;
            let expanded       = &tree.expanded;
            let search_res_set = &tree.search_result_set;
            let visible        = &tree.visible;
            let selected       = tree.selected;

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
                        ui, index, expanded, selected, search_res_set, node_idx,
                        row_h, key_font.clone(), val_font.clone(),
                    );
                    actions.extend(row_actions);
                }
            });
        }

        // Apply actions after borrows released
        for action in actions {
            match action {
                RowAction::Select(n)           => { tree.selected = Some(n); }
                RowAction::Toggle(n)           => { tree.toggle(n); }
                RowAction::ExpandRecursive(n)   => { tree.expand_recursive(n); }
                RowAction::CollapseRecursive(n) => { tree.collapse_recursive(n); }
            }
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
            let chars: Vec<char> = String::from_utf8_lossy(inner).chars().take(501).collect();
            let s: String = if chars.len() > 500 {
                let t: String = chars[..500].iter().collect();
                format!("{}…", t)
            } else {
                chars.into_iter().collect()
            };
            (format!("\"{}\"", s), str_color)
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
    index:            &index::JsonIndex,
    expanded:         &std::collections::HashSet<u32>,
    selected:         Option<u32>,
    search_result_set:&std::collections::HashSet<u32>,
    node_idx:         u32,
    row_h:            f32,
    key_font:         egui::FontId,
    val_font:         egui::FontId,
) -> Vec<RowAction> {
    use index::NodeKind;

    let node = &index.nodes[node_idx as usize];
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
    // Key text + color (the " : " separator is painted separately, in PUNCT)
    let (key_text, key_color) = key_parts(index, node, dark);
    let sep_text  = " : ";
    let sep_color = if dark { theme::PUNCT } else { egui::Color32::from_rgb(120, 120, 120) };

    // Value text + color
    let (value_text, value_color) = value_parts(index, node, dark);

    let indent  = 4.0 + depth as f32 * 16.0;

    // Pre-compute display strings and key width (needed before allocation in both modes).
    let key_display   = bidi_reorder(&key_text);
    let value_display = bidi_reorder(&value_text);
    let (key_w, sep_w) = if !key_text.is_empty() {
        let kw = ui.painter()
            .layout_no_wrap(key_display.as_ref().to_owned(), key_font.clone(), egui::Color32::BLACK)
            .rect.width();
        let sw = ui.painter()
            .layout_no_wrap(sep_text.to_owned(), key_font.clone(), egui::Color32::BLACK)
            .rect.width();
        (kw, sw)
    } else {
        (0.0, 0.0)
    };

    // Widen the row so ScrollArea::both() can scroll horizontally.
    let val_w = ui.painter()
        .layout_no_wrap(value_display.as_ref().to_owned(), val_font.clone(), egui::Color32::BLACK)
        .rect.width();
    let content_w = indent + 18.0 + key_w + sep_w + val_w + 8.0;
    let row_w = content_w.max(ui.available_width());
    let (id, rect) = ui.allocate_space(egui::vec2(row_w, row_h));

    let response = ui.interact(rect, id, egui::Sense::click());

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
        let hover_bg = if dark { theme::HOVER_BG } else { ui.visuals().widgets.hovered.weak_bg_fill };
        ui.painter().rect_filled(rect, 0.0, hover_bg);
    }

    let painter  = ui.painter();
    let text_col = if is_selected { ui.visuals().selection.stroke.color } else { ui.visuals().text_color() };

    // Indent guides — one 1 px vertical line per ancestor level, aligned under
    // the parent chevrons.
    if dark {
        for d in 0..depth {
            let gx = rect.left() + 4.0 + d as f32 * 16.0 + 8.0;
            painter.vline(gx, rect.y_range(), egui::Stroke::new(1.0, theme::INDENT_GUIDE));
        }
    }

    // y position for single-line elements: centred in the first row_h band.
    let y1 = rect.top() + row_h / 2.0;
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
    if !key_text.is_empty() {
        painter.text(egui::pos2(x, y1), egui::Align2::LEFT_CENTER, key_display.as_ref(), key_font.clone(), key_color);
        x += key_w;
        painter.text(egui::pos2(x, y1), egui::Align2::LEFT_CENTER, sep_text, key_font.clone(), sep_color);
        x += sep_w;
    }

    // Value — single line, vertically centred.
    painter.text(egui::pos2(x, y1), egui::Align2::LEFT_CENTER, value_display.as_ref(), val_font, value_color);

    // Collect actions
    let mut actions: Vec<RowAction> = Vec::new();
    if response.clicked() {
        actions.push(RowAction::Select(node_idx));
        // Toggle if click was on triangle
        if can_toggle {
            if let Some(click_pos) = response.interact_pointer_pos() {
                if tri_rect.contains(click_pos) {
                    actions.push(RowAction::Toggle(node_idx));
                }
            }
        }
    }
    if response.double_clicked() && can_toggle {
        // Double-click anywhere on a container toggles it
        actions.push(RowAction::Toggle(node_idx));
    }

    // Context menu (right-click)
    response.context_menu(|ui| {
        let n = &index.nodes[node_idx as usize];

        if ui.button("Copy Path").clicked() {
            ui.ctx().copy_text(build_path(&index.nodes, index, node_idx));
            ui.close();
        }

        // "Copy Key" only when the node actually has a key or array index
        let key_str: Option<String> = if n.key_len > 0 {
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
            let raw = index.value_bytes(n);
            ui.ctx().copy_text(String::from_utf8_lossy(raw).into_owned());
            ui.close();
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
    });

    actions
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
            let key = idx_obj.key_of(node);
            // dot notation for simple identifiers, bracket+quote otherwise
            if !key.is_empty()
                && key.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                segments.push(format!(".{key}"));
            } else {
                segments.push(format!(".[\"{key}\"]"));
            }
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
    fn status_bar(&self, ui: &mut egui::Ui) {
        // 1 px top border above the bar
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, theme::BORDER));

        if self.mode == AppMode::Compare {
            self.compare_status_bar(ui);
            return;
        }

        ui.horizontal_centered(|ui| {
            if let Some(info) = &self.file_info {
                ui.label(
                    egui::RichText::new(format!("📄 {}", info.name)).color(theme::TEXT_PRIMARY),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format_size(info.size_bytes)).color(theme::TEXT_MUTED),
                );
                if let Some(t) = &self.tree {
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} nodes",
                            format_count(t.index.nodes.len().saturating_sub(1))
                        ))
                            .color(theme::TEXT_FAINT),
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
            }

            // Right-aligned: encoding, format badge, root-type badge.
            if let Some(t) = &self.tree {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new("UTF-8").small().color(theme::TEXT_FAINT));
                    ui.add_space(8.0);

                    let fmt = if t.index.is_ndjson { "NDJSON" } else { "JSON" };
                    status_badge(
                        ui,
                        egui::RichText::new(fmt).small().strong().color(egui::Color32::WHITE),
                        theme::ACCENT,
                        egui::Stroke::NONE,
                    );
                });
            }
        });
    }
}

/// Small rounded badge used in the status bar.
fn status_badge(ui: &mut egui::Ui, text: egui::RichText, fill: egui::Color32, stroke: egui::Stroke) {
    egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(text);
        });
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
        self.file_info    = Some(FileInfo { name, size_bytes: size });
        self.tree         = None;
        self.load_error   = None;
        self.load_progress = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.load_rx      = Some(loader::spawn_load(path));
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
        let (data, name) = match paste::decode_jwt(text) {
            Some(decoded) => (decoded, "Pasted JWT"),
            None          => (text.as_bytes().to_vec(), "Pasted JSON"),
        };
        self.file_info    = Some(FileInfo { name: name.to_owned(), size_bytes: data.len() as u64 });
        self.tree         = None;
        self.load_error   = None;
        self.load_progress = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.load_rx      = Some(loader::spawn_parse(data));
    }

    fn kick_search(&mut self) {
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
            self.search_pending =
                Some(std::thread::spawn(move || search::search(&index, &query, use_regex)));
        }
    }
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
        pane.file_info     = Some(FileInfo { name, size_bytes: size });
        pane.index         = None;
        pane.load_error    = None;
        pane.load_progress = 0.0;
        pane.load_rx       = Some(loader::spawn_load(path));
    }

    fn open_pasted_into_pane(&mut self, side: Side, text: &str) {
        let text = text.trim();
        if text.is_empty() { return; }
        let (data, name) = match paste::decode_jwt(text) {
            Some(d) => (d, "Pasted JWT"),
            None    => (text.as_bytes().to_vec(), "Pasted JSON"),
        };
        let pane = self.compare.pane_mut(side);
        pane.file_info     = Some(FileInfo { name: name.to_owned(), size_bytes: data.len() as u64 });
        pane.index         = None;
        pane.load_error    = None;
        pane.load_progress = 0.0;
        pane.load_rx       = Some(loader::spawn_parse(data));
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
                LoadMsg::Error(e)    => { pane.load_error = Some(e); pane.load_rx = None; }
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
        tree.refresh_visible(&result);
        self.compare.result  = Some(result);
        self.compare.tree    = Some(tree);
        self.compare.diff_rx = None;
        ctx.request_repaint();
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
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::BORDER));

        let mut changed = false;
        egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                changed |= diff_option_toggle(ui, "Aa",    "Ignore case (values & keys)", &mut self.compare.options.ignore_case);
                changed |= diff_option_toggle(ui, "[≈]",   "Ignore array order",          &mut self.compare.options.ignore_array_order);
                changed |= diff_option_toggle(ui, "∅=–",   "Treat null as missing",       &mut self.compare.options.null_equals_missing);
                changed |= diff_option_toggle(ui, "1≈\"1\"", "Type coercion",             &mut self.compare.options.type_coercion);
                changed |= diff_option_toggle(ui, "␣",     "Trim whitespace in strings",  &mut self.compare.options.trim_whitespace);

                ui.separator();
                ui.label(egui::RichText::new("ignore keys").color(theme::TEXT_MUTED));
                if ui.add(egui::TextEdit::singleline(&mut self.compare.ignore_keys_raw).desired_width(130.0).hint_text("id, ts")).changed() {
                    changed = true;
                }
                ui.label(egui::RichText::new("regex").color(theme::TEXT_MUTED));
                if ui.add(egui::TextEdit::singleline(&mut self.compare.ignore_pattern_raw).desired_width(110.0).hint_text("^_")).changed() {
                    changed = true;
                }
                if self.compare.pattern_error {
                    ui.colored_label(egui::Color32::from_rgb(0xE5, 0x53, 0x4B), "⚠").on_hover_text("Invalid regex");
                }
            });
        });

        if changed {
            self.recompute_options_from_raw();
            self.compare.needs_rediff = true;
        }
    }

    fn compare_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            fn name(p: &ComparePane) -> &str {
                p.file_info.as_ref().map(|f| f.name.as_str()).unwrap_or("—")
            }
            ui.label(egui::RichText::new(format!("◧ {}", name(&self.compare.left))).color(theme::TEXT_PRIMARY));
            ui.label(egui::RichText::new("vs").color(theme::TEXT_FAINT));
            ui.label(egui::RichText::new(format!("{} ◨", name(&self.compare.right))).color(theme::TEXT_PRIMARY));

            if self.compare.left.load_rx.is_some() || self.compare.right.load_rx.is_some() {
                ui.spinner();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.compare.diff_rx.is_some() {
                    ui.spinner();
                    ui.label(egui::RichText::new("Comparing…").small().color(theme::TEXT_FAINT));
                } else if let Some(result) = &self.compare.result {
                    let total = result.changed + result.added + result.removed;
                    let txt = if total == 0 { "identical".to_string() } else { format!("{total} differences") };
                    ui.label(egui::RichText::new(txt).small().color(theme::TEXT_FAINT));
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
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), ui.min_rect().bottom(), egui::Stroke::new(1.0, theme::BORDER));
        let _ = r;
    }

    fn pane_header(&mut self, ui: &mut egui::Ui, side: Side) {
        let active = self.compare.active_pane == side;
        let (name, loading) = {
            let pane = self.compare.pane(side);
            (pane.file_info.as_ref().map(|f| f.name.clone()), pane.load_rx.is_some())
        };
        let title = name.unwrap_or_else(|| "— no document —".to_string());

        // Reserve the whole header rect up-front and sense clicks on it, so a
        // click anywhere on the header (the area not covered by the Open / Paste
        // buttons, which are drawn on top and keep their own clicks) activates
        // the pane. The buttons are laid out inside via a child UI.
        let margin = egui::vec2(8.0, 4.0);
        let height = ui.spacing().interact_size.y + 2.0 * margin.y;
        let (rect, bg) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
        if bg.clicked() {
            self.compare.active_pane = side;
        }

        ui.painter().rect_filled(
            rect, 0.0,
            if active { theme::SELECTION_BG } else { theme::BG_PANEL },
        );

        let content_rect = egui::Rect::from_min_max(rect.min + margin, rect.max - margin);
        let mut content_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        {
            let ui = &mut content_ui;
            ui.label(egui::RichText::new(format!("📄 {title}")).color(theme::TEXT_PRIMARY));
            if loading { ui.spinner(); }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Paste").clicked() {
                    self.compare.active_pane = side;
                    let ctx = ui.ctx().clone();
                    self.request_paste(&ctx);
                }
                if ui.small_button("Open").clicked() {
                    self.compare.active_pane = side;
                    self.open_into_pane_dialog(side);
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
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Load JSON into both panes to compare.\nClick a pane, then ⌘O to open or ⌘V to paste.")
                        .color(theme::TEXT_MUTED),
                );
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

        let avail_h   = ui.available_height();
        let row_pitch = row_h + ui.spacing().item_spacing.y;
        let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false; 2]);
        if let Some(row) = scroll_to_row {
            let y = (row as f32 * row_pitch - avail_h / 2.0 + row_h / 2.0).max(0.0);
            scroll_area = scroll_area.vertical_scroll_offset(y);
        }

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
fn diff_option_toggle(ui: &mut egui::Ui, label: &str, hover: &str, value: &mut bool) -> bool {
    let color = if *value { theme::ACCENT } else { theme::TEXT_MUTED };
    let resp = ui
        .selectable_label(*value, egui::RichText::new(label).color(color))
        .on_hover_text(hover);
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
    expanded:  &std::collections::HashSet<u32>,
    selected:  Option<u32>,
    node_idx:  u32,
    row_h:     f32,
    key_font:  egui::FontId,
    val_font:  egui::FontId,
    root:      u32,
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

    let mid_x = rect.center().x;
    let left_cell  = egui::Rect::from_min_max(rect.left_top(), egui::pos2(mid_x, rect.bottom()));
    let right_cell = egui::Rect::from_min_max(egui::pos2(mid_x, rect.top()), rect.right_bottom());

    // Hover first, so status tints layer over it.
    if !is_selected && response.hovered() {
        let hover_bg = if dark { theme::HOVER_BG } else { ui.visuals().widgets.hovered.weak_bg_fill };
        ui.painter().rect_filled(rect, 0.0, hover_bg);
    }

    // Per-cell status tints.
    let (lt, rt) = match status {
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
    ui.painter().vline(mid_x, rect.y_range(), egui::Stroke::new(1.0, theme::BORDER));

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
                ui.ctx().copy_text(String::from_utf8_lossy(left.value_bytes(&left.nodes[li as usize])).into_owned());
                ui.close();
            }
        }
        if let Some(ri) = dn.right_idx() {
            if ui.button("Copy Right Value").clicked() {
                ui.ctx().copy_text(String::from_utf8_lossy(right.value_bytes(&right.nodes[ri as usize])).into_owned());
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
            painter.vline(gx, cell.y_range(), egui::Stroke::new(1.0, theme::INDENT_GUIDE));
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
        let kw = painter.layout_no_wrap(key_display.as_ref().to_owned(), key_font.clone(), egui::Color32::BLACK).rect.width();
        painter.text(egui::pos2(x, y1), egui::Align2::LEFT_CENTER, key_display.as_ref(), key_font.clone(), key_color);
        x += kw;
        let sw = painter.layout_no_wrap(sep_text.to_owned(), key_font.clone(), egui::Color32::BLACK).rect.width();
        painter.text(egui::pos2(x, y1), egui::Align2::LEFT_CENTER, sep_text, key_font.clone(), sep_color);
        x += sw;
    }
    painter.text(egui::pos2(x, y1), egui::Align2::LEFT_CENTER, value_display.as_ref(), val_font.clone(), value_color);

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
