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

// ─── app state ───────────────────────────────────────────────────────────────

struct FileInfo {
    name:       String,
    size_bytes: u64,
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
        .with_title("JSON Viewer")
        .with_inner_size([1200.0, 800.0])
        .with_min_inner_size([700.0, 400.0]);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "JSON Viewer",
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
            if acts & macos_menu::ACT_OPEN_FILE    != 0 { self.open_file_dialog(); }
            if acts & macos_menu::ACT_PASTE        != 0 { self.request_paste(ui.ctx()); }
            if acts & macos_menu::ACT_SETTINGS     != 0 { self.settings_open = true; }
            if acts & macos_menu::ACT_FOCUS_SEARCH != 0 { self.focus_search = true; }
            if acts & macos_menu::ACT_COLLAPSE_ALL != 0 {
                if let Some(t) = &mut self.tree { t.collapse_all(); }
            }
            if acts & macos_menu::ACT_EXPAND_ALL   != 0 {
                if let Some(t) = &mut self.tree { t.expand_all(); }
            }
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

        // Keep repainting while loading
        if self.load_rx.is_some() {
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
            self.open_file(path);
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
                self.open_pasted(&text);
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

        if cmd_o      { self.open_file_dialog(); }
        if cmd_f      { self.focus_search = true; }
        if cmd_comma  { self.settings_open = true; }
        if arrow_up    { if let Some(t) = &mut self.tree { t.select_up(); } }
        if arrow_down  { if let Some(t) = &mut self.tree { t.select_down(); } }
        if arrow_left  { if let Some(t) = &mut self.tree { t.select_left(); } }
        if arrow_right { if let Some(t) = &mut self.tree { t.select_right(); } }
        if cmd_g       { if let Some(t) = &mut self.tree { t.search_next(); } }
        if cmd_shift_g { if let Some(t) = &mut self.tree { t.search_prev(); } }
        if opt_c       { if let Some(t) = &mut self.tree { t.collapse_all(); } }
        if opt_x       { if let Some(t) = &mut self.tree { t.expand_all(); } }
        if page_up     { if let Some(t) = &mut self.tree { t.select_page_up(20); } }
        if page_down   { if let Some(t) = &mut self.tree { t.select_page_down(20); } }
        if home        { if let Some(t) = &mut self.tree { t.select_home(); } }
        if end         { if let Some(t) = &mut self.tree { t.select_end(); } }

        // ── 4b. Type-ahead selection ──
        // Only active when no text widget (e.g. search box) has keyboard focus.
        if self.tree.is_some() && ui.ctx().memory(|m| m.focused().is_none()) {
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

        if self.settings.show_breadcrumbs && self.tree.is_some() {
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
            self.tree_panel(ui);
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

            // Settings button (right-aligned)
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
    let (key_text, key_color): (String, egui::Color32) = if node.key_len > 0 {
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
    };
    let sep_text  = " : ";
    let sep_color = if dark { theme::PUNCT } else { egui::Color32::from_rgb(120, 120, 120) };

    // Value text + color
    let str_color       = if dark { theme::STRING }    else { egui::Color32::from_rgb(163, 21, 21) };
    let container_color = if dark { theme::CONTAINER } else { egui::Color32::from_rgb(100, 100, 100) };
    let (value_text, value_color): (String, egui::Color32) = match kind {
        NodeKind::Object => (format!("{{ {} }}", child_count), container_color),
        NodeKind::Array  => (format!("[ {} ]",   child_count), container_color),
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
    };

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
