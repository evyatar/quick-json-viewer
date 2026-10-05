//! All view code: the chrome around the tree (menu bar, toolbar, breadcrumbs,
//! status bar), the Viewer / Compare panels, popup menus and modal dialogs.

use iced::widget::{
    button, center, column, container, mouse_area, opaque, progress_bar, responsive, rich_text, row,
    rule, scrollable, space, span, stack, text, text_editor, text_input, tooltip, Button, Text,
};
use iced::{Color, Element, Font, Length, Padding};

use crate::ai;
use crate::codegen;
use crate::export;
use crate::index::{self, NodeKind};
use crate::merge;
use crate::search;
use crate::settings;
use crate::theme::{self, Palette};
use crate::tree_view::{text_width, DiffRows, Rows, TreeView, ViewerRows};
use crate::url_parse;
use crate::update;
use crate::{
    added_path, build_path, App, AppMode, Dialog, DiffKind, DiffOpt, ExportFormat, ExportScope,
    MenuBarId, Message, Overlay, OverlayKind, SaveAction, Side, Submenu, ADD_KEY_ID,
    ADD_VALUE_ID, DIFF_SCROLL_ID, EDIT_ID, SEARCH_ID, TREE_SCROLL_ID, URL_ID,
};
use crate::{format_count, format_size, EditField};

pub const MENU_BUTTON_W: f32 = 64.0;
pub const MENU_BAR_H: f32 = 24.0;
const TOOLBAR_H: f32 = 44.0;
const STATUS_H: f32 = 26.0;

/// Per-frame text style helpers derived from the settings + theme.
#[derive(Clone, Copy)]
struct Ui {
    pal:   Palette,
    font:  Font,
    fs:    f32,
    small: f32,
}

impl Ui {
    fn new(app: &App) -> Self {
        let fs = app.settings.font_size;
        Self {
            pal: Palette::for_dark(app.is_dark()),
            font: app.settings.val_font(),
            fs,
            small: (fs - 2.0).max(10.0),
        }
    }

    fn t<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        text(s).size(self.fs).font(self.font)
    }
    fn primary<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        self.t(s).color(self.pal.text_primary)
    }
    fn muted<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        self.t(s).color(self.pal.text_muted)
    }
    fn faint<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        self.t(s).color(self.pal.text_faint)
    }
    fn small<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        text(s).size(self.small).font(self.font)
    }
    fn mono<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        text(s).size(self.fs).font(Font::MONOSPACE)
    }

    /// Point size for symbol / emoji glyphs. They come from a fallback font
    /// that renders visibly smaller than the monospace letters at equal size,
    /// so they get bumped up to match.
    fn sym_size(&self) -> f32 {
        (self.fs * 1.1).round()
    }
    /// A standalone symbol glyph (⚙ ▲ ▼ ✕ …) sized to match body text.
    fn sym<'a>(&self, s: impl iced::advanced::text::IntoFragment<'a>) -> Text<'a> {
        text(s).size(self.sym_size())
    }
    /// A symbol glyph followed by a text label, e.g. `☑ Select`.
    fn sym_label<'a>(&self, glyph: &'a str, label: &'a str) -> Element<'a, Message> {
        row![self.sym(glyph), self.t(label)].spacing(5).align_y(iced::Center).into()
    }
    fn sym_btn<'a>(&self, glyph: &'a str, msg: Option<Message>) -> Button<'a, Message> {
        button(self.sym(glyph)).style(theme::button_style).padding([1, 7]).on_press_maybe(msg)
    }

    fn btn<'a>(&self, label: &'a str, msg: Option<Message>) -> Button<'a, Message> {
        button(self.t(label)).style(theme::button_style).padding([3, 8]).on_press_maybe(msg)
    }
    fn small_btn<'a>(&self, label: &'a str, msg: Option<Message>) -> Button<'a, Message> {
        button(self.small(label)).style(theme::button_style).padding([2, 6]).on_press_maybe(msg)
    }
    fn tab<'a>(&self, label: impl Into<Element<'a, Message>>, active: bool, msg: Message) -> Button<'a, Message> {
        button(label).style(theme::tab_button_style(active)).padding([3, 8]).on_press(msg)
    }

    fn keycap<'a>(&self, cap: &'a str) -> Element<'a, Message> {
        container(text(cap).size(16).font(Font::MONOSPACE).color(self.pal.text_primary))
            .padding([2, 7])
            .style(theme::keycap_style)
            .into()
    }

    /// One `⌘ O` / `↑ / ↓` chip group.
    fn keycaps<'a>(&self, keys: &'a str) -> Element<'a, Message> {
        let mut r = row![].spacing(4).align_y(iced::Center);
        for (i, cap) in keys.split(" / ").enumerate() {
            if i > 0 {
                r = r.push(self.muted("/"));
            }
            r = r.push(self.keycap(cap));
        }
        r.into()
    }
}

fn tip<'a>(el: impl Into<Element<'a, Message>>, tip: impl Into<String>, ui: &Ui) -> Element<'a, Message> {
    let tip: String = tip.into();
    tooltip(
        el,
        container(text(tip).size(ui.small).color(ui.pal.text_primary)).padding(6).style(theme::card_style),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .into()
}

fn hdivider<'a>() -> Element<'a, Message> {
    rule::horizontal(1).style(theme::divider_style).into()
}

// ─── root ────────────────────────────────────────────────────────────────────

pub fn root(app: &App) -> Element<'_, Message> {
    let ui = Ui::new(app);

    let mut col = column![].width(Length::Fill).height(Length::Fill);
    if app.settings.show_menu_bar {
        col = col.push(menu_bar(app, &ui));
    }
    col = col.push(toolbar(app, &ui));
    col = col.push(hdivider());
    if let Some(banner) = update_banner(app, &ui) {
        col = col.push(banner);
    }
    if app.mode == AppMode::Viewer && app.settings.show_breadcrumbs && app.tree.is_some() {
        col = col.push(breadcrumbs_bar(app, &ui));
        col = col.push(hdivider());
    }
    if app.mode == AppMode::Compare {
        col = col.push(compare_options_bar(app, &ui));
        col = col.push(hdivider());
    }

    let central: Element<'_, Message> = match app.mode {
        AppMode::Viewer  => tree_panel(app, &ui),
        AppMode::Compare => compare_panel(app, &ui),
    };
    let mut main_row = row![container(central).width(Length::Fill).height(Length::Fill)].height(Length::Fill);
    if app.mode == AppMode::Viewer && app.settings.ai_enabled && app.ai.open {
        main_row = main_row.push(ai_side_panel(app, &ui));
    }
    col = col.push(main_row);
    col = col.push(hdivider());
    col = col.push(status_bar(app, &ui));

    // While the AI panel's splitter is held, track the pointer to resize it.
    let col: Element<'_, Message> = if app.ai_resizing {
        mouse_area(col).on_move(Message::AiResizeDrag).into()
    } else {
        col.into()
    };
    let mut layers = stack![col];
    if let Some(overlay) = app.overlay {
        layers = layers.push(popup_layer(app, &ui, overlay));
    }
    if let Some(modal) = modal_layer(app, &ui) {
        layers = layers.push(modal);
    }
    layers.width(Length::Fill).height(Length::Fill).into()
}

// ─── menu bar ────────────────────────────────────────────────────────────────

fn menu_bar<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let mut r = row![space().width(6)].align_y(iced::Center);
    for id in MenuBarId::ALL {
        let active = matches!(app.overlay, Some(Overlay { kind: OverlayKind::MenuBar(cur), .. }) if cur == id);
        r = r.push(
            button(text(id.label()).size(13).font(ui.font).center())
                .width(MENU_BUTTON_W)
                .padding([2, 4])
                .style(theme::tab_button_style(active))
                .on_press(Message::MenuBar(id)),
        );
    }
    container(r)
        .width(Length::Fill)
        .height(MENU_BAR_H)
        .style(theme::panel_style)
        .into()
}

// ─── toolbar ─────────────────────────────────────────────────────────────────

fn toolbar<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let mut r = row![].spacing(6).align_y(iced::Center).width(Length::Fill);

    for (label, mode) in [("Viewer", AppMode::Viewer), ("Compare", AppMode::Compare)] {
        r = r.push(ui.tab(ui.t(label), app.mode == mode, Message::SetMode(mode)));
    }
    r = r.push(space().width(10));

    match app.mode {
        AppMode::Viewer  => r = viewer_toolbar(app, ui, r),
        AppMode::Compare => r = compare_toolbar(app, ui, r),
    }

    r = r.push(space().width(Length::Fill));
    r = r.push(tip(ui.sym_btn("⚙", Some(Message::ToggleSettings)), "Settings", ui));

    container(r)
        .width(Length::Fill)
        .height(TOOLBAR_H)
        .padding([0, 10])
        .align_y(iced::Center)
        .style(theme::panel_style)
        .into()
}

fn viewer_toolbar<'a>(
    app: &'a App,
    ui: &Ui,
    mut r: iced::widget::Row<'a, Message>,
) -> iced::widget::Row<'a, Message> {
    r = r.push(ui.btn("Open File", Some(Message::OpenFile)));

    if let Some(tree) = &app.tree {
        r = r.push(tip(
            ui.tab(ui.sym_label("☑", "Select"), tree.multi_select, Message::ToggleMultiSelect),
            "Multi-select mode — check rows, then right-click → Export",
            ui,
        ));
    }

    // The button is always visible; on an unconfigured install it sends the
    // user to Settings → AI Assistant instead of opening an assistant that
    // could not talk to a provider.
    let configured = app.settings.ai_enabled && app.ai_settings_ui.key_present(app.settings.ai_provider);
    r = r.push(tip(
        ui.tab(ui.sym_label("✨", "AI"), app.ai.open, Message::ToggleAi),
        if configured {
            "AI assistant — query and edit with your own API key"
        } else {
            "AI assistant — set up your API key in Settings"
        },
        ui,
    ));
    r = r.push(space().width(8));

    // Shrink the search field on narrow windows so the controls to its right
    // stay visible instead of being pushed off-screen.
    let ai_w = if app.settings.ai_enabled && app.ai.open { app.ai.width } else { 0.0 };
    let search_w = (app.window_size.width - ai_w - 690.0).clamp(80.0, 260.0);

    let mut pill = row![
        ui.sym("🔍").color(ui.pal.text_muted).size(ui.fs*0.9),
        text_input("Search (age > 30)", &app.search_input)
            .id(SEARCH_ID)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit)
            .size(ui.fs)
            .font(ui.font)
            .padding([2, 4])
            .width(search_w)
            .style(theme::search_input_style),
    ]
    .spacing(4)
    .align_y(iced::Center);
    if !app.search_input.is_empty() {
        pill = pill.push(
            button(text("✕").size(ui.fs).color(ui.pal.text_muted))
                .padding([0, 4])
                .style(theme::flat_button_style)
                .on_press(Message::SearchClear),
        );
    }
    r = r.push(container(pill).padding([2, 8]).style(theme::pill_style));

    let use_re = app.tree.as_ref().map(|t| t.search_use_regex).unwrap_or(false);
    r = r.push(tip(ui.tab(ui.mono(".*"), use_re, Message::ToggleRegex), "Regex mode", ui));
    r = r.push(tip(
        ui.btn("?", Some(Message::OpenDialog(Dialog::SearchHelp))),
        "Search syntax help",
        ui,
    ));

    let has_results = !app.search_input.is_empty();
    r = r.push(ui.sym_btn("▲", has_results.then_some(Message::SearchPrev)));
    r = r.push(ui.sym_btn("▼", has_results.then_some(Message::SearchNext)));

    if let Some(t) = &app.tree {
        if !t.search_results.is_empty() {
            r = r.push(ui.muted(format!("{}/{}", t.search_cursor + 1, t.search_results.len())));
        }
    }
    r
}

fn compare_toolbar<'a>(
    app: &'a App,
    ui: &Ui,
    mut r: iced::widget::Row<'a, Message>,
) -> iced::widget::Row<'a, Message> {
    // Summary of the current diff. Each counter is a toggle: clicking it
    // shows/hides that type of change; a muted colour means it's turned off.
    if let Some(result) = &app.compare.result {
        let counts = (result.changed, result.added, result.removed);
        // All-zero counters mean identical files — say so instead of showing
        // a row of "0 …" badges.
        if counts == (0, 0, 0) {
            r = r.push(ui.t("identical files").color(theme::NEW));
            return r;
        }
        let filter = app.compare.filter;
        let badges = [
            (counts.0, "changed", theme::CHANGED, filter.changed, DiffKind::Changed),
            (counts.1, "added",   theme::NEW,     filter.added,   DiffKind::Added),
            (counts.2, "removed", theme::DELETED, filter.removed, DiffKind::Removed),
        ];
        for (n, label, color, on, kind) in badges {
            let color = if on { color } else { ui.pal.text_faint };
            r = r.push(tip(
                button(ui.t(format!("{n} {label}")).color(color))
                    .padding([3, 6])
                    .style(theme::flat_button_style)
                    .on_press(Message::DiffFilter(kind)),
                if on { format!("Hide {label} nodes") } else { format!("Show {label} nodes") },
                ui,
            ));
        }
    } else {
        r = r.push(ui.muted("Load both panes to compare"));
    }

    r = r.push(space().width(6));

    let has_diffs = app.compare.result.as_ref().map_or(false, |res| !res.diff_positions.is_empty());
    r = r.push(tip(ui.sym_btn("▲", has_diffs.then_some(Message::ComparePrev)), "Previous difference", ui));
    r = r.push(tip(ui.sym_btn("▼", has_diffs.then_some(Message::CompareNext)), "Next difference", ui));
    r = r.push(tip(
        ui.tab(ui.t("diffs only"), app.compare.show_only_diffs, Message::ToggleOnlyDiffs),
        "Hide unchanged nodes",
        ui,
    ));
    r
}

// ─── update banner ───────────────────────────────────────────────────────────

/// Thin top strip shown when a newer release is available. Notify-only: it
/// links to the release page and offers the `brew upgrade` command — it never
/// downloads or replaces the binary.
fn update_banner<'a>(app: &'a App, ui: &Ui) -> Option<Element<'a, Message>> {
    let info = app.pending_update()?;
    let upgrading = app.install_watcher_rx.is_some();
    let label = if upgrading {
        format!("Installing v{}…", info.version)
    } else {
        format!("Update available — v{}", info.version)
    };
    let notes_hint: String = info.notes.lines().take(12).collect::<Vec<_>>().join("\n");

    let mut r = row![
        ui.sym("⬆").color(Color::WHITE),
        ui.t(label).color(Color::WHITE),
        space().width(Length::Fill),
    ]
    .spacing(6)
    .align_y(iced::Center);

    if !upgrading {
        let view_btn = ui.small_btn("View release", Some(Message::OpenUrl(info.html_url.clone())));
        if notes_hint.trim().is_empty() {
            r = r.push(view_btn);
        } else {
            r = r.push(tip(view_btn, notes_hint, ui));
        }
        r = r.push(ui.small_btn("Copy command", Some(Message::CopyBrewCommand)));
    }
    r = r.push(tip(
        ui.small_btn(if upgrading { "Upgrading…" } else { "Upgrade now" }, (!upgrading).then_some(Message::UpgradeNow)),
        format!("Runs in the background:\n{}", update::BREW_UPGRADE_CMD),
        ui,
    ));
    if !upgrading {
        r = r.push(tip(ui.sym_btn("✕", Some(Message::DismissUpdate)), "Dismiss", ui));
    }

    Some(
        container(r)
            .width(Length::Fill)
            .height(ui.fs + 16.0)
            .padding([0, 10])
            .align_y(iced::Center)
            .style(theme::fill_style(ui.pal.accent))
            .into(),
    )
}

// ─── breadcrumbs ─────────────────────────────────────────────────────────────

fn breadcrumbs_bar<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let height = ui.fs + 14.0;
    let strip = |content: Element<'a, Message>| -> Element<'a, Message> {
        container(content)
            .width(Length::Fill)
            .height(height)
            .padding([0, 10])
            .align_y(iced::Center)
            .style(theme::strip_style)
            .into()
    };

    let Some(tree) = &app.tree else { return strip(space().into()) };
    let Some(sel) = tree.selected else { return strip(space().into()) };
    let index = &*tree.index;
    let added_items = &tree.added_items;
    let nodes_len = index.nodes.len();
    let font_size = ui.fs - 1.0;

    // Ancestor chain, root first. A pending (not-yet-saved) added item has no
    // real node — walk up from its real parent instead.
    let mut chain: Vec<u32> = Vec::new();
    let mut cur = sel;
    while export::is_added(nodes_len, cur) {
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

    let mut r = row![].spacing(4).align_y(iced::Center);
    for (i, &node_idx) in chain.iter().enumerate() {
        if i > 0 {
            r = r.push(text("›").size(font_size).font(Font::MONOSPACE).color(ui.pal.text_faint));
        }
        let is_added_row = export::is_added(nodes_len, node_idx);
        let label: String = if is_added_row {
            let item = &added_items[node_idx as usize - nodes_len];
            match &item.key {
                Some(k) => k.clone(),
                None => format!("[{}]", export::added_display_index(&index.nodes, added_items, node_idx)),
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
        let is_last = i + 1 == chain.len();
        let color = if is_added_row { theme::NEW } else if is_last { ui.pal.key } else { ui.pal.text_muted };
        let seg = button(text(label).size(font_size).font(Font::MONOSPACE).color(color))
            .padding([1, 4])
            .style(theme::flat_button_style)
            .on_press(Message::BreadcrumbJump(node_idx));
        // Right-click copies the segment's JSON path.
        r = r.push(mouse_area(seg).on_right_press(Message::CopyPath(node_idx)));
    }

    strip(
        scrollable(r)
            .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::hidden()))
            .width(Length::Fill)
            .into(),
    )
}

// ─── compare options bar ─────────────────────────────────────────────────────

fn compare_options_bar<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let o = &app.compare.options;
    let toggles = [
        ("Aa",      "Ignore case (values & keys)", o.ignore_case,         DiffOpt::IgnoreCase),
        ("[≈]",     "Ignore array order",          o.ignore_array_order,  DiffOpt::ArrayOrder),
        ("∅=–",     "Treat null as missing",       o.null_equals_missing, DiffOpt::NullMissing),
        ("1≈\"1\"", "Type coercion",               o.type_coercion,       DiffOpt::TypeCoercion),
        ("␣",       "Trim whitespace in strings",  o.trim_whitespace,     DiffOpt::Trim),
    ];
    let mut r = row![].spacing(6).align_y(iced::Center);
    for (label, hover, on, opt) in toggles {
        r = r.push(tip(ui.tab(ui.t(label), on, Message::DiffOption(opt)), hover, ui));
    }
    r = r.push(rule::vertical(1).style(theme::divider_style));
    r = r.push(ui.muted("ignore keys"));
    r = r.push(
        text_input("id, ts", &app.compare.ignore_keys_raw)
            .on_input(Message::IgnoreKeys)
            .size(ui.fs)
            .font(ui.font)
            .padding([2, 6])
            .width(130)
            .style(theme::input_style),
    );
    r = r.push(ui.muted("regex"));
    r = r.push(
        text_input("^_", &app.compare.ignore_pattern_raw)
            .on_input(Message::IgnorePattern)
            .size(ui.fs)
            .font(ui.font)
            .padding([2, 6])
            .width(110)
            .style(theme::input_style),
    );
    if app.compare.pattern_error {
        r = r.push(tip(ui.sym("⚠").color(theme::DELETED), "Invalid regex", ui));
    }

    container(
        scrollable(r)
            .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::hidden()))
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(ui.fs + 20.0)
    .padding([0, 10])
    .align_y(iced::Center)
    .style(theme::strip_style)
    .into()
}

// ─── status bar ──────────────────────────────────────────────────────────────

fn status_bar<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let content: Element<'a, Message> = match app.mode {
        AppMode::Viewer  => viewer_status(app, ui),
        AppMode::Compare => compare_status(app, ui),
    };
    container(content)
        .width(Length::Fill)
        .height(STATUS_H)
        .padding([0, 10])
        .align_y(iced::Center)
        .style(theme::panel_style)
        .into()
}

fn viewer_status<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let mut r = row![].spacing(10).align_y(iced::Center).width(Length::Fill);
    if let Some(info) = &app.file_info {
        r = r.push(ui.sym("📄"));
        r = r.push(ui.primary(info.name.as_str()));
        r = r.push(ui.muted(format_size(info.size_bytes)));
        if let Some(t) = &app.tree {
            r = r.push(ui.faint(format!("{} nodes", format_count(t.index.nodes.len().saturating_sub(1)))));
        }
    }
    if app.load_rx.is_some() {
        r = r.push(ui.t(format!("Loading… {:.0}%", app.load_progress * 100.0)));
        r = r.push(progress_bar(0.0..=1.0, app.load_progress).length(120).girth(6));
    }
    if let Some(e) = &app.load_error {
        r = r.push(ui.t(format!("Error: {e}")).color(theme::DELETED));
        if app.load_error_ctx.is_some() {
            r = r.push(ui.small_btn("Show context", Some(Message::OpenDialog(Dialog::ErrorContext(None)))));
        }
    }
    r = r.push(space().width(Length::Fill));

    // Right-aligned: selection export, badges, save actions, clear.
    if app.file_info.is_some() {
        if let Some(t) = &app.tree {
            if t.multi_select && !t.checked.is_empty() {
                r = r.push(ui.muted(format!("{} selected", format_count(t.checked.len()))));
                r = r.push(
                    button(ui.small("Export JSON"))
                        .padding([2, 8])
                        .style(theme::accent_button_style)
                        .on_press(Message::Export(ExportScope::Selection, ExportFormat::Json)),
                );
            }
            r = r.push(text(if t.index.is_ndjson { "NDJSON" } else { "JSON" }).size(ui.small).color(ui.pal.text_faint));
            r = r.push(text("UTF-8").size(ui.small).color(ui.pal.text_faint));

            let dirty = app.is_dirty();
            let can_over = app.can_overwrite();
            if dirty {
                r = r.push(tip(
                    ui.small_btn("Discard Changes", Some(Message::DiscardChanges)),
                    "Discard all unsaved changes",
                    ui,
                ));
                if can_over {
                    r = r.push(tip(
                        ui.small_btn("Save a Copy", Some(Message::Save(SaveAction::Copy))),
                        "Save the edited JSON to a new file",
                        ui,
                    ));
                }
                let (label, hover, action) = if can_over {
                    ("Save Changes", "Overwrite the original file and clear changes", SaveAction::Overwrite)
                } else {
                    ("● Save a Copy…", "Save the edited JSON to a new file", SaveAction::Copy)
                };
                r = r.push(tip(
                    button(ui.small(label)).padding([2, 8]).style(theme::accent_button_style).on_press(Message::Save(action)),
                    hover,
                    ui,
                ));
            }
        }
        r = r.push(tip(
            ui.small_btn("Clear", Some(Message::ClearDocument)),
            "Unload the current document (discards unsaved changes)",
            ui,
        ));
    }
    r.into()
}

fn compare_status<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    fn name(p: &crate::ComparePane) -> &str {
        p.file_info.as_ref().map(|f| f.name.as_str()).unwrap_or("—")
    }
    let mut r = row![
        ui.sym("◧").color(ui.pal.text_primary),
        ui.primary(name(&app.compare.left)),
        ui.faint("vs"),
        ui.primary(name(&app.compare.right)),
        ui.sym("◨").color(ui.pal.text_primary),
    ]
    .spacing(8)
    .align_y(iced::Center)
    .width(Length::Fill);
    if app.compare.left.load_rx.is_some() || app.compare.right.load_rx.is_some() {
        r = r.push(ui.muted("Loading…"));
    }
    r = r.push(space().width(Length::Fill));
    if app.compare.diff_rx.is_some() {
        r = r.push(text("Comparing…").size(ui.small).color(ui.pal.text_faint));
    } else if let Some(result) = &app.compare.result {
        let total = result.changed + result.added + result.removed;
        if total > 0 {
            r = r.push(ui.t(format!("{total} differences")));
        }
    }
    r.into()
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

fn empty_state<'a>(ui: &Ui) -> Element<'a, Message> {
    // Pick a tip once per launch so it doesn't change between frames.
    static TIP_IDX: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let tip_text = EMPTY_STATE_TIPS[*TIP_IDX.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as usize)
            .unwrap_or(0)
            % EMPTY_STATE_TIPS.len()
    })];

    let options = [
        ("⌘ O", "Open a file"),
        ("⌘ L", "Fetch JSON from a URL"),
        ("⌘ V", "Paste JSON, a JWT, or a curl command"),
        ("drop", "Drag a file anywhere in this window"),
    ];
    let mut grid = column![].spacing(10);
    for (cap, desc) in options {
        grid = grid.push(
            row![
                container(ui.keycaps(cap)).width(90).align_x(iced::Right),
                ui.muted(desc),
            ]
            .spacing(14)
            .align_y(iced::Center),
        );
    }

    center(
        column![
            text("{ }").size(34).font(Font::MONOSPACE).color(ui.pal.text_faint),
            space().height(10),
            text("Open a JSON file to get started").size(17).font(ui.font).color(ui.pal.text_primary),
            space().height(20),
            grid,
            space().height(26),
            text(format!("Tip:  {tip_text}")).size(12).font(ui.font).color(ui.pal.text_faint),
        ]
        .align_x(iced::Center),
    )
    .into()
}

fn tree_panel<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let Some(tree) = &app.tree else {
        if app.load_rx.is_some() {
            return center(
                column![
                    ui.muted(format!("Loading… {:.0}%", app.load_progress * 100.0)),
                    progress_bar(0.0..=1.0, app.load_progress).length(240).girth(8),
                ]
                .spacing(10)
                .align_x(iced::Center),
            )
            .into();
        }
        return empty_state(ui);
    };

    let rows = Rows::Viewer(ViewerRows {
        index:             &tree.index,
        added_items:       &tree.added_items,
        expanded:          &tree.expanded,
        selected:          tree.selected,
        search_result_set: &tree.search_result_set,
        visible:           &tree.visible,
        edit_overlay:      &app.edit_overlay,
        saved_overlay:     &app.saved_overlay,
        multi_select:      tree.multi_select,
        checked:           &tree.checked,
    });
    let view = TreeView::new(
        rows,
        app.settings.row_height(),
        app.settings.key_font(),
        app.settings.val_font(),
        ui.fs,
        ui.pal.dark,
        app.doc_generation,
        Message::Row,
    );
    scrollable(view)
        .id(TREE_SCROLL_ID)
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new(),
            horizontal: scrollable::Scrollbar::new(),
        })
        .on_scroll(Message::TreeScrolled)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::scrollable_style)
        .into()
}

// ─── compare panel ───────────────────────────────────────────────────────────

/// `s` shortened to fit `max_w` by replacing its middle with "…" — keeping
/// both ends, so similar file names (shared prefix, differing date or
/// extension) stay distinguishable. Returns whether it was shortened.
fn ellipsize_middle(s: &str, font: Font, size: f32, max_w: f32) -> (String, bool) {
    if text_width(s, font, size) <= max_w {
        return (s.to_owned(), false);
    }
    let chars: Vec<char> = s.chars().collect();
    let cut = |keep: usize| -> String {
        let head = keep.div_ceil(2);
        let tail = keep - head;
        let mut out: String = chars[..head].iter().collect();
        out.push('…');
        out.extend(&chars[chars.len() - tail..]);
        out
    };
    // Largest number of kept characters that still fits.
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if text_width(&cut(mid), font, size) <= max_w { lo = mid } else { hi = mid - 1 }
    }
    (cut(lo), true)
}

fn pane_header<'a>(app: &'a App, ui: &Ui, side: Side) -> Element<'a, Message> {
    let pane = app.compare.pane(side);
    let active = app.compare.active_pane == side;
    let loading = pane.load_rx.is_some();
    let loaded = pane.file_info.is_some();
    let dirty = pane.is_dirty();
    let name = pane.file_info.as_ref().map(|f| f.name.clone()).unwrap_or_else(|| "— no document —".to_string());

    // The title takes whatever width the buttons leave and is cut down with
    // "…" to fit; the full name is then shown as a tooltip.
    let suffix = if dirty { " ●" } else { "" };
    let (font, fs, small, pal) = (ui.font, ui.fs, ui.small, ui.pal);
    let title = responsive(move |size| {
        let avail = (size.width - text_width(suffix, font, fs)).max(0.0);
        let (shown, cut) = ellipsize_middle(&name, font, fs, avail);
        let t = text(format!("{shown}{suffix}"))
            .size(fs)
            .font(font)
            .color(pal.text_primary)
            .wrapping(text::Wrapping::None);
        if !cut {
            return t.into();
        }
        tooltip(
            t,
            container(text(name.clone()).size(small).color(pal.text_primary)).padding(6).style(theme::card_style),
            tooltip::Position::Bottom,
        )
        .gap(4)
        .into()
    })
    .width(Length::Fill)
    .height(Length::Fixed((fs * 1.3).ceil() + 1.0));

    let mut r = row![ui.sym("📄"), title].spacing(8).align_y(iced::Center).width(Length::Fill);
    if loading {
        r = r.push(ui.muted("Loading…"));
    }
    if pane.index.is_some() && !loading {
        r = r.push(tip(ui.small_btn("Copy", Some(Message::CopyPaneDocument(side))), "Copy this document, with changes, to the clipboard", ui));
    }
    if dirty && !loading {
        if pane.file_info.as_ref().is_some_and(|f| f.path.is_some()) {
            r = r.push(tip(ui.small_btn("Save", Some(Message::PaneSave(side, SaveAction::Overwrite))), "Overwrite the file", ui));
        }
        r = r.push(tip(ui.small_btn("Save As…", Some(Message::PaneSave(side, SaveAction::Copy))), "Save to a new file", ui));
    }
    if loaded || loading {
        r = r.push(tip(ui.small_btn("Clear", Some(Message::PaneClear(side))), "Unload this pane", ui));
    } else {
        r = r.push(ui.small_btn("Open", Some(Message::PaneOpen(side))));
        r = r.push(ui.small_btn("Paste", Some(Message::PanePaste(side))));
    }

    let bg = if active { ui.pal.selection_bg } else { ui.pal.bg_panel };
    let mut col = column![
        mouse_area(
            container(r).width(Length::Fill).padding([10, 12]).style(theme::fill_style(bg)),
        )
        .on_press(Message::PaneActivate(side))
        .interaction(iced::mouse::Interaction::Pointer),
    ]
    .width(Length::Fill);

    if let Some(e) = &pane.load_error {
        let mut err = row![ui.t(format!("Error: {e}")).color(theme::DELETED)].spacing(8).align_y(iced::Center);
        if pane.load_error_ctx.is_some() {
            err = err.push(ui.small_btn("Show context", Some(Message::PaneErrorCtx(side))));
        }
        col = col.push(container(err).padding([4, 12]));
    }
    col.into()
}

fn compare_panel<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let headers = row![
        container(pane_header(app, ui, Side::Left)).width(Length::FillPortion(1)),
        container(pane_header(app, ui, Side::Right)).width(Length::FillPortion(1)),
    ]
    .width(Length::Fill);

    let both = app.compare.left.index.is_some() && app.compare.right.index.is_some();
    let body: Element<'a, Message> = if !both {
        // Clicking the empty area below a header selects that pane, same as
        // clicking the header itself.
        let options = [
            ("⌘ O", "Open a file"),
            ("⌘ V", "Paste from clipboard"),
            ("drop", "Drag a file onto the pane"),
        ];
        let mut grid = column![].spacing(10);
        for (cap, desc) in options {
            grid = grid.push(
                row![
                    container(ui.keycaps(cap)).width(90).align_x(iced::Right),
                    ui.muted(desc),
                ]
                .spacing(14)
                .align_y(iced::Center),
            );
        }
        let hint = center(
            column![
                text("{ } ⇄ { }").size(28).font(Font::MONOSPACE).color(ui.pal.text_faint),
                space().height(10),
                text("Load JSON into both panes to compare").size(17).font(ui.font).color(ui.pal.text_primary),
                space().height(6),
                ui.muted("Click a pane, then:"),
                space().height(14),
                grid,
            ]
            .align_x(iced::Center),
        );
        stack![
            row![
                mouse_area(container(space()).width(Length::FillPortion(1)).height(Length::Fill))
                    .on_press(Message::PaneActivate(Side::Left)),
                mouse_area(container(space()).width(Length::FillPortion(1)).height(Length::Fill))
                    .on_press(Message::PaneActivate(Side::Right)),
            ]
            .width(Length::Fill)
            .height(Length::Fill),
            hint,
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    } else if let (Some(result), Some(tree)) = (&app.compare.result, &app.compare.tree) {
        let rows = Rows::Diff(DiffRows {
            result,
            expanded: &tree.expanded,
            selected: tree.selected,
            visible:  &tree.visible,
        });
        let view = TreeView::new(
            rows,
            app.settings.row_height(),
            app.settings.key_font(),
            app.settings.val_font(),
            ui.fs,
            ui.pal.dark,
            app.compare.generation,
            Message::DiffRow,
        );
        scrollable(view)
            .id(DIFF_SCROLL_ID)
            .on_scroll(Message::DiffScrolled)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::scrollable_style)
            .into()
    } else {
        center(ui.muted("Comparing…")).into()
    };

    let panel = column![headers, hdivider(), body].width(Length::Fill).height(Length::Fill);
    // Track the pointer so a file drop lands in the pane under it.
    mouse_area(panel).on_move(Message::CursorMoved).into()
}

// ─── AI side panel ───────────────────────────────────────────────────────────

fn ai_side_panel<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let has_doc = app.tree.is_some();
    let panel = ai::panel::view(&app.ai, &app.settings, has_doc, &ui.pal, ui.font, ui.fs).map(Message::Ai);
    let handle = mouse_area(
        container(space()).width(5).height(Length::Fill).style(theme::fill_style(ui.pal.border)),
    )
    .on_press(Message::AiResizeStart)
    .interaction(iced::mouse::Interaction::ResizingHorizontally);
    row![
        handle,
        container(panel)
            .width(app.ai.width)
            .height(Length::Fill)
            .padding([0, 10])
            .style(theme::panel_style),
    ]
    .height(Length::Fill)
    .into()
}

// ─── popup menus ─────────────────────────────────────────────────────────────

const MENU_W: f32 = 230.0;
const MENU_ITEM_H: f32 = 26.0;

/// One popup-menu entry.
enum Item {
    Action { label: String, shortcut: &'static str, msg: Option<Message> },
    Sub { label: &'static str, id: Submenu, children: Vec<(String, Message)> },
    Separator,
}

fn action(label: impl Into<String>, msg: Message) -> Item {
    Item::Action { label: label.into(), shortcut: "", msg: Some(msg) }
}

fn action_if(enabled: bool, label: impl Into<String>, shortcut: &'static str, msg: Message) -> Item {
    Item::Action { label: label.into(), shortcut, msg: enabled.then_some(msg) }
}

fn popup_layer<'a>(app: &'a App, ui: &Ui, overlay: Overlay) -> Element<'a, Message> {
    let items = match overlay.kind {
        OverlayKind::Row(n)     => row_menu_items(app, n),
        OverlayKind::DiffRow(n) => diff_menu_items(app, n),
        OverlayKind::MenuBar(m) => menubar_items(app, m),
    };

    // Estimate the height so the menu can be kept inside the window.
    let mut est_h = 8.0;
    for it in &items {
        est_h += match it {
            Item::Separator => 7.0,
            Item::Sub { id, children, .. } => {
                MENU_ITEM_H + if overlay.submenu == Some(*id) { children.len() as f32 * MENU_ITEM_H } else { 0.0 }
            }
            Item::Action { .. } => MENU_ITEM_H,
        };
    }
    let x = overlay.position.x.min((app.window_size.width - MENU_W - 8.0).max(0.0)).max(0.0);
    let y = overlay.position.y.min((app.window_size.height - est_h - 8.0).max(0.0)).max(0.0);

    let mut col = column![].width(Length::Fill).spacing(0);
    for it in items {
        match it {
            Item::Separator => {
                col = col.push(container(rule::horizontal(1).style(theme::divider_style)).padding([3, 4]));
            }
            Item::Action { label, shortcut, msg } => {
                col = col.push(menu_item(ui, label, shortcut, msg));
            }
            Item::Sub { label, id, children } => {
                let open = overlay.submenu == Some(id);
                col = col.push(
                    button(
                        row![
                            text(label).size(ui.small).font(ui.font),
                            space().width(Length::Fill),
                            text(if open { "▾" } else { "▸" }).size(ui.small),
                        ]
                        .align_y(iced::Center),
                    )
                    .width(Length::Fill)
                    .padding([4, 10])
                    .style(theme::menu_item_style)
                    .on_press(Message::OpenSubmenu(id)),
                );
                if open {
                    for (child_label, msg) in children {
                        col = col.push(
                            button(text(child_label).size(ui.small).font(ui.font))
                                .width(Length::Fill)
                                .padding(Padding { top: 4.0, right: 10.0, bottom: 4.0, left: 26.0 })
                                .style(theme::menu_item_style)
                                .on_press(Message::MenuPick(Box::new(msg))),
                        );
                    }
                }
            }
        }
    }

    let menu = container(col).width(MENU_W).padding(4).style(theme::card_style);

    stack![
        // Transparent click-catcher: any press outside the menu closes it.
        opaque(
            mouse_area(container(space()).width(Length::Fill).height(Length::Fill))
                .on_press(Message::CloseOverlay)
                .on_right_press(Message::CloseOverlay),
        ),
        container(opaque(menu)).padding(Padding { top: y, left: x, right: 0.0, bottom: 0.0 }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn menu_item<'a>(ui: &Ui, label: String, shortcut: &'static str, msg: Option<Message>) -> Element<'a, Message> {
    let mut r = row![text(label).size(ui.small).font(ui.font), space().width(Length::Fill)].align_y(iced::Center);
    if !shortcut.is_empty() {
        r = r.push(text(shortcut).size(ui.small).color(ui.pal.text_faint));
    }
    button(r)
        .width(Length::Fill)
        .padding([4, 10])
        .style(theme::menu_item_style)
        .on_press_maybe(msg.map(|m| Message::MenuPick(Box::new(m))))
        .into()
}

fn row_menu_items(app: &App, node_idx: u32) -> Vec<Item> {
    let mut items = Vec::new();
    let Some(t) = &app.tree else { return items };
    let index = &*t.index;
    let is_new = t.is_added(node_idx);
    let node = if is_new {
        crate::tree_view::synthetic_node(index, &t.added_items, node_idx)
    } else {
        index.nodes[node_idx as usize]
    };
    let kind = node.kind;
    let is_container = matches!(kind, NodeKind::Object | NodeKind::Array);
    let has_children = node.child_count > 0;
    let is_deleted = app.edit_overlay.get(&node_idx).map_or(false, |e| e.deleted);
    let is_root = node.parent == u32::MAX;
    let added_key: Option<&str> = if is_new { t.added_item(node_idx).key.as_deref() } else { None };
    let any_checked = !t.checked.is_empty();

    // Edit items — hidden for deleted nodes.
    if !is_deleted {
        if !is_container {
            items.push(action("Edit Value", Message::StartEdit(node_idx, EditField::Value)));
        }
        if node.key_len > 0 || added_key.is_some() {
            items.push(action("Edit Key", Message::StartEdit(node_idx, EditField::Key)));
        }
    }
    if is_container {
        let label = if kind == NodeKind::Array { "Add Item" } else { "Add Property" };
        items.push(action(label, Message::StartAdd(node_idx)));
    }
    if !is_root {
        items.push(action(if is_deleted { "Restore" } else { "Delete" }, Message::ToggleDelete(node_idx)));
    }
    items.push(Item::Separator);

    items.push(action("Copy Path", Message::CopyPath(node_idx)));
    let has_key = added_key.is_some() || node.key_len > 0 || node.array_index != u32::MAX;
    if has_key {
        items.push(action("Copy Key", Message::CopyKey(node_idx)));
    }
    items.push(action("Copy Value", Message::CopyValue(node_idx)));
    // Only while there are unsaved edits: copy the value with the edit
    // overlay applied. Pending adds live in `added_items`, so a document
    // whose only unsaved change is a new item is still dirty here.
    if !is_deleted && (app.edit_overlay != app.saved_overlay || !t.added_items.is_empty()) {
        items.push(action("Copy Modified Value", Message::CopyModifiedValue(node_idx)));
    }
    if is_container {
        items.push(Item::Sub {
            label: "Copy as Code",
            id: Submenu::CopyAsCode,
            children: codegen::LANGUAGES
                .iter()
                .map(|&lang| (lang.label().to_owned(), Message::CopyAsCode(node_idx, lang)))
                .collect(),
        });
    }

    // Find similar — real nodes only; pending adds aren't in the index.
    if !is_new {
        items.push(Item::Separator);
        let has_key = node.key_len > 0;
        let children: Vec<(String, Message)> = [
            search::SimilarBy::KeyAndValue,
            search::SimilarBy::Key,
            search::SimilarBy::Value,
        ]
        .into_iter()
        // Key-based modes need a key: every array element would otherwise
        // match every other keyless node.
        .filter(|by| has_key || *by == search::SimilarBy::Value)
        .map(|by| (by.label().to_owned(), Message::FindSimilar(node_idx, by)))
        .collect();
        items.push(Item::Sub { label: "Find Similar Items", id: Submenu::FindSimilar, children });
    }

    if is_container && has_children {
        items.push(Item::Separator);
        items.push(action("Expand All", Message::ExpandRecursive(node_idx)));
        items.push(action("Collapse All", Message::CollapseRecursive(node_idx)));
    }

    items.push(Item::Separator);
    let mut export_children = Vec::new();
    if any_checked {
        export_children.push(("Selected nodes as JSON".to_owned(), Message::Export(ExportScope::Selection, ExportFormat::Json)));
        export_children.push(("Selected nodes as CSV".to_owned(), Message::Export(ExportScope::Selection, ExportFormat::Csv)));
    }
    // Pending added items aren't real tree nodes, so per-node export doesn't
    // apply to them.
    if !is_new {
        export_children.push(("This node as JSON".to_owned(), Message::Export(ExportScope::Node(node_idx), ExportFormat::Json)));
        export_children.push(("This node as CSV".to_owned(), Message::Export(ExportScope::Node(node_idx), ExportFormat::Csv)));
    }
    export_children.push(("Whole file as JSON".to_owned(), Message::Export(ExportScope::File, ExportFormat::Json)));
    export_children.push(("Whole file as CSV".to_owned(), Message::Export(ExportScope::File, ExportFormat::Csv)));
    items.push(Item::Sub { label: "Export", id: Submenu::Export, children: export_children });

    items
}

fn diff_menu_items(app: &App, node_idx: u32) -> Vec<Item> {
    let mut items = Vec::new();
    let Some(result) = &app.compare.result else { return items };
    let dn = &result.nodes[node_idx as usize];
    if dn.left_idx().is_some() {
        items.push(action("Copy Left Value", Message::CopyDiffValue(Side::Left, node_idx)));
    }
    if dn.right_idx().is_some() {
        items.push(action("Copy Right Value", Message::CopyDiffValue(Side::Right, node_idx)));
    }
    items.push(action("Copy Path", Message::CopyDiffPath(node_idx)));
    items.push(Item::Separator);
    items.push(action("Copy Left Document", Message::CopyPaneDocument(Side::Left)));
    items.push(action("Copy Right Document", Message::CopyPaneDocument(Side::Right)));
    // Copying a value that's absent on the source side deletes it.
    let to_right = merge::can_copy(result, node_idx, Side::Right);
    let to_left = merge::can_copy(result, node_idx, Side::Left);
    if to_right || to_left {
        items.push(Item::Separator);
    }
    if to_right {
        items.push(action(
            if dn.left_idx().is_some() { "Copy to Right →" } else { "Delete from Right" },
            Message::CompareCopy(node_idx, Side::Right),
        ));
    }
    if to_left {
        items.push(action(
            if dn.right_idx().is_some() { "← Copy to Left" } else { "Delete from Left" },
            Message::CompareCopy(node_idx, Side::Left),
        ));
    }
    items
}

fn menubar_items(app: &App, id: MenuBarId) -> Vec<Item> {
    let has_tree = app.tree.is_some();
    // In Compare, Save / Save As act on the active pane.
    let (dirty, can_over) = match app.mode {
        AppMode::Viewer => (app.is_dirty(), app.can_overwrite()),
        AppMode::Compare => {
            let pane = app.compare.pane(app.compare.active_pane);
            (pane.is_dirty(), pane.file_info.as_ref().is_some_and(|f| f.path.is_some()))
        }
    };
    let compare = app.mode == AppMode::Compare;
    match id {
        MenuBarId::File => vec![
            action_if(true, "Open…", "⌘O", Message::OpenFile),
            action_if(true, "Open URL…", "⌘L", Message::OpenUrlDialog),
            action_if(true, "Paste JSON / JWT", "⌘V", Message::RequestPaste),
            Item::Separator,
            Item::Sub {
                label: "Export File",
                id: Submenu::ExportFile,
                children: if has_tree {
                    vec![
                        ("As JSON".to_owned(), Message::Export(ExportScope::File, ExportFormat::Json)),
                        ("As CSV".to_owned(), Message::Export(ExportScope::File, ExportFormat::Csv)),
                    ]
                } else {
                    Vec::new()
                },
            },
            Item::Separator,
            action_if(dirty && can_over, "Save", "⌘S", Message::Save(SaveAction::Overwrite)),
            action_if(dirty, if compare { "Save As…" } else { "Save a Copy" }, "⇧⌘S", Message::Save(SaveAction::Copy)),
            action_if(dirty && !compare, "Discard Changes", "", Message::DiscardChanges),
            Item::Separator,
            action_if(true, "Settings", "⌘,", Message::OpenDialog(Dialog::Settings)),
        ],
        MenuBarId::Edit => vec![
            action_if(app.can_undo(), "Undo", "⌘Z", Message::Undo),
            action_if(app.can_redo(), "Redo", "⇧⌘Z", Message::Redo),
        ],
        MenuBarId::View => vec![
            action_if(has_tree, "Collapse All", "⌥C", Message::CollapseAll),
            action_if(has_tree, "Expand All", "⌥X", Message::ExpandAll),
            Item::Separator,
            action_if(true, "Search", "⌘F", Message::FocusSearch),
        ],
        MenuBarId::Help => vec![
            action("Keyboard Shortcuts", Message::OpenDialog(Dialog::Help)),
            action("Search Syntax", Message::OpenDialog(Dialog::SearchHelp)),
            Item::Separator,
            action("About JSON Viewer", Message::OpenDialog(Dialog::About)),
        ],
    }
}

// ─── modal dialogs ───────────────────────────────────────────────────────────

/// Wrap dialog content in a centred card over a dimmed, click-to-dismiss backdrop.
fn modal<'a>(content: Element<'a, Message>, on_blur: Message, max_h: f32) -> Element<'a, Message> {
    let card = container(scrollable(content).style(theme::scrollable_style))
        .padding(16)
        .max_height(max_h)
        .style(theme::card_style);
    opaque(
        mouse_area(center(opaque(card)).style(theme::backdrop_style))
            .on_press(on_blur),
    )
}

fn dialog_title<'a>(title: &'a str, close: Message) -> Element<'a, Message> {
    row![
        text(title).size(18),
        space().width(Length::Fill),
        button(text("✕").size(17)).style(theme::flat_button_style).on_press(close),
    ]
    .align_y(iced::Center)
    .into()
}

fn modal_layer<'a>(app: &'a App, ui: &Ui) -> Option<Element<'a, Message>> {
    let max_h = (app.window_size.height - 60.0).max(200.0);
    if let Some(state) = &app.editing_node {
        return Some(modal(edit_dialog(app, ui, state), Message::EditCancel, max_h));
    }
    if let Some(state) = &app.adding_item {
        return Some(modal(add_dialog(app, ui, state), Message::AddCancel, max_h));
    }
    let dialog = app.dialog?;
    let content: Element<'a, Message> = match dialog {
        Dialog::Settings => settings::view(&app.settings, app.default_status, &app.ai_settings_ui, &ui.pal)
            .map(Message::Settings),
        Dialog::Help => help_dialog(ui),
        Dialog::SearchHelp => search_help_dialog(ui),
        Dialog::About => about_dialog(app, ui),
        Dialog::Url => url_dialog(app, ui),
        Dialog::ErrorContext(side) => {
            let (error, ctx) = match side {
                None => (app.load_error.as_deref(), app.load_error_ctx.as_ref()),
                Some(s) => {
                    let pane = app.compare.pane(s);
                    (pane.load_error.as_deref(), pane.load_error_ctx.as_ref())
                }
            };
            let title = match side {
                None => "Parse Error Context".to_owned(),
                Some(Side::Left) => "Parse Error Context — Left".to_owned(),
                Some(Side::Right) => "Parse Error Context — Right".to_owned(),
            };
            error_context_dialog(ui, title, error, ctx)
        }
    };
    Some(modal(content, Message::CloseDialog, max_h))
}

/// Caption + hairline that introduces a group of rows in a help table.
fn help_section<'a>(ui: &Ui, title: &'a str) -> Element<'a, Message> {
    column![
        text(title.to_uppercase()).size(11).font(ui.font).color(ui.pal.text_muted),
        space().height(4),
        hdivider(),
    ]
    .into()
}

/// Shared layout for the help dialogs: a title, then sections of a two-column
/// table with zebra-striped rows. `lead` renders the left cell of each row
/// (keycaps for shortcuts, a code chip for search syntax).
fn help_table<'a>(
    ui: &Ui,
    title: &'a str,
    width: f32,
    lead_width: f32,
    sections: &'a [(&'a str, &'a [(&'a str, &'a str)])],
    lead: impl Fn(&'a str) -> Element<'a, Message>,
) -> Element<'a, Message> {
    let mut col = column![dialog_title(title, Message::CloseDialog)].width(width);
    for (si, (section, rows)) in sections.iter().enumerate() {
        col = col.push(space().height(if si == 0 { 12 } else { 18 }));
        col = col.push(help_section(ui, section));
        for (ri, (key, desc)) in rows.iter().enumerate() {
            let cells = row![
                container(lead(key)).width(lead_width),
                text(*desc).size(13).font(ui.font).color(ui.pal.text_primary).width(Length::Fill),
            ]
            .spacing(16)
            .align_y(iced::Center);
            let mut cell = container(cells).padding([5, 8]).width(Length::Fill);
            if ri % 2 == 0 {
                cell = cell.style(theme::bubble_style(ui.pal.stripe_bg));
            }
            col = col.push(cell);
        }
    }
    col.into()
}

/// Inline code chip for a search-syntax example.
fn code_chip<'a>(ui: &Ui, code: &'a str) -> Element<'a, Message> {
    container(text(code).size(13).font(Font::MONOSPACE).color(ui.pal.text_primary))
        .padding([2, 7])
        .style(theme::keycap_style)
        .into()
}

fn help_dialog<'a>(ui: &Ui) -> Element<'a, Message> {
    const SECTIONS: &[(&str, &[(&str, &str)])] = &[
        ("File", &[
            ("⌘ O", "Open file"),
            ("⌘ L", "Open URL"),
            ("⌘ V", "Paste JSON / JWT / curl from clipboard"),
            ("⌘ ,", "Settings"),
        ]),
        ("Navigation", &[
            ("↑ / ↓", "Select previous / next row"),
            ("← / →", "Collapse / expand node"),
            ("Page Up/Dn", "Jump 20 rows"),
            ("Home / End", "Jump to first / last row"),
        ]),
        ("Tree", &[
            ("⌥ C", "Collapse all"),
            ("⌥ X", "Expand all"),
        ]),
        ("Search", &[
            ("⌘ F", "Focus search box"),
            ("Enter", "Next result"),
            ("⌘ G", "Next result"),
            ("⌘ ⇧ G", "Previous result"),
        ]),
        ("Editing", &[
            ("Double-click", "Edit leaf value"),
            ("F2", "Edit selected value"),
            ("⌘ Z", "Undo"),
            ("⇧ ⌘ Z", "Redo"),
            ("⌘ S", "Save (overwrite original)"),
            ("⇧ ⌘ S", "Save a Copy"),
        ]),
    ];
    help_table(ui, "Keyboard Shortcuts", 440.0, 150.0, SECTIONS, |key| ui.keycaps(key))
}

fn search_help_dialog<'a>(ui: &Ui) -> Element<'a, Message> {
    const SECTIONS: &[(&str, &[(&str, &str)])] = &[
        ("Text", &[
            ("error", "Keys or values containing \"error\""),
            ("\"foo bar\"", "Quote to match text with spaces"),
        ]),
        ("Target", &[
            ("key:name", "Keys containing \"name\""),
            ("value:err", "Values containing \"err\""),
        ]),
        ("Comparison", &[
            ("age > 30", "Key \"age\" with numeric value > 30"),
            ("price <= 9.99", "Operators:  =  !=  <  <=  >  >="),
            ("status = active", "Exact string equality"),
            ("value > 100", "Any key with numeric value > 100"),
            ("date >= 2024-01-01", "Non-numbers compare alphabetically"),
        ]),
        ("Combining", &[
            ("key:user value > 1000", "Space-separated parts — all must match"),
        ]),
        ("Regex", &[
            (".* toggle", "Regex on keys and values (disables the above)"),
        ]),
    ];
    help_table(ui, "Search Syntax", 520.0, 190.0, SECTIONS, |code| code_chip(ui, code))
}

fn about_dialog<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let update_badge: Element<'a, Message> = match app.pending_update() {
        Some(info) => column![
            text(format!("Update available: v{}", info.version)).size(13).color(theme::WARN_ORANGE),
            button(text("View release").size(13).color(ui.pal.accent))
                .style(theme::flat_button_style)
                .on_press(Message::OpenUrl(info.html_url.clone())),
        ]
        .spacing(4)
        .align_x(iced::Center)
        .into(),
        None => text("Up to date").size(13).color(theme::OK_GREEN).into(),
    };
    column![
        dialog_title("About", Message::CloseDialog),
        space().height(12),
        column![
            text("JSON Viewer").size(22),
            space().height(4),
            text(concat!("Version ", env!("CARGO_PKG_VERSION"))).size(12).color(ui.pal.text_muted),
            space().height(6),
            update_badge,
            space().height(12),
            hdivider(),
            space().height(12),
            text("A fast, lightweight JSON tree viewer with advanced search, BiDi text support, and more.")
                .size(13)
                .font(ui.font),
            space().height(12),
            hdivider(),
            space().height(12),
            text("Created by").size(12).color(ui.pal.text_muted),
            text("Evyatar Shalom").size(14),
            space().height(12),
        ]
        .align_x(iced::Center)
        .width(340),
    ]
    .width(340)
    .into()
}

fn url_dialog<'a>(app: &'a App, ui: &Ui) -> Element<'a, Message> {
    let text_value = app.url_editor.text();
    let parsed = url_parse::parse_request(&text_value);
    let can_open = parsed.is_some();

    let mut col = column![
        dialog_title("Open URL", Message::CloseDialog),
        space().height(4),
        text("Paste a URL, curl command, or fetch() call:").size(13).font(ui.font),
        space().height(6),
        text_editor(&app.url_editor)
            .id(URL_ID)
            .placeholder("https://api.example.com/data\n— or —\ncurl -H \"Authorization: Bearer …\" https://api.example.com/data")
            .on_action(Message::UrlEdit)
            .key_binding(|kp| {
                use iced::keyboard::key::Named;
                use iced::keyboard::Key;
                if kp.modifiers.command() && matches!(kp.key.as_ref(), Key::Named(Named::Enter)) {
                    Some(text_editor::Binding::Custom(Message::UrlOpen))
                } else {
                    text_editor::Binding::from_key_press(kp)
                }
            })
            .font(Font::MONOSPACE)
            .size(13)
            .height(100)
            .style(dialog_editor_style),
        space().height(6),
    ]
    .width(520);

    if let Some(req) = &parsed {
        col = col.push(
            row![text("→").size(12).color(ui.pal.text_muted), text(req.url.clone()).size(12).color(ui.pal.accent)]
                .spacing(6),
        );
        if !req.headers.is_empty() {
            col = col.push(text(format!("{} header(s) detected", req.headers.len())).size(12).color(ui.pal.text_muted));
        }
        col = col.push(space().height(4));
    }

    col = col.push(
        row![
            ui.btn("Open", can_open.then_some(Message::UrlOpen)),
            ui.btn("Cancel", Some(Message::CloseDialog)),
            space().width(Length::Fill),
            text("⌘↵ to open").size(12).color(ui.pal.text_faint),
        ]
        .spacing(6)
        .align_y(iced::Center),
    );
    col.into()
}

fn dialog_editor_style(theme: &iced::Theme, status: text_editor::Status) -> text_editor::Style {
    let pal = Palette::of(theme);
    let border_color = match status {
        text_editor::Status::Focused { .. } => pal.accent,
        text_editor::Status::Hovered => pal.text_muted,
        _ => pal.border,
    };
    text_editor::Style {
        background: iced::Background::Color(pal.bg_search),
        border: iced::Border { color: border_color, width: 1.0, radius: 6.0.into() },
        placeholder: pal.text_faint,
        value: pal.text_primary,
        selection: if pal.dark { theme::TEXT_SELECT_BG } else { theme::LIGHT_SELECTION_BG },
    }
}

fn edit_dialog<'a>(app: &'a App, ui: &Ui, state: &'a crate::EditingState) -> Element<'a, Message> {
    let title = match state.field {
        EditField::Key   => "Edit Key",
        EditField::Value => "Edit Value",
    };
    let path = app.tree.as_ref().map(|tree| {
        if tree.is_added(state.node_idx) {
            added_path(&tree.index, &tree.added_items, state.node_idx)
        } else {
            build_path(&tree.index.nodes, &tree.index, state.node_idx)
        }
    });

    let mut col = column![dialog_title(title, Message::EditCancel), space().height(4)].width(400);
    if let Some(path) = path {
        col = col.push(text(path).size(12).font(Font::MONOSPACE).color(ui.pal.text_muted));
        col = col.push(space().height(4));
    }
    col = col.push(
        text_input("", &state.text)
            .id(EDIT_ID)
            .on_input(Message::EditText)
            .on_submit(Message::EditCommit)
            .font(Font::MONOSPACE)
            .size(ui.fs)
            .width(Length::Fill)
            .style(theme::input_style),
    );
    col = col.push(space().height(8));
    col = col.push(
        row![
            ui.btn("OK", Some(Message::EditCommit)),
            ui.btn("Cancel", Some(Message::EditCancel)),
        ]
        .spacing(6),
    );
    col.into()
}

fn add_dialog<'a>(app: &'a App, ui: &Ui, state: &'a crate::AddingState) -> Element<'a, Message> {
    let Some(tree) = &app.tree else { return space().into() };
    let is_object = tree.kind_of(state.parent) == index::NodeKind::Object;
    let path = if tree.is_added(state.parent) {
        added_path(&tree.index, &tree.added_items, state.parent)
    } else {
        build_path(&tree.index.nodes, &tree.index, state.parent)
    };
    let value_text = state.value.text();
    let parsed = serde_json::from_str::<serde_json::Value>(&value_text);
    let value_valid = parsed.is_ok();
    let key_valid = !is_object || !state.key.trim().is_empty();
    let valid = value_valid && key_valid;
    let single_line = !value_text.contains('\n');
    let title = if is_object { "Add Property" } else { "Add Item" };
    let hint = if is_object { format!("{path}.…") } else { format!("{path}[…]") };

    let mut col = column![
        dialog_title(title, Message::AddCancel),
        space().height(4),
        text(hint).size(12).font(Font::MONOSPACE).color(ui.pal.text_muted),
        space().height(4),
    ]
    .spacing(4)
    .width(480);

    if is_object {
        col = col.push(text("Key").size(12).color(ui.pal.text_muted));
        col = col.push(
            text_input("property name", &state.key)
                .id(ADD_KEY_ID)
                .on_input(Message::AddKey)
                .on_submit(Message::AddCommit)
                .font(Font::MONOSPACE)
                .size(ui.fs)
                .width(Length::Fill)
                .style(theme::input_style),
        );
        col = col.push(space().height(6));
    }
    col = col.push(
        row![
            text("Value (any JSON)").size(12).color(ui.pal.text_muted),
            space().width(Length::Fill),
            ui.small_btn("Format", value_valid.then_some(Message::AddFormat)),
        ]
        .align_y(iced::Center),
    );
    col = col.push(
        text_editor(&state.value)
            .id(ADD_VALUE_ID)
            .placeholder("42, \"text\", true, null — or an object / array:\n{\n  \"id\": 1,\n  \"tags\": [\"a\", \"b\"]\n}")
            .on_action(Message::AddValue)
            .key_binding(move |kp| {
                use iced::keyboard::key::Named;
                use iced::keyboard::Key;
                // ⌘↵ always commits, and so does a bare ↵ on a single-line
                // value (the old scalar flow). Once the value spans lines —
                // a pasted or typed object — ↵ inserts a newline instead, and
                // ⇧↵ always does.
                let enter = matches!(kp.key.as_ref(), Key::Named(Named::Enter));
                let commit = enter
                    && !kp.modifiers.shift()
                    && (kp.modifiers.command() || single_line);
                if commit {
                    Some(text_editor::Binding::Custom(Message::AddCommit))
                } else {
                    text_editor::Binding::from_key_press(kp)
                }
            })
            .font(Font::MONOSPACE)
            .size(ui.fs)
            .height(140)
            .style(dialog_editor_style),
    );
    col = col.push(if value_text.trim().is_empty() {
        text("").size(12)
    } else {
        match &parsed {
            Ok(v)  => text(describe_json_value(v)).size(12).color(ui.pal.text_muted),
            Err(e) => text(format!("Not valid JSON — {e}")).size(12).color(theme::DELETED),
        }
    });
    col = col.push(space().height(8));
    col = col.push(
        row![
            ui.btn("Add", valid.then_some(Message::AddCommit)),
            ui.btn("Cancel", Some(Message::AddCancel)),
            space().width(Length::Fill),
            text(if single_line { "↵ to add" } else { "⌘↵ to add · ⇧↵ for a newline" })
                .size(12)
                .color(ui.pal.text_faint),
        ]
        .spacing(6)
        .align_y(iced::Center),
    );
    col.into()
}

/// One-line summary of a parsed value, shown under the Add dialog's editor so
/// it's clear an object / array was recognised as a tree, not raw text.
fn describe_json_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => match m.len() {
            1 => "Object with 1 key".to_owned(),
            n => format!("Object with {n} keys"),
        },
        serde_json::Value::Array(a) => match a.len() {
            1 => "Array with 1 item".to_owned(),
            n => format!("Array with {n} items"),
        },
        serde_json::Value::String(_) => "String".to_owned(),
        serde_json::Value::Number(_) => "Number".to_owned(),
        serde_json::Value::Bool(_)   => "Boolean".to_owned(),
        serde_json::Value::Null      => "Null".to_owned(),
    }
}

/// Renders the bytes surrounding a parse error in a code-block-style
/// container, preserving line breaks and highlighting the errored byte.
fn error_context_dialog<'a>(
    ui: &Ui,
    title: String,
    error: Option<&'a str>,
    ctx: Option<&'a crate::loader::ErrorContext>,
) -> Element<'a, Message> {
    let mut col = column![
        dialog_title_owned(title, Message::CloseDialog),
        space().height(6),
        text("Source surrounding the error (highlighting where parsing failed):").size(13).font(ui.font),
        space().height(8),
    ]
    .width(660);
    if let Some(error) = error {
        col = col.push(text(error).size(13).color(theme::DELETED));
        col = col.push(space().height(8));
    }
    if let Some(ec) = ctx {
        // A highlighted bare newline is invisible; show a visible marker before it.
        let at_display: String = if ec.at == "\n" { "↵\n".to_owned() } else { ec.at.clone() };
        let block: iced::widget::text::Rich<'a, (), Message> = rich_text![
            span(ec.before.as_str()).color(ui.pal.text_primary),
            span(at_display).color(Color::WHITE).background(Color::from_rgb8(200, 50, 50)),
            span(ec.after.as_str()).color(ui.pal.text_primary),
        ]
        .font(Font::MONOSPACE)
        .size(12);
        col = col.push(
            container(
                scrollable(container(block).padding(8))
                    .direction(scrollable::Direction::Both {
                        vertical: scrollable::Scrollbar::new(),
                        horizontal: scrollable::Scrollbar::new(),
                    })
                    .width(Length::Fill)
                    .height(320)
                    .style(theme::scrollable_style),
            )
            .width(Length::Fill)
            .style(theme::pill_style),
        );
    }
    col = col.push(space().height(6));
    col.into()
}

fn dialog_title_owned<'a>(title: String, close: Message) -> Element<'a, Message> {
    row![
        text(title).size(18),
        space().width(Length::Fill),
        button(text("✕").size(17)).style(theme::flat_button_style).on_press(close),
    ]
    .align_y(iced::Center)
    .into()
}
