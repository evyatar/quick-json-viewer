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
mod tree_view;
mod update;
mod url_parse;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{scrollable, text_editor};
use iced::{event, keyboard, mouse, window, Element, Point, Size, Subscription, Task, Theme};

use ai::panel::AiPanelMsg;
use export::AddedItem;
use loader::LoadMsg;
use settings::{Settings, SettingsMsg};
use tree::TreeState;
use tree_view::{Region, RowEvent};

// ─── widget ids ──────────────────────────────────────────────────────────────

pub const TREE_SCROLL_ID: &str = "tree_scroll";
pub const DIFF_SCROLL_ID: &str = "diff_scroll";
pub const SEARCH_ID: &str = "search_input";
pub const EDIT_ID: &str = "edit_input";
pub const ADD_KEY_ID: &str = "add_key_input";
pub const ADD_VALUE_ID: &str = "add_value_input";
pub const URL_ID: &str = "url_input";

/// Open a URL in the default browser (release pages, markdown links).
pub fn open_in_browser(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

// ─── row actions ─────────────────────────────────────────────────────────────

/// What an export operates on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportScope {
    /// The whole document.
    File,
    /// A single node's subtree.
    Node(u32),
    /// The checked multi-selection (pruned common-ancestor subtree).
    Selection,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportFormat {
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditField { Key, Value }

/// Which save operation a UI control is requesting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveAction { Overwrite, Copy }

struct EditingState {
    node_idx: u32,
    field:    EditField,
    text:     String,
}

/// State for the "Add Item" / "Add Property" dialog: the target container,
/// the key typed so far (only used when the target is an Object), and the
/// raw JSON value text typed so far.
struct AddingState {
    parent: u32,
    key:    String,
    /// Multi-line so a whole object / array can be pasted or typed in.
    value:  text_editor::Content,
}

impl AddingState {
    fn new(parent: u32) -> Self {
        Self { parent, key: String::new(), value: text_editor::Content::new() }
    }

    fn text(&self) -> String {
        self.value.text()
    }

    #[cfg(test)]
    fn set_value(&mut self, text: &str) {
        self.value = text_editor::Content::with_text(text);
    }

    /// Re-indent the typed value when it already parses as JSON; a no-op
    /// otherwise so the user's in-progress text is never mangled.
    fn format(&mut self) {
        let text = self.text();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Ok(pretty) = serde_json::to_string_pretty(&v) {
                self.value = text_editor::Content::with_text(&pretty);
            }
        }
    }
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
/// or a batch of changes applied together (an AI changeset) that
/// undoes/redoes as one unit.
#[derive(Clone)]
enum UndoAction {
    Overlay(UndoEntry),
    /// `rows` is how many entries the add pushed onto `TreeState::added_items`
    /// — a container value expands into one row per element, and undo has to
    /// take the whole group back off.
    Add { parent: u32, key: Option<String>, raw_value: String, rows: usize },
    /// An AI changeset: overlay edits plus any items it appended. `adds` are
    /// always the most recently pushed pending items, so undoing pops them
    /// in reverse (same LIFO invariant as `UndoAction::Add`); `add_rows` is
    /// their combined row count.
    Batch { entries: Vec<UndoEntry>, adds: Vec<AddedItem>, add_rows: usize },
}

// ─── app state ───────────────────────────────────────────────────────────────

/// Top-level view: explore one document, or compare two.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppMode {
    Viewer,
    Compare,
}

/// Which pane of the Compare view is the target for Open / Paste / drop.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Side {
    #[default]
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
    /// a busy note instead of freezing; the result is collected in `poll_diff`.
    diff_rx:            Option<std::sync::mpsc::Receiver<diff::DiffResult>>,
    active_pane:        Side,
    needs_rediff:       bool,
    show_only_diffs:    bool,
    /// Which diff-status types are shown; toggled by clicking the counters.
    filter:             diff::StatusFilter,
    /// Bumped per computed diff so the row widget resets its cached widths.
    generation:         u64,
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

/// Modal dialogs. Only one is shown at a time; the edit / add dialogs are
/// driven by `editing_node` / `adding_item` and take precedence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialog {
    Settings,
    Help,
    SearchHelp,
    About,
    Url,
    /// Parse-error context for the viewer (`None`) or a compare pane.
    ErrorContext(Option<Side>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuBarId { File, Edit, View, Help }

impl MenuBarId {
    pub const ALL: [MenuBarId; 4] = [MenuBarId::File, MenuBarId::Edit, MenuBarId::View, MenuBarId::Help];
    pub fn label(self) -> &'static str {
        match self {
            MenuBarId::File => "File",
            MenuBarId::Edit => "Edit",
            MenuBarId::View => "View",
            MenuBarId::Help => "Help",
        }
    }
}

/// A nested section of a popup menu, expanded inline when picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Submenu {
    CopyAsCode,
    FindSimilar,
    Export,
    ExportFile,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayKind {
    /// Context menu for a viewer row.
    Row(u32),
    /// Context menu for a compare row.
    DiffRow(u32),
    /// Dropdown of the in-app menu bar.
    MenuBar(MenuBarId),
}

/// A popup (context menu or menu-bar dropdown) anchored at a window position.
#[derive(Clone, Copy, Debug)]
pub struct Overlay {
    pub kind:     OverlayKind,
    pub position: Point,
    pub submenu:  Option<Submenu>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffOpt {
    IgnoreCase,
    ArrayOrder,
    NullMissing,
    TypeCoercion,
    Trim,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind { Changed, Added, Removed }

#[derive(Clone, Debug)]
pub enum Message {
    // ── runtime plumbing ──
    Tick,
    MenuWake,
    Event(iced::Event, event::Status, window::Id),
    SystemTheme(iced::theme::Mode),
    Noop,

    // ── toolbar / global actions ──
    SetMode(AppMode),
    OpenFile,
    OpenUrlDialog,
    RequestPaste,
    Pasted(Option<String>),
    ToggleMultiSelect,
    ToggleAi,
    SearchInput(String),
    SearchSubmit,
    SearchClear,
    ToggleRegex,
    SearchPrev,
    SearchNext,
    FocusSearch,
    CollapseAll,
    ExpandAll,
    Undo,
    Redo,
    CopySelected,
    OpenDialog(Dialog),
    CloseDialog,
    ToggleSettings,

    // ── compare ──
    DiffFilter(DiffKind),
    ComparePrev,
    CompareNext,
    ToggleOnlyDiffs,
    DiffOption(DiffOpt),
    IgnoreKeys(String),
    IgnorePattern(String),
    PaneActivate(Side),
    PaneClear(Side),
    PaneOpen(Side),
    PanePaste(Side),
    PaneErrorCtx(Side),
    CursorMoved(Point),

    // ── trees ──
    Row(RowEvent),
    DiffRow(RowEvent),
    TreeScrolled(scrollable::Viewport),
    DiffScrolled(scrollable::Viewport),
    MenuBar(MenuBarId),
    OpenSubmenu(Submenu),
    CloseOverlay,
    /// A popup-menu item was picked: close the popup, then apply the message.
    MenuPick(Box<Message>),
    BreadcrumbJump(u32),

    // ── row / document actions ──
    StartEdit(u32, EditField),
    StartAdd(u32),
    ToggleDelete(u32),
    ExpandRecursive(u32),
    CollapseRecursive(u32),
    FindSimilar(u32, search::SimilarBy),
    CopyPath(u32),
    CopyKey(u32),
    CopyValue(u32),
    CopyModifiedValue(u32),
    CopyAsCode(u32, codegen::CodeLanguage),
    CopyDiffValue(Side, u32),
    CopyDiffPath(u32),
    Export(ExportScope, ExportFormat),
    Save(SaveAction),
    DiscardChanges,
    ClearDocument,

    // ── dialogs ──
    Settings(SettingsMsg),
    UrlEdit(text_editor::Action),
    UrlOpen,
    EditText(String),
    EditCommit,
    EditCancel,
    AddKey(String),
    AddValue(text_editor::Action),
    AddFormat,
    AddCommit,
    AddCancel,

    // ── update banner ──
    DismissUpdate,
    UpgradeNow,
    CopyBrewCommand,
    OpenUrl(String),

    // ── AI panel ──
    Ai(AiPanelMsg),
    AiResizeStart,
    /// Pointer moved while the AI panel's resize handle is held.
    AiResizeDrag(Point),

    // ── dev: self-screenshot (JSONVIEWER_SCREENSHOT=path[,action]) ──
    DebugShot,
    DebugShotTaken(window::Screenshot),
}

struct App {
    load_rx:        Option<std::sync::mpsc::Receiver<LoadMsg>>,
    load_progress:  f32,
    load_error:     Option<String>,
    load_error_ctx: Option<loader::ErrorContext>,
    tree:           Option<TreeState>,
    search_input:   String,
    search_pending: Option<std::thread::JoinHandle<Option<Vec<u32>>>>,
    /// Raised to abort the in-flight search thread (results would be stale).
    search_cancel:  Arc<std::sync::atomic::AtomicBool>,
    /// Debounce deadline — typing reschedules; the search fires only after
    /// the input has been quiet briefly, instead of one full index scan per
    /// keystroke.
    search_debounce_until: Option<Instant>,
    file_info:      Option<FileInfo>,
    settings:       Settings,
    default_status: settings::DefaultStatus,
    dialog:         Option<Dialog>,
    overlay:        Option<Overlay>,
    url_editor:     text_editor::Content,
    type_ahead:     String,
    type_ahead_time: Option<Instant>,
    mode:           AppMode,
    compare:        CompareState,
    update_rx:            Option<std::sync::mpsc::Receiver<update::UpdateMsg>>,
    update_available:     Option<update::ReleaseInfo>,
    editing_node:  Option<EditingState>,
    adding_item:   Option<AddingState>,
    edit_overlay:  std::collections::HashMap<u32, export::NodeEdit>,
    /// Snapshot of `edit_overlay` at the last overwrite-save; edits matching it
    /// are considered persisted (no dirty marker). Empty = nothing saved yet.
    saved_overlay: std::collections::HashMap<u32, export::NodeEdit>,
    undo_stack:    Vec<UndoAction>,
    redo_stack:    Vec<UndoAction>,
    /// In-flight background export/save; result polled each tick so large
    /// documents serialize + write without freezing the UI.
    bg_write_rx:   Option<std::sync::mpsc::Receiver<Result<BgWriteDone, String>>>,
    install_watcher_rx:   Option<std::sync::mpsc::Receiver<update::UpdateMsg>>,
    /// BYOK AI assistant panel (chat state, in-flight turn, pending changeset).
    ai:             ai::panel::AiPanelState,
    /// Transient UI state for the settings window's AI section (key buffer).
    ai_settings_ui: ai::panel::AiSettingsUi,
    ai_resizing:    bool,
    /// Whether the OS prefers a dark appearance (drives `Theme::Auto`).
    system_dark:    bool,
    window_size:    Size,
    window_id:      Option<window::Id>,
    /// Last known (scroll offset y, viewport height) of the two row lists.
    tree_viewport:  Option<(f32, f32)>,
    diff_viewport:  Option<(f32, f32)>,
    /// Bumped whenever a different document is loaded into the viewer.
    doc_generation: u64,
    /// Last pointer x in Compare mode — picks the drop-target pane.
    cursor_x:       Option<f32>,
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
            tree:           None,
            search_input:   String::new(),
            search_pending: None,
            search_cancel:  Arc::new(std::sync::atomic::AtomicBool::new(false)),
            search_debounce_until: None,
            file_info:      None,
            settings:       Settings::default(),
            default_status: settings::DefaultStatus::Untried,
            dialog:         None,
            overlay:        None,
            url_editor:     text_editor::Content::new(),
            type_ahead:     String::new(),
            type_ahead_time: None,
            mode:           AppMode::Viewer,
            compare:        CompareState::default(),
            update_rx:            None,
            update_available:     None,
            editing_node:  None,
            adding_item:   None,
            edit_overlay:  std::collections::HashMap::new(),
            saved_overlay: std::collections::HashMap::new(),
            undo_stack:    Vec::new(),
            redo_stack:    Vec::new(),
            bg_write_rx:   None,
            install_watcher_rx:   None,
            ai:             ai::panel::AiPanelState::default(),
            ai_settings_ui: ai::panel::AiSettingsUi::default(),
            ai_resizing:    false,
            system_dark:    true,
            window_size:    Size::new(1200.0, 800.0),
            window_id:      None,
            tree_viewport:  None,
            diff_viewport:  None,
            doc_generation: 0,
            cursor_x:       None,
            #[cfg(target_os = "macos")]
            menu_installed: false,
        }
    }
}

// ─── entry point ─────────────────────────────────────────────────────────────

fn app_icon() -> Option<window::Icon> {
    let bytes = include_bytes!("icon.png");
    let img   = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let img   = img.into_rgba8();
    let (w, h) = (img.width(), img.height());
    window::icon::from_rgba(img.into_raw(), w, h).ok()
}

fn main() -> iced::Result {
    // Arrange for application:openFile: to be injected into the app delegate at
    // will-finish-launching time — after winit sets its delegate, before macOS
    // dispatches the initial open-document Apple Event from Finder.
    #[cfg(target_os = "macos")]
    macos_menu::register_open_file_handler();

    iced::application(App::boot, App::update, App::view)
        .title("Quick JSON Viewer")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size: Size::new(1200.0, 800.0),
            min_size: Some(Size::new(700.0, 400.0)),
            icon: app_icon(),
            exit_on_close_request: false,
            ..window::Settings::default()
        })
        .antialiasing(true)
        .run()
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = App { settings: Settings::load(), ..Default::default() };
        // Update check: fire once on launch (the tick loop collects the result).
        app.update_rx = Some(update::spawn_check());
        // `quick-json-viewer file.json` opens the file straight away.
        if let Some(arg) = std::env::args_os().nth(1) {
            let path = PathBuf::from(arg);
            if path.is_file() {
                app.open_file(path);
            }
        }
        (app, iced::system::theme().map(Message::SystemTheme))
    }

    fn is_dark(&self) -> bool {
        self.settings.is_dark(self.system_dark)
    }

    fn theme(&self) -> Theme {
        theme::iced_theme(self.is_dark())
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            event::listen_with(|event, status, window| match &event {
                // Only the events `handle_event` acts on — forwarding every
                // pointer move would rebuild the whole view per pixel.
                iced::Event::Keyboard(keyboard::Event::KeyPressed { .. })
                | iced::Event::Window(
                    window::Event::Opened { .. }
                    | window::Event::Resized(_)
                    | window::Event::CloseRequested
                    | window::Event::FileDropped(_),
                )
                | iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::Event(event, status, window))
                }
                _ => None,
            }),
            iced::system::theme_changes().map(Message::SystemTheme),
        ];
        #[cfg(target_os = "macos")]
        subs.push(Subscription::run(macos_menu::wake_stream).map(|_| Message::MenuWake));

        let fast_busy = self.load_rx.is_some()
            || self.bg_write_rx.is_some()
            || self.ai.busy()
            || self.compare.left.load_rx.is_some()
            || self.compare.right.load_rx.is_some()
            || self.compare.diff_rx.is_some()
            || self.update_rx.is_some()
            || self.search_pending.is_some()
            || self.search_debounce_until.is_some();
        if fast_busy {
            subs.push(iced::time::every(Duration::from_millis(30)).map(|_| Message::Tick));
        } else if self.install_watcher_rx.is_some() {
            subs.push(iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick));
        }
        if std::env::var_os("JSONVIEWER_SCREENSHOT").is_some() {
            subs.push(iced::time::every(Duration::from_secs(3)).map(|_| Message::DebugShot));
        }
        Subscription::batch(subs)
    }

    fn view(&self) -> Element<'_, Message> {
        view::root(self)
    }
}

// ─── update ──────────────────────────────────────────────────────────────────

impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        Task::batch([task, self.scroll_tasks()])
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::Tick => self.poll_background(),
            Message::MenuWake => self.handle_menu_actions(),
            Message::Event(ev, status, id) => {
                self.window_id = Some(id);
                self.handle_event(ev, status)
            }
            Message::SystemTheme(mode) => {
                self.system_dark = !matches!(mode, iced::theme::Mode::Light);
                Task::none()
            }

            // ── toolbar / global ──
            Message::SetMode(mode) => { self.set_mode(mode); Task::none() }
            Message::OpenFile => { self.open_active_dialog(); Task::none() }
            Message::OpenUrlDialog => self.open_url_dialog(),
            Message::RequestPaste => iced::clipboard::read().map(Message::Pasted),
            Message::Pasted(text) => { self.handle_pasted(text); Task::none() }
            Message::ToggleMultiSelect => {
                if let Some(t) = &mut self.tree {
                    let on = !t.multi_select;
                    t.set_multi_select(on);
                }
                Task::none()
            }
            Message::ToggleAi => {
                let configured = self.settings.ai_enabled
                    && self.ai_settings_ui.key_present(self.settings.ai_provider);
                if configured {
                    self.ai.open = !self.ai.open;
                    if self.ai.open {
                        return iced::widget::operation::focus(ai::panel::AI_INPUT_ID);
                    }
                } else {
                    self.ai_settings_ui.focus = true;
                    self.dialog = Some(Dialog::Settings);
                }
                Task::none()
            }
            Message::SearchInput(s) => {
                self.search_input = s;
                self.search_debounce_until = Some(Instant::now() + Duration::from_millis(150));
                Task::none()
            }
            Message::SearchSubmit => {
                if self.search_debounce_until.take().is_some() {
                    self.kick_search();
                } else if let Some(t) = &mut self.tree {
                    t.search_next();
                }
                Task::none()
            }
            Message::SearchClear => {
                self.search_input.clear();
                self.kick_search();
                iced::widget::operation::focus(SEARCH_ID)
            }
            Message::ToggleRegex => {
                if let Some(t) = &mut self.tree {
                    t.search_use_regex = !t.search_use_regex;
                }
                self.kick_search();
                Task::none()
            }
            Message::SearchPrev => { if let Some(t) = &mut self.tree { t.search_prev(); } Task::none() }
            Message::SearchNext => { if let Some(t) = &mut self.tree { t.search_next(); } Task::none() }
            Message::FocusSearch => iced::widget::operation::focus(SEARCH_ID),
            Message::CollapseAll => { self.collapse_all_active(); Task::none() }
            Message::ExpandAll => { self.expand_all_active(); Task::none() }
            Message::Undo => { self.undo(); Task::none() }
            Message::Redo => { self.redo(); Task::none() }
            Message::CopySelected => self.copy_selected(),
            Message::OpenDialog(d) => {
                if d == Dialog::Url {
                    return self.open_url_dialog();
                }
                self.dialog = Some(d);
                Task::none()
            }
            Message::CloseDialog => { self.close_dialog(); Task::none() }
            Message::ToggleSettings => {
                if self.dialog == Some(Dialog::Settings) {
                    self.close_dialog();
                } else {
                    self.dialog = Some(Dialog::Settings);
                }
                Task::none()
            }

            // ── compare ──
            Message::DiffFilter(kind) => {
                let mut filter = self.compare.filter;
                match kind {
                    DiffKind::Changed => filter.changed = !filter.changed,
                    DiffKind::Added   => filter.added   = !filter.added,
                    DiffKind::Removed => filter.removed = !filter.removed,
                }
                self.set_diff_filter(filter);
                Task::none()
            }
            Message::ComparePrev => { self.compare_prev_diff(); Task::none() }
            Message::CompareNext => { self.compare_next_diff(); Task::none() }
            Message::ToggleOnlyDiffs => {
                let only = !self.compare.show_only_diffs;
                self.set_only_diffs(only);
                Task::none()
            }
            Message::DiffOption(opt) => {
                let o = &mut self.compare.options;
                match opt {
                    DiffOpt::IgnoreCase   => o.ignore_case = !o.ignore_case,
                    DiffOpt::ArrayOrder   => o.ignore_array_order = !o.ignore_array_order,
                    DiffOpt::NullMissing  => o.null_equals_missing = !o.null_equals_missing,
                    DiffOpt::TypeCoercion => o.type_coercion = !o.type_coercion,
                    DiffOpt::Trim         => o.trim_whitespace = !o.trim_whitespace,
                }
                self.compare.needs_rediff = true;
                self.recompute_diff_if_needed();
                Task::none()
            }
            Message::IgnoreKeys(s) => {
                self.compare.ignore_keys_raw = s;
                self.recompute_options_from_raw();
                self.compare.needs_rediff = true;
                self.recompute_diff_if_needed();
                Task::none()
            }
            Message::IgnorePattern(s) => {
                self.compare.ignore_pattern_raw = s;
                self.recompute_options_from_raw();
                self.compare.needs_rediff = true;
                self.recompute_diff_if_needed();
                Task::none()
            }
            Message::PaneActivate(side) => { self.compare.active_pane = side; Task::none() }
            Message::PaneClear(side) => { self.clear_pane(side); Task::none() }
            Message::PaneOpen(side) => {
                self.compare.active_pane = side;
                self.open_into_pane_dialog(side);
                Task::none()
            }
            Message::PanePaste(side) => {
                self.compare.active_pane = side;
                iced::clipboard::read().map(Message::Pasted)
            }
            Message::PaneErrorCtx(side) => {
                self.dialog = Some(Dialog::ErrorContext(Some(side)));
                Task::none()
            }
            Message::CursorMoved(p) => { self.cursor_x = Some(p.x); Task::none() }

            // ── trees ──
            Message::Row(ev) => self.handle_row_event(ev),
            Message::DiffRow(ev) => { self.handle_diff_row_event(ev); Task::none() }
            Message::TreeScrolled(vp) => {
                self.tree_viewport = Some((vp.absolute_offset().y, vp.bounds().height));
                Task::none()
            }
            Message::DiffScrolled(vp) => {
                self.diff_viewport = Some((vp.absolute_offset().y, vp.bounds().height));
                Task::none()
            }
            Message::MenuBar(id) => {
                if matches!(self.overlay, Some(Overlay { kind: OverlayKind::MenuBar(cur), .. }) if cur == id) {
                    self.overlay = None;
                } else {
                    let idx = MenuBarId::ALL.iter().position(|m| *m == id).unwrap_or(0) as f32;
                    self.overlay = Some(Overlay {
                        kind: OverlayKind::MenuBar(id),
                        position: Point::new(6.0 + idx * view::MENU_BUTTON_W, view::MENU_BAR_H),
                        submenu: None,
                    });
                }
                Task::none()
            }
            Message::OpenSubmenu(sub) => {
                if let Some(o) = &mut self.overlay {
                    o.submenu = if o.submenu == Some(sub) { None } else { Some(sub) };
                }
                Task::none()
            }
            Message::CloseOverlay => { self.overlay = None; Task::none() }
            Message::MenuPick(inner) => {
                self.overlay = None;
                self.handle(*inner)
            }
            Message::BreadcrumbJump(node_idx) => {
                if let Some(t) = &mut self.tree {
                    t.selected = Some(node_idx);
                    t.ensure_visible(node_idx);
                }
                Task::none()
            }

            // ── row / document actions ──
            Message::StartEdit(n, field) => self.start_edit_task(n, field),
            Message::StartAdd(parent) => self.start_add_item_task(parent),
            Message::ToggleDelete(n) => { self.toggle_delete(n); Task::none() }
            Message::ExpandRecursive(n) => { if let Some(t) = &mut self.tree { t.expand_recursive(n); } Task::none() }
            Message::CollapseRecursive(n) => { if let Some(t) = &mut self.tree { t.collapse_recursive(n); } Task::none() }
            Message::FindSimilar(n, by) => { self.kick_find_similar(n, by); Task::none() }
            Message::CopyPath(n) => copy(self.path_of(n)),
            Message::CopyKey(n) => copy(self.key_of(n)),
            Message::CopyValue(n) => copy(self.value_text_of(n)),
            Message::CopyModifiedValue(n) => copy(self.modified_value_text_of(n)),
            Message::CopyAsCode(n, lang) => copy(self.code_of(n, lang)),
            Message::CopyDiffValue(side, n) => copy(self.diff_value_text_of(side, n)),
            Message::CopyDiffPath(n) => copy(self.diff_path_of(n)),
            Message::Export(scope, fmt) => { self.export(scope, fmt); Task::none() }
            Message::Save(SaveAction::Overwrite) => { self.save_overwrite(); Task::none() }
            Message::Save(SaveAction::Copy) => { self.save_copy(); Task::none() }
            Message::DiscardChanges => { self.discard_changes(); Task::none() }
            Message::ClearDocument => { self.clear_document(); Task::none() }

            // ── dialogs ──
            Message::Settings(msg) => self.handle_settings_msg(msg),
            Message::UrlEdit(action) => { self.url_editor.perform(action); Task::none() }
            Message::UrlOpen => {
                let text = self.url_editor.text();
                if let Some(req) = url_parse::parse_request(&text) {
                    self.dialog = None;
                    self.url_editor = text_editor::Content::new();
                    self.open_url_request(req);
                }
                Task::none()
            }
            Message::EditText(s) => {
                if let Some(e) = &mut self.editing_node { e.text = s; }
                Task::none()
            }
            Message::EditCommit => { self.commit_edit(); Task::none() }
            Message::EditCancel => { self.editing_node = None; Task::none() }
            Message::AddKey(s) => {
                if let Some(a) = &mut self.adding_item { a.key = s; }
                Task::none()
            }
            Message::AddValue(action) => {
                if let Some(a) = &mut self.adding_item { a.value.perform(action); }
                Task::none()
            }
            Message::AddFormat => {
                if let Some(a) = &mut self.adding_item { a.format(); }
                Task::none()
            }
            Message::AddCommit => {
                if self.add_item_valid() {
                    self.commit_add_item();
                }
                Task::none()
            }
            Message::AddCancel => { self.adding_item = None; Task::none() }

            // ── update banner ──
            Message::DismissUpdate => {
                if let Some(info) = &self.update_available {
                    self.settings.dismissed_update = Some(info.version.clone());
                    self.settings.save();
                }
                Task::none()
            }
            Message::UpgradeNow => {
                if self.install_watcher_rx.is_none() {
                    self.install_watcher_rx = Some(update::launch_brew_upgrade());
                }
                Task::none()
            }
            Message::CopyBrewCommand => copy(Some(update::BREW_UPGRADE_CMD.to_owned())),
            Message::OpenUrl(url) => { open_in_browser(&url); Task::none() }

            // ── AI ──
            Message::Ai(msg) => {
                let index = self.tree.as_ref().map(|t| &t.index);
                let file_name = self.file_info.as_ref().map(|f| f.name.as_str()).unwrap_or("document.json");
                let apply = ai::panel::update(&mut self.ai, msg, &self.settings, index, file_name);
                if let Some(edits) = apply {
                    self.apply_ai_edits(edits);
                }
                Task::none()
            }
            Message::AiResizeStart => { self.ai_resizing = true; Task::none() }
            Message::AiResizeDrag(p) => {
                if self.ai_resizing {
                    self.ai.width = (self.window_size.width - p.x).clamp(260.0, 780.0);
                }
                Task::none()
            }

            Message::DebugShot => {
                // Optional scripted action before the shot, e.g. "compare",
                // "menu:3", "settings", "help", "ai".
                let spec = std::env::var("JSONVIEWER_SCREENSHOT").unwrap_or_default();
                let action = spec.split(',').nth(1).unwrap_or("").to_owned();
                let pre = match action.as_str() {
                    "compare"  => self.handle(Message::SetMode(AppMode::Compare)),
                    "compare2" => {
                        let t1 = self.handle(Message::SetMode(AppMode::Compare));
                        if let Some(p) = std::env::var_os("JSONVIEWER_SECOND") {
                            self.open_file_into_pane(Side::Right, PathBuf::from(p));
                        }
                        t1
                    }
                    "expand" => {
                        if let Some(t) = &mut self.tree { t.expand_all(); t.selected = Some(12); }
                        Task::none()
                    }
                    "ai" => {
                        self.settings.ai_enabled = true;
                        self.ai.open = true;
                        Task::none()
                    }
                    "light" => {
                        self.settings.theme = settings::Theme::Light;
                        if let Some(t) = &mut self.tree { t.expand_all(); t.selected = Some(12); }
                        Task::none()
                    }
                    "settings" => self.handle(Message::OpenDialog(Dialog::Settings)),
                    "help"     => self.handle(Message::OpenDialog(Dialog::Help)),
                    "search"   => self.handle(Message::OpenDialog(Dialog::SearchHelp)),
                    "about"    => self.handle(Message::OpenDialog(Dialog::About)),
                    "url"      => self.handle(Message::OpenUrlDialog),
                    "menubar"  => { self.settings.show_menu_bar = true; self.handle(Message::MenuBar(MenuBarId::File)) }
                    a if a.starts_with("menu:") => {
                        let n: u32 = a[5..].parse().unwrap_or(1);
                        self.handle(Message::Row(RowEvent::RightClick { node: n, position: Point::new(300.0, 200.0) }))
                    }
                    a if a.starts_with("edit:") => {
                        let n: u32 = a[5..].parse().unwrap_or(1);
                        self.handle(Message::StartEdit(n, EditField::Value))
                    }
                    a if a.starts_with("select:") => {
                        let n: u32 = a[7..].parse().unwrap_or(1);
                        if let Some(t) = &mut self.tree { t.selected = Some(n); t.ensure_visible(n); }
                        Task::none()
                    }
                    a if a.starts_with("find:") => self.handle(Message::SearchInput(a[5..].to_owned())),
                    _ => Task::none(),
                };
                let shot = match self.window_id {
                    Some(id) => window::screenshot(id).map(Message::DebugShotTaken),
                    None => window::latest().then(|id| match id {
                        Some(id) => window::screenshot(id).map(Message::DebugShotTaken),
                        None => Task::none(),
                    }),
                };
                // Give the pre-action a frame to render before capturing.
                pre.chain(Task::future(async {
                    tokio_sleep(Duration::from_millis(700)).await;
                }).discard()).chain(shot)
            }
            Message::DebugShotTaken(shot) => {
                let spec = std::env::var("JSONVIEWER_SCREENSHOT").unwrap_or_default();
                let path = spec.split(',').next().unwrap_or("shot.png").to_owned();
                if let Some(img) = image::RgbaImage::from_raw(shot.size.width, shot.size.height, shot.rgba.to_vec()) {
                    let _ = img.save(&path);
                }
                iced::exit()
            }
        }
    }

    fn handle_settings_msg(&mut self, msg: SettingsMsg) -> Task<Message> {
        match msg {
            SettingsMsg::Close => { self.close_dialog(); return Task::none(); }
            SettingsMsg::Ai(ai_msg) => {
                ai::panel::apply_settings_msg(&mut self.settings, &mut self.ai_settings_ui, ai_msg);
            }
            other => {
                if settings::apply(&mut self.settings, &mut self.default_status, &other) {
                    // A manual check is an explicit "show me" — override any
                    // prior dismissal so the banner reappears even for the
                    // same release.
                    self.settings.dismissed_update = None;
                    self.update_available = None;
                    self.settings.save();
                    self.update_rx = Some(update::spawn_check());
                    return Task::none();
                }
            }
        }
        self.settings.save();
        Task::none()
    }

    fn close_dialog(&mut self) {
        if self.dialog == Some(Dialog::Settings) {
            // The AI-section highlight only lasts as long as this visit.
            self.ai_settings_ui.focus = false;
            self.settings.save();
        }
        self.dialog = None;
    }

    /// Whether a modal (dialog, edit, add) currently owns the keyboard.
    fn modal_open(&self) -> bool {
        self.dialog.is_some() || self.editing_node.is_some() || self.adding_item.is_some()
    }

    // ── runtime events ──

    fn handle_event(&mut self, ev: iced::Event, status: event::Status) -> Task<Message> {
        match ev {
            iced::Event::Window(window::Event::Opened { size, .. }) => {
                self.window_size = size;
                #[cfg(target_os = "macos")]
                if !self.menu_installed {
                    macos_menu::install();
                    self.menu_installed = true;
                }
                Task::none()
            }
            iced::Event::Window(window::Event::Resized(size)) => {
                self.window_size = size;
                Task::none()
            }
            iced::Event::Window(window::Event::CloseRequested) => {
                self.settings.save();
                match self.window_id {
                    Some(id) => window::close(id),
                    None => window::latest().then(|id| match id {
                        Some(id) => window::close(id),
                        None => iced::exit(),
                    }),
                }
            }
            iced::Event::Window(window::Event::FileDropped(path)) => {
                match self.mode {
                    AppMode::Viewer  => self.open_file(path),
                    AppMode::Compare => {
                        let side = self.drop_side();
                        self.open_file_into_pane(side, path);
                    }
                }
                Task::none()
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                self.ai_resizing = false;
                Task::none()
            }
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) => {
                self.handle_key(key, modifiers, text.map(|t| t.to_string()), status)
            }
            _ => Task::none(),
        }
    }

    fn handle_key(
        &mut self,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
        text: Option<String>,
        status: event::Status,
    ) -> Task<Message> {
        use keyboard::key::Named;
        use keyboard::Key;

        let cmd   = modifiers.command();
        let shift = modifiers.shift();
        let alt   = modifiers.alt();
        let none  = !cmd && !shift && !alt && !modifiers.control();
        let ignored = status == event::Status::Ignored;
        let modal = self.modal_open();

        // Letter shortcuts arrive as the (possibly shifted) character.
        let ch: Option<char> = match key.as_ref() {
            Key::Character(c) => c.chars().next().map(|c| c.to_ascii_lowercase()),
            _ => None,
        };

        // Escape closes popups and dialogs, innermost first.
        if matches!(key.as_ref(), Key::Named(Named::Escape)) {
            if self.overlay.is_some() {
                self.overlay = None;
            } else if self.editing_node.is_some() {
                self.editing_node = None;
            } else if self.adding_item.is_some() {
                self.adding_item = None;
            } else if self.dialog.is_some() {
                self.close_dialog();
            }
            return Task::none();
        }

        // ⌘↵ submits the URL dialog (the editor keeps plain Enter as newline).
        if self.dialog == Some(Dialog::Url) && cmd && matches!(key.as_ref(), Key::Named(Named::Enter)) {
            return self.handle(Message::UrlOpen);
        }

        // Global shortcuts (work even while a text field is focused).
        if cmd && !modal {
            match ch {
                Some('o') if !shift => return self.handle(Message::OpenFile),
                Some('l') if !shift && !alt => return self.handle(Message::OpenUrlDialog),
                Some('f') if !shift => return self.handle(Message::FocusSearch),
                Some(',') => return self.handle(Message::ToggleSettings),
                Some('g') if !shift => {
                    return match self.mode {
                        AppMode::Viewer => self.handle(Message::SearchNext),
                        AppMode::Compare => self.handle(Message::CompareNext),
                    };
                }
                Some('g') if shift => {
                    return match self.mode {
                        AppMode::Viewer => self.handle(Message::SearchPrev),
                        AppMode::Compare => self.handle(Message::ComparePrev),
                    };
                }
                _ => {}
            }
        }
        if alt && !cmd && !modal {
            match ch {
                Some('c') => return self.handle(Message::CollapseAll),
                Some('x') => return self.handle(Message::ExpandAll),
                _ => {}
            }
        }

        // Everything below competes with text fields: only when no widget
        // consumed the key and no modal is open.
        if !ignored || modal {
            return Task::none();
        }

        if cmd {
            match ch {
                // ⌘V — view clipboard text as a document.
                Some('v') if !shift && !alt => return self.handle(Message::RequestPaste),
                // ⌘C — copy the selected node's value.
                Some('c') if !shift && !alt => return self.handle(Message::CopySelected),
                Some('z') if !shift && self.mode == AppMode::Viewer => return self.handle(Message::Undo),
                Some('z') if shift && self.mode == AppMode::Viewer => return self.handle(Message::Redo),
                Some('s') if !shift && self.mode == AppMode::Viewer => {
                    if self.is_dirty() {
                        return if self.can_overwrite() {
                            self.handle(Message::Save(SaveAction::Overwrite))
                        } else {
                            self.handle(Message::Save(SaveAction::Copy))
                        };
                    }
                    return Task::none();
                }
                Some('s') if shift && self.mode == AppMode::Viewer => {
                    if self.is_dirty() {
                        return self.handle(Message::Save(SaveAction::Copy));
                    }
                    return Task::none();
                }
                _ => return Task::none(),
            }
        }

        if none {
            if let Key::Named(named) = key.as_ref() {
                match self.mode {
                    AppMode::Viewer => {
                        if let Some(t) = &mut self.tree {
                            match named {
                                Named::ArrowUp    => t.select_up(),
                                Named::ArrowDown  => t.select_down(),
                                Named::ArrowLeft  => t.select_left(),
                                Named::ArrowRight => t.select_right(),
                                Named::PageUp     => t.select_page_up(20),
                                Named::PageDown   => t.select_page_down(20),
                                Named::Home       => t.select_home(),
                                Named::End        => t.select_end(),
                                _ => {}
                            }
                        }
                    }
                    AppMode::Compare => {
                        if let (Some(result), Some(t)) = (&self.compare.result, &mut self.compare.tree) {
                            match named {
                                Named::ArrowUp    => t.select_up(),
                                Named::ArrowDown  => t.select_down(),
                                Named::ArrowLeft  => t.select_left(result),
                                Named::ArrowRight => t.select_right(result),
                                Named::PageUp     => t.select_page_up(20),
                                Named::PageDown   => t.select_page_down(20),
                                Named::Home       => t.select_home(),
                                Named::End        => t.select_end(),
                                _ => {}
                            }
                        }
                    }
                }

                if self.mode == AppMode::Viewer {
                    match named {
                        // F2 → edit selected leaf value.
                        Named::F2 => {
                            if let Some(t) = &self.tree {
                                if let Some(sel) = t.selected {
                                    let editable = !matches!(
                                        t.kind_of(sel),
                                        index::NodeKind::Object | index::NodeKind::Array
                                    );
                                    if editable {
                                        return self.start_edit_task(sel, EditField::Value);
                                    }
                                }
                            }
                        }
                        // Delete → toggle delete on the selected node (skip the root).
                        Named::Delete => {
                            let to_delete = self.tree.as_ref().and_then(|t| {
                                t.selected.filter(|&sel| t.is_added(sel) || t.index.nodes[sel as usize].parent != u32::MAX)
                            });
                            if let Some(n) = to_delete {
                                self.toggle_delete(n);
                            }
                        }
                        _ => {}
                    }
                }
                return Task::none();
            }
        }

        // Type-ahead selection: plain printable characters.
        if !cmd && !alt && !modifiers.control() && self.mode == AppMode::Viewer && self.tree.is_some() {
            if let Some(t) = text {
                let typed: String = t.chars().filter(|c| !c.is_control()).collect();
                if !typed.is_empty() {
                    let now = Instant::now();
                    if self.type_ahead_time.map_or(true, |t0| now.duration_since(t0) > Duration::from_secs(1)) {
                        self.type_ahead.clear();
                    }
                    self.type_ahead.push_str(&typed);
                    self.type_ahead_time = Some(now);
                    let prefix = self.type_ahead.clone();
                    if let Some(tree) = &mut self.tree {
                        tree.type_ahead_select(&prefix);
                    }
                }
            }
        }
        Task::none()
    }

    /// Drain the macOS menu-bar / Finder-open queue.
    fn handle_menu_actions(&mut self) -> Task<Message> {
        #[cfg(target_os = "macos")]
        {
            let acts = macos_menu::take_actions();
            let mut tasks = Vec::new();
            if acts & macos_menu::ACT_OPEN_FILE     != 0 { tasks.push(self.handle(Message::OpenFile)); }
            if acts & macos_menu::ACT_OPEN_URL      != 0 { tasks.push(self.handle(Message::OpenUrlDialog)); }
            if acts & macos_menu::ACT_PASTE         != 0 { tasks.push(self.handle(Message::RequestPaste)); }
            if acts & macos_menu::ACT_SETTINGS      != 0 { self.dialog = Some(Dialog::Settings); }
            if acts & macos_menu::ACT_FOCUS_SEARCH  != 0 { tasks.push(self.handle(Message::FocusSearch)); }
            if acts & macos_menu::ACT_COLLAPSE_ALL  != 0 { self.collapse_all_active(); }
            if acts & macos_menu::ACT_EXPAND_ALL    != 0 { self.expand_all_active(); }
            if acts & macos_menu::ACT_HELP          != 0 { self.dialog = Some(Dialog::Help); }
            if acts & macos_menu::ACT_SEARCH_SYNTAX != 0 { self.dialog = Some(Dialog::SearchHelp); }
            if acts & macos_menu::ACT_ABOUT         != 0 { self.dialog = Some(Dialog::About); }
            if acts & macos_menu::ACT_EXPORT_JSON   != 0 { self.export(ExportScope::File, ExportFormat::Json); }
            if acts & macos_menu::ACT_EXPORT_CSV    != 0 { self.export(ExportScope::File, ExportFormat::Csv); }
            if acts & macos_menu::ACT_SAVE          != 0 { self.save_overwrite(); }
            if acts & macos_menu::ACT_SAVE_COPY     != 0 { self.save_copy(); }
            if acts & macos_menu::ACT_UNDO          != 0 { self.undo(); }
            if acts & macos_menu::ACT_REDO          != 0 { self.redo(); }
            if let Some(path) = macos_menu::take_open_file() { self.open_file(path); }
            return Task::batch(tasks);
        }
        #[allow(unreachable_code)]
        Task::none()
    }

    /// Poll every background channel. Runs on the tick subscription, which is
    /// only active while something is in flight.
    fn poll_background(&mut self) -> Task<Message> {
        // Loader — drain everything queued so progress messages don't lag.
        while let Some(rx) = &self.load_rx {
            match rx.try_recv() {
                Ok(LoadMsg::Progress(p)) => self.load_progress = p,
                Ok(LoadMsg::Done(idx)) => {
                    self.tree = Some(TreeState::new(idx));
                    self.load_rx = None;
                }
                Ok(LoadMsg::Error(e, ctx)) => {
                    self.load_error = Some(e);
                    self.load_error_ctx = ctx;
                    self.load_rx = None;
                }
                Err(_) => break,
            }
        }

        // Background export/save writer.
        if let Some(rx) = &self.bg_write_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.bg_write_rx = None;
                    self.finish_bg_write(res);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.bg_write_rx = None,
            }
        }

        self.ai.poll();

        self.poll_pane_loader(Side::Left);
        self.poll_pane_loader(Side::Right);
        self.recompute_diff_if_needed();
        self.poll_diff();
        self.poll_update();

        // Debounced search kick + poll background search.
        if let Some(deadline) = self.search_debounce_until {
            if Instant::now() >= deadline {
                self.search_debounce_until = None;
                self.kick_search();
            }
        }
        let search_done = self.search_pending.as_ref().map(|h| h.is_finished()).unwrap_or(false);
        if search_done {
            // None = the scan was cancelled mid-flight; discard.
            let results = self.search_pending.take().unwrap().join().ok().flatten();
            if let (Some(t), Some(results)) = (&mut self.tree, results) {
                t.set_search_results(results);
            }
        }
        Task::none()
    }

    // ── tree interaction ──

    fn handle_row_event(&mut self, ev: RowEvent) -> Task<Message> {
        let Some(tree) = &mut self.tree else { return Task::none() };
        match ev {
            RowEvent::Click { node, region } => {
                match region {
                    Region::Checkbox => tree.toggle_check(node),
                    Region::Caret => {
                        tree.selected = Some(node);
                        tree.toggle(node);
                    }
                    _ => tree.selected = Some(node),
                }
                Task::none()
            }
            RowEvent::DoubleClick { node, region } => {
                match region {
                    Region::Checkbox => tree.toggle_check(node),
                    // A double-click on the caret is just two toggles; the
                    // first press already toggled, so treat this as a plain
                    // click to end up toggled once.
                    Region::Caret => {
                        tree.selected = Some(node);
                        tree.toggle(node);
                    }
                    Region::Key | Region::Other => {
                        tree.selected = Some(node);
                        let kind = tree.kind_of(node);
                        let is_container = matches!(kind, index::NodeKind::Object | index::NodeKind::Array);
                        let child_count = if tree.is_added(node) {
                            tree.added_child_count(node)
                        } else {
                            tree.index.nodes[node as usize].child_count
                        };
                        let can_toggle = is_container && child_count > 0 && node != tree.index.root;
                        if can_toggle {
                            tree.toggle(node);
                        } else if !is_container {
                            let field = if region == Region::Key { EditField::Key } else { EditField::Value };
                            return self.start_edit_task(node, field);
                        }
                    }
                }
                Task::none()
            }
            RowEvent::RightClick { node, position } => {
                self.overlay = Some(Overlay { kind: OverlayKind::Row(node), position, submenu: None });
                Task::none()
            }
        }
    }

    fn handle_diff_row_event(&mut self, ev: RowEvent) {
        let (Some(result), Some(tree)) = (&self.compare.result, &mut self.compare.tree) else { return };
        match ev {
            RowEvent::Click { node, region } => {
                tree.selected = Some(node);
                if region == Region::Caret {
                    tree.toggle(node, result);
                }
            }
            RowEvent::DoubleClick { node, region } => {
                tree.selected = Some(node);
                let dn = &result.nodes[node as usize];
                let can_toggle = dn.child_count > 0 && node != result.root;
                // Same caret rule as the viewer: a double-click on the caret
                // counts as one toggle in total.
                if can_toggle && (region == Region::Caret || region == Region::Other) {
                    tree.toggle(node, result);
                }
            }
            RowEvent::RightClick { node, position } => {
                self.overlay = Some(Overlay { kind: OverlayKind::DiffRow(node), position, submenu: None });
            }
        }
    }

    /// Turn pending `scroll_to_row` / `reveal_row` requests on the tree states
    /// into scrollable commands.
    fn scroll_tasks(&mut self) -> Task<Message> {
        let row_h = self.settings.row_height();
        let mut tasks = Vec::new();
        let default_h = (self.window_size.height - 150.0).max(100.0);

        if let Some(t) = &mut self.tree {
            let scroll_to = t.scroll_to_row.take();
            let reveal = t.reveal_row.take();
            let (off, vp_h) = self.tree_viewport.unwrap_or((0.0, default_h));
            if let Some(y) = scroll_target(scroll_to, reveal, row_h, off, vp_h, t.visible.len()) {
                tasks.push(iced::widget::operation::scroll_to(
                    TREE_SCROLL_ID,
                    scrollable::AbsoluteOffset { x: None, y: Some(y) },
                ));
            }
        }
        if let Some(t) = &mut self.compare.tree {
            let scroll_to = t.scroll_to_row.take();
            let reveal = t.reveal_row.take();
            let (off, vp_h) = self.diff_viewport.unwrap_or((0.0, default_h));
            if let Some(y) = scroll_target(scroll_to, reveal, row_h, off, vp_h, t.visible.len()) {
                tasks.push(iced::widget::operation::scroll_to(
                    DIFF_SCROLL_ID,
                    scrollable::AbsoluteOffset { x: None, y: Some(y) },
                ));
            }
        }
        Task::batch(tasks)
    }

    fn start_edit_task(&mut self, node_idx: u32, field: EditField) -> Task<Message> {
        self.start_edit(node_idx, field);
        if self.editing_node.is_some() {
            iced::widget::operation::focus(EDIT_ID)
        } else {
            Task::none()
        }
    }

    fn start_add_item_task(&mut self, parent: u32) -> Task<Message> {
        self.start_add_item(parent);
        let is_object = self.tree.as_ref().map_or(false, |t| t.kind_of(parent) == index::NodeKind::Object);
        iced::widget::operation::focus(if is_object { ADD_KEY_ID } else { ADD_VALUE_ID })
    }

    fn add_item_valid(&self) -> bool {
        let Some(state) = &self.adding_item else { return false };
        let Some(tree) = &self.tree else { return false };
        let is_object = tree.kind_of(state.parent) == index::NodeKind::Object;
        let value_valid = serde_json::from_str::<serde_json::Value>(&state.text()).is_ok();
        let key_valid = !is_object || !state.key.trim().is_empty();
        value_valid && key_valid
    }

    // ── clipboard helpers ──

    fn copy_selected(&self) -> Task<Message> {
        if self.mode != AppMode::Viewer { return Task::none(); }
        let Some(t) = &self.tree else { return Task::none() };
        let Some(sel) = t.selected else { return Task::none() };
        let text = if t.is_added(sel) {
            t.added_item(sel).raw_value.clone()
        } else {
            let n = &t.index.nodes[sel as usize];
            String::from_utf8_lossy(t.index.value_bytes(n)).into_owned()
        };
        copy(Some(text))
    }

    fn path_of(&self, node_idx: u32) -> Option<String> {
        let t = self.tree.as_ref()?;
        Some(if t.is_added(node_idx) {
            added_path(&t.index, &t.added_items, node_idx)
        } else {
            build_path(&t.index.nodes, &t.index, node_idx)
        })
    }

    fn key_of(&self, node_idx: u32) -> Option<String> {
        let t = self.tree.as_ref()?;
        if t.is_added(node_idx) {
            return t.added_item(node_idx).key.clone();
        }
        let n = &t.index.nodes[node_idx as usize];
        if n.key_len > 0 {
            Some(t.index.key_of(n).to_owned())
        } else if n.array_index != u32::MAX {
            Some(n.array_index.to_string())
        } else {
            None
        }
    }

    fn value_text_of(&self, node_idx: u32) -> Option<String> {
        let t = self.tree.as_ref()?;
        Some(if t.is_added(node_idx) {
            t.added_item(node_idx).raw_value.clone()
        } else if self.settings.copy_compact_json {
            export::json_compact(&t.index, node_idx)
        } else {
            let n = &t.index.nodes[node_idx as usize];
            String::from_utf8_lossy(t.index.value_bytes(n)).into_owned()
        })
    }

    fn modified_value_text_of(&self, node_idx: u32) -> Option<String> {
        let t = self.tree.as_ref()?;
        Some(if self.settings.copy_compact_json {
            export::json_compact_with_edits(&t.index, node_idx, &self.edit_overlay, &t.added_items)
        } else {
            export::json_with_edits(&t.index, node_idx, &self.edit_overlay, &t.added_items)
                .trim_end()
                .to_owned()
        })
    }

    fn code_of(&self, node_idx: u32, lang: codegen::CodeLanguage) -> Option<String> {
        let t = self.tree.as_ref()?;
        let index = &t.index;
        let is_new = t.is_added(node_idx);
        let added_item = is_new.then(|| t.added_item(node_idx));
        let node = if is_new {
            tree_view::synthetic_node(index, &t.added_items, node_idx)
        } else {
            index.nodes[node_idx as usize]
        };
        let root_name = match added_item.and_then(|it| it.key.as_deref()) {
            Some(k) => codegen::to_pascal_case(k),
            None if node.key_len > 0 && !is_new => codegen::to_pascal_case(index.key_of(&node)),
            None => "RootObject".to_owned(),
        };
        // A pending container has no source bytes — serialize the rows it
        // holds (with any edits) and generate from that.
        let pending_json = is_new.then(|| {
            export::json_compact_with_edits(index, node_idx, &self.edit_overlay, &t.added_items)
        });
        let raw: &[u8] = match &pending_json {
            Some(j) => j.as_bytes(),
            None    => index.value_bytes(&node),
        };
        Some(codegen::generate(raw, lang, &root_name))
    }

    fn diff_value_text_of(&self, side: Side, node_idx: u32) -> Option<String> {
        let result = self.compare.result.as_ref()?;
        let dn = &result.nodes[node_idx as usize];
        let (index, i) = match side {
            Side::Left  => (&*result.left, dn.left_idx()?),
            Side::Right => (&*result.right, dn.right_idx()?),
        };
        Some(if self.settings.copy_compact_json {
            export::json_compact(index, i)
        } else {
            String::from_utf8_lossy(index.value_bytes(&index.nodes[i as usize])).into_owned()
        })
    }

    fn diff_path_of(&self, node_idx: u32) -> Option<String> {
        let result = self.compare.result.as_ref()?;
        let dn = &result.nodes[node_idx as usize];
        let (idx, n) = match (dn.left_idx(), dn.right_idx()) {
            (Some(li), _) => (&*result.left, li),
            (_, Some(ri)) => (&*result.right, ri),
            _             => (&*result.left, 0),
        };
        Some(build_path(&idx.nodes, idx, n))
    }

    fn handle_pasted(&mut self, text: Option<String>) {
        let Some(text) = text else { return };
        // A file copied in Finder pastes only its display name as text; the
        // actual path lives in the pasteboard's file-url type. Check that
        // first and open the file like a drop.
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

/// Async sleep without pulling tokio in directly: spin on a std thread.
async fn tokio_sleep(d: Duration) {
    let (tx, rx) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(d);
        let _ = tx.send(());
    });
    let _ = rx.await;
}

/// Clipboard write as a task (no-op for `None`).
fn copy(text: Option<String>) -> Task<Message> {
    match text {
        Some(t) => iced::clipboard::write(t),
        None => Task::none(),
    }
}

/// Vertical scroll offset that centres `scroll_to` or minimally reveals
/// `reveal`, given the current offset and viewport height.
fn scroll_target(
    scroll_to: Option<usize>,
    reveal: Option<usize>,
    row_h: f32,
    offset: f32,
    vp_h: f32,
    rows: usize,
) -> Option<f32> {
    let max_off = (rows as f32 * row_h - vp_h).max(0.0);
    if let Some(row) = scroll_to {
        let y = (row as f32 * row_h - vp_h / 2.0 + row_h / 2.0).clamp(0.0, max_off);
        return Some(y);
    }
    if let Some(row) = reveal {
        let top = row as f32 * row_h;
        let bottom = top + row_h;
        if top < offset {
            return Some(top.clamp(0.0, max_off));
        }
        if bottom > offset + vp_h {
            return Some((bottom - vp_h).clamp(0.0, max_off));
        }
    }
    None
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

    /// Reset per-document viewer state before a new load starts.
    fn reset_document_state(&mut self) {
        self.tree         = None;
        self.load_error   = None;
        self.load_error_ctx = None;
        self.load_progress = 0.0;
        self.search_input.clear();
        self.search_pending = None;
        self.search_debounce_until = None;
        self.edit_overlay.clear();
        self.saved_overlay.clear();
        self.editing_node = None;
        self.adding_item  = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.tree_viewport = None;
        self.doc_generation += 1;
    }

    fn open_file(&mut self, path: PathBuf) {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.file_info = Some(FileInfo { name, size_bytes: size, path: Some(path.clone()) });
        self.reset_document_state();
        self.load_rx = Some(loader::spawn_load(path));
    }

    /// Unload the current document and reset the viewer to its empty state.
    fn clear_document(&mut self) {
        self.file_info = None;
        self.load_rx   = None;
        self.reset_document_state();
        if self.dialog == Some(Dialog::ErrorContext(None)) {
            self.dialog = None;
        }
    }

    /// Unload one Compare pane and drop the diff computed against it.
    fn clear_pane(&mut self, side: Side) {
        *self.compare.pane_mut(side) = ComparePane::default();
        self.compare.result       = None;
        self.compare.tree         = None;
        self.compare.diff_rx      = None;
        self.compare.needs_rediff = false;
        if self.dialog == Some(Dialog::ErrorContext(Some(side))) {
            self.dialog = None;
        }
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
        self.file_info = Some(FileInfo { name: name.to_owned(), size_bytes: data.len() as u64, path: None });
        self.reset_document_state();
        self.load_rx = Some(loader::spawn_parse(data));
    }

    fn open_url_dialog(&mut self) -> Task<Message> {
        self.dialog = Some(Dialog::Url);
        iced::widget::operation::focus(URL_ID)
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
        self.file_info = Some(FileInfo { name, size_bytes: 0, path: None });
        self.reset_document_state();
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

    /// Right-click → Find Similar Items: scan for every node sharing the
    /// reference node's key and/or value, and land the hits in the search
    /// results so highlighting, ▲/▼ and ⌘G work exactly as for a typed query.
    fn kick_find_similar(&mut self, node_idx: u32, by: search::SimilarBy) {
        // Abort any in-flight scan — its results would land on top of ours.
        self.search_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        self.search_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.search_pending = None;
        self.search_debounce_until = None;
        let Some(t) = &self.tree else { return };
        // Show the closest typed equivalent in the box so the user can see
        // what's highlighted and tweak it. It's a display approximation: the
        // scan matches keys and values exactly, `key:` is a substring filter,
        // and long values are elided.
        self.search_input = t
            .index
            .nodes
            .get(node_idx as usize)
            .map(|n| {
                let key = t.index.key_of(n);
                let val = elide(&String::from_utf8_lossy(t.index.value_bytes(n)), 40);
                match by {
                    search::SimilarBy::Key         => format!("key:{key}"),
                    search::SimilarBy::Value       => format!("value = {val}"),
                    search::SimilarBy::KeyAndValue => format!("\"{key}\" = {val}"),
                }
            })
            .unwrap_or_default();
        let index  = Arc::clone(&t.index);
        let cancel = Arc::clone(&self.search_cancel);
        self.search_pending =
            Some(std::thread::spawn(move || search::find_similar(&index, node_idx, by, &cancel)));
    }
}

/// Shorten `s` to at most `max` chars, marking the cut with an ellipsis.
fn elide(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None           => s.to_owned(),
    }
}

// ─── editing ─────────────────────────────────────────────────────────────────

impl App {
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

    /// True when there are edits not yet persisted by an overwrite-save.
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

    /// Record an undoable "add item" action. Undo removes the item's `rows`
    /// again; this relies on strict LIFO ordering (see
    /// `TreeState::remove_last_added_items`).
    fn push_undo_add(&mut self, parent: u32, key: Option<String>, raw_value: String, rows: usize) {
        self.undo_stack.push(UndoAction::Add { parent, key, raw_value, rows });
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
            UndoAction::Add { rows, .. } => {
                if let Some(tree) = &mut self.tree {
                    tree.remove_last_added_items(*rows);
                }
            }
            UndoAction::Batch { entries, add_rows, .. } => {
                for entry in entries.iter().rev() {
                    match &entry.before {
                        Some(edit) => { self.edit_overlay.insert(entry.node_idx, edit.clone()); }
                        None       => { self.edit_overlay.remove(&entry.node_idx); }
                    }
                }
                if let Some(tree) = &mut self.tree {
                    tree.remove_last_added_items(*add_rows);
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
            UndoAction::Add { parent, key, raw_value, .. } => {
                if let Some(tree) = &mut self.tree {
                    tree.add_item(*parent, key.clone(), raw_value.clone());
                }
            }
            UndoAction::Batch { entries, adds, .. } => {
                for entry in entries {
                    match &entry.after {
                        Some(edit) => { self.edit_overlay.insert(entry.node_idx, edit.clone()); }
                        None       => { self.edit_overlay.remove(&entry.node_idx); }
                    }
                }
                if let Some(tree) = &mut self.tree {
                    for add in adds {
                        tree.add_item(add.parent, add.key.clone(), add.raw_value.clone());
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
            // A pending item's value is raw JSON text (typed into the Add
            // dialog, or sliced out of a pasted container), so it gets the
            // same quote-stripping as a real string node. Its key is plain
            // text in `AddedItem::key`, not in the byte arena.
            let text = match field {
                EditField::Value => {
                    let raw = self
                        .edit_overlay
                        .get(&node_idx)
                        .and_then(|e| e.value_override.clone())
                        .unwrap_or_else(|| tree.added_item(node_idx).raw_value.clone());
                    if export::added_kind(&raw) == NodeKind::String {
                        serde_json::from_str::<String>(&raw).unwrap_or(raw)
                    } else {
                        raw
                    }
                }
                EditField::Key => self
                    .edit_overlay
                    .get(&node_idx)
                    .and_then(|e| e.key_override.clone())
                    .or_else(|| tree.added_item(node_idx).key.clone())
                    .unwrap_or_default(),
            };
            self.editing_node = Some(EditingState { node_idx, field, text });
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

        self.editing_node = Some(EditingState { node_idx, field, text });
    }

    /// Store the committed edit into `edit_overlay` and clear `editing_node`.
    fn commit_edit(&mut self) {
        let Some(state) = self.editing_node.take() else { return };
        let Some(tree) = &self.tree else { return };
        use index::NodeKind;

        if tree.is_added(state.node_idx) {
            // Mirror the real-node path: a string is re-encoded as a JSON
            // literal, anything else is stored as the raw JSON text typed.
            let was_string = {
                let cur = self
                    .edit_overlay
                    .get(&state.node_idx)
                    .and_then(|e| e.value_override.as_deref())
                    .unwrap_or(tree.added_item(state.node_idx).raw_value.as_str());
                export::added_kind(cur) == NodeKind::String
            };
            let before = self.edit_overlay.get(&state.node_idx).cloned();
            let entry = self
                .edit_overlay
                .entry(state.node_idx)
                .or_insert_with(export::NodeEdit::default);
            match state.field {
                EditField::Value => {
                    entry.value_override = Some(if was_string {
                        json_string_literal(&state.text)
                    } else {
                        state.text
                    });
                }
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
                    json_string_literal(&state.text)
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
        let mut adds: Vec<AddedItem> = Vec::new();
        let mut failed = 0usize;
        for edit in &edits {
            let node_idx = match ai::tools::resolve_path(&index, &edit.path) {
                Ok(n) => n,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            // Adds are collected here and appended below — `tree` is borrowed
            // immutably for the duration of the path resolution above.
            if let ai::EditAction::AddItem { key, value } = &edit.action {
                adds.push(AddedItem {
                    parent:    node_idx,
                    key:       key.clone(),
                    raw_value: value.clone(),
                });
                continue;
            }
            let before = self.edit_overlay.get(&node_idx).cloned();
            let entry = self.edit_overlay.entry(node_idx).or_default();
            match &edit.action {
                ai::EditAction::SetValue(v)  => entry.value_override = Some(v.clone()),
                ai::EditAction::RenameKey(k) => entry.key_override = Some(k.clone()),
                ai::EditAction::Delete       => entry.deleted = true,
                ai::EditAction::AddItem { .. } => unreachable!("handled above"),
            }
            let after = self.edit_overlay.get(&node_idx).cloned();
            entries.push(UndoEntry { node_idx, before, after });
        }
        let mut add_rows = 0usize;
        if !adds.is_empty() {
            if let Some(tree) = &mut self.tree {
                let rows_before = tree.added_items.len();
                for add in &adds {
                    tree.add_item(add.parent, add.key.clone(), add.raw_value.clone());
                }
                add_rows = tree.added_items.len() - rows_before;
            }
        }
        if !entries.is_empty() || !adds.is_empty() {
            self.undo_stack.push(UndoAction::Batch { entries, adds, add_rows });
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

    /// Open the "Add Item" / "Add Property" dialog for appending a new child
    /// to `parent` (an Array or Object node).
    fn start_add_item(&mut self, parent: u32) {
        self.adding_item = Some(AddingState::new(parent));
    }

    /// Append the typed value (and, for an Object parent, key) to
    /// `added_items`, select it, and record the undoable action.
    fn commit_add_item(&mut self) {
        let Some(state) = self.adding_item.take() else { return };
        let Some(tree) = &mut self.tree else { return };
        let is_object = tree.kind_of(state.parent) == index::NodeKind::Object;
        let key = if is_object { Some(state.key.clone()) } else { None };
        let raw_value = state.text();
        let rows_before = tree.added_items.len();
        let new_id = tree.add_item(state.parent, key.clone(), raw_value.clone());
        let rows = tree.added_items.len() - rows_before;
        tree.selected = Some(new_id);
        tree.ensure_visible(new_id);
        self.push_undo_add(state.parent, key, raw_value, rows);
    }
}

/// Bound undo history so marathon editing sessions can't grow memory without
/// limit. Oldest entries fall off first.
fn cap_undo(stack: &mut Vec<UndoAction>) {
    const UNDO_CAP: usize = 1000;
    if stack.len() > UNDO_CAP {
        let excess = stack.len() - UNDO_CAP;
        stack.drain(..excess);
    }
}

/// Write `bytes` to `path` atomically (sibling temp file + rename) so that an
/// existing memory map of `path` is never truncated out from under the app.
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

// ─── paths ───────────────────────────────────────────────────────────────────

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

/// Encode `s` as a JSON string literal (quotes included).
fn json_string_literal(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// JSONPath for a pending row, walking up through any pending ancestors to
/// the real node they hang off (`build_path` only knows about real nodes).
pub fn added_path(index: &index::JsonIndex, added: &[export::AddedItem], node_idx: u32) -> String {
    let item = &added[node_idx as usize - index.nodes.len()];
    let parent_path = if export::is_added(index.nodes.len(), item.parent) {
        added_path(index, added, item.parent)
    } else {
        build_path(&index.nodes, index, item.parent)
    };
    let segment = match &item.key {
        Some(k) => path_key_segment(k),
        None    => format!("[{}]", export::added_display_index(&index.nodes, added, node_idx)),
    };
    format!("{parent_path}{segment}")
}

/// Builds a JSONPath string like `$.store.books[2].title` for `node_idx`.
pub fn build_path(nodes: &[index::Node], idx_obj: &index::JsonIndex, node_idx: u32) -> String {
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
                self.recompute_diff_if_needed();
            }
        }
        self.mode = mode;
        self.overlay = None;
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
        pane.load_progress  = 0.0;
        pane.load_rx        = Some(loader::spawn_parse(data));
        self.compare.active_pane = side.other();
    }

    fn poll_pane_loader(&mut self, side: Side) {
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
                    pane.load_rx        = None;
                }
            }
        }
        if did_load { self.compare.needs_rediff = true; }
    }

    /// Kick off a diff on a background thread when a pane changed or an option
    /// toggled. Computing inline would block the UI thread and make the window
    /// stop responding on large documents; instead we spawn and poll for the
    /// result in `poll_diff`, showing a busy note meanwhile.
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
    fn poll_diff(&mut self) {
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
        self.compare.generation += 1;
        self.diff_viewport = None;
    }

    /// Collect the result of a background update check / install watcher.
    fn poll_update(&mut self) {
        if let Some(rx) = &self.update_rx {
            match rx.try_recv() {
                Ok(msg) => {
                    self.update_rx = None;
                    match msg {
                        update::UpdateMsg::Available(info) => self.update_available = Some(info),
                        update::UpdateMsg::UpToDate => {}
                        update::UpdateMsg::Error(e) => eprintln!("update check failed: {e}"),
                        update::UpdateMsg::Installed => {}
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.update_rx = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if let Some(rx) = &self.install_watcher_rx {
            match rx.try_recv() {
                Ok(update::UpdateMsg::Installed) => update::restart_app(),
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.install_watcher_rx = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
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

    /// Pick the drop target pane from the pointer's last horizontal position.
    fn drop_side(&self) -> Side {
        let center = self.window_size.width / 2.0;
        let x = self.cursor_x.unwrap_or(center);
        if x < center { Side::Left } else { Side::Right }
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

pub fn format_count(n: usize) -> String {
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

pub fn format_size(n: u64) -> String {
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
    fn ai_changeset_adds_items_and_undoes_as_one_unit() {
        let mut app = app_with(r#"{"items": [1], "meta": {"a": 1}}"#);
        let index = Arc::clone(&app.tree.as_ref().unwrap().index);
        let name = nav(&index, &["meta", "a"]);

        app.apply_ai_edits(vec![
            ai::ProposedEdit {
                path:   "$.items".to_owned(),
                action: ai::EditAction::AddItem { key: None, value: "2".to_owned() },
                old:    "[…] (1 items)".to_owned(),
            },
            ai::ProposedEdit {
                path:   "$.meta".to_owned(),
                action: ai::EditAction::AddItem {
                    key:   Some("b".to_owned()),
                    value: "true".to_owned(),
                },
                old:    "{…} (1 keys)".to_owned(),
            },
            ai::ProposedEdit {
                path:   "$.meta.a".to_owned(),
                action: ai::EditAction::SetValue("9".to_owned()),
                old:    "1".to_owned(),
            },
        ]);

        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items.len(), 2);
        assert_eq!(t.added_items[0].raw_value, "2");
        assert_eq!(t.added_items[0].key, None);
        assert_eq!(t.added_items[1].key.as_deref(), Some("b"));
        assert_eq!(app.edit_overlay[&name].value_override.as_deref(), Some("9"));
        assert!(app.is_dirty());

        // The whole changeset — overlay edits and adds together — is one step.
        app.undo();
        assert!(app.tree.as_ref().unwrap().added_items.is_empty());
        assert!(!app.edit_overlay.contains_key(&name));
        assert!(!app.is_dirty());

        app.redo();
        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items.len(), 2);
        assert_eq!(t.added_items[1].key.as_deref(), Some("b"));
        assert_eq!(app.edit_overlay[&name].value_override.as_deref(), Some("9"));

        // The adds survive serialization.
        let json = export::json_with_edits(&index, index.root, &app.edit_overlay,
                                           &app.tree.as_ref().unwrap().added_items);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["items"], serde_json::json!([1, 2]));
        assert_eq!(v["meta"], serde_json::json!({"a": 9, "b": true}));
    }

    #[test]
    fn ai_add_into_unresolvable_path_is_reported() {
        let mut app = app_with(r#"{"items": [1]}"#);
        app.apply_ai_edits(vec![ai::ProposedEdit {
            path:   "$.nope".to_owned(),
            action: ai::EditAction::AddItem { key: None, value: "2".to_owned() },
            old:    String::new(),
        }]);
        assert!(app.tree.as_ref().unwrap().added_items.is_empty());
        assert!(!app.can_undo());
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
        app.adding_item.as_mut().unwrap().set_value("3");
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
        app.adding_item.as_mut().unwrap().set_value("2");
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
        app.adding_item.as_mut().unwrap().set_value("3");
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
    fn edits_inside_a_pasted_container_reach_the_saved_file() {
        let (mut app, path) = app_with_file(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        let base = app.tree.as_ref().unwrap().index.nodes.len() as u32;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().set_value(r#"{"a": 1, "b": "x", "c": [3]}"#);
        app.commit_add_item();
        // Rows: base = the object, then "a", "b", "c", and c's element.
        assert_eq!(app.tree.as_ref().unwrap().added_items.len(), 5);

        app.start_edit(base + 1, EditField::Value); // "a": 1 → 9
        app.editing_node.as_mut().unwrap().text = "9".to_owned();
        app.commit_edit();

        app.start_edit(base + 2, EditField::Value); // "b": string, quotes stripped
        assert_eq!(app.editing_node.as_ref().unwrap().text, "x");
        app.editing_node.as_mut().unwrap().text = "y".to_owned();
        app.commit_edit();

        app.start_edit(base + 3, EditField::Key); // "c" → "cc"
        app.editing_node.as_mut().unwrap().text = "cc".to_owned();
        app.commit_edit();

        app.toggle_delete(base + 4); // drop the 3 inside "c"

        app.start_add_item(base); // append a property to the pasted object
        {
            let st = app.adding_item.as_mut().unwrap();
            st.key = "d".to_owned();
            st.set_value("true");
        }
        app.commit_add_item();

        app.save_overwrite();
        app.wait_bg_write();
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            serde_json::json!([1, {"a": 9, "b": "y", "cc": [], "d": true}]),
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deleting_a_pasted_container_drops_the_whole_item() {
        let (mut app, path) = app_with_file(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        let base = app.tree.as_ref().unwrap().index.nodes.len() as u32;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().set_value(r#"{"a": 1}"#);
        app.commit_add_item();
        app.toggle_delete(base);

        app.save_overwrite();
        app.wait_bg_write();
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!([1]));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn undo_add_into_a_pasted_container_pops_only_that_row() {
        let mut app = app_with(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        let base = app.tree.as_ref().unwrap().index.nodes.len() as u32;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().set_value(r#"{"a": 1}"#);
        app.commit_add_item();

        app.start_add_item(base);
        {
            let st = app.adding_item.as_mut().unwrap();
            st.key = "b".to_owned();
            st.set_value("2");
        }
        app.commit_add_item();
        assert_eq!(app.tree.as_ref().unwrap().added_items.len(), 3);

        app.undo(); // just the "b" row
        assert_eq!(app.tree.as_ref().unwrap().added_items.len(), 2);
        app.undo(); // the pasted object and its row
        assert!(app.tree.as_ref().unwrap().added_items.is_empty());
        assert!(!app.is_dirty());
    }

    #[test]
    fn pasted_container_saves_as_a_single_value() {
        let (mut app, path) = app_with_file(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().set_value(r#"{"a": [1, 2]}"#);
        app.commit_add_item();
        // The pasted object shows as a subtree, but only the item itself is
        // serialized — its derived rows are display-only.
        assert_eq!(app.tree.as_ref().unwrap().added_items.len(), 4);

        app.save_overwrite();
        app.wait_bg_write();
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!([1, {"a": [1, 2]}]));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn multiline_object_added_to_array_expands_and_serializes() {
        let mut app = app_with(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        let base = app.tree.as_ref().unwrap().index.nodes.len() as u32;
        app.start_add_item(root);
        // Typed / pasted the way a pretty-printed object arrives: newlines
        // and indentation, which the single-line input used to drop.
        app.adding_item
            .as_mut()
            .unwrap()
            .set_value("{\n  \"id\": 1,\n  \"tags\": [\n    \"a\"\n  ]\n}");
        assert!(app.add_item_valid());
        app.commit_add_item();

        let t = app.tree.as_ref().unwrap();
        // The object, "id", "tags", and tags' one element.
        assert_eq!(t.added_items.len(), 4);
        assert_eq!(t.kind_of(base), index::NodeKind::Object);
        assert_eq!(t.selected, Some(base));

        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &t.added_items);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([1, {"id": 1, "tags": ["a"]}]));
    }

    #[test]
    fn multiline_array_added_as_object_property() {
        let mut app = app_with(r#"{"a": 1}"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        {
            let st = app.adding_item.as_mut().unwrap();
            st.key = "b".to_owned();
            st.set_value("[\n  1,\n  {\"c\": 2}\n]");
        }
        assert!(app.add_item_valid());
        app.commit_add_item();

        let t = app.tree.as_ref().unwrap();
        // The array, its two elements, and "c" inside the second one.
        assert_eq!(t.added_items.len(), 4);
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &t.added_items);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1, "b": [1, {"c": 2}]}));
    }

    #[test]
    fn format_prettifies_valid_json_and_leaves_invalid_text_alone() {
        let mut app = app_with(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);

        let st = app.adding_item.as_mut().unwrap();
        st.set_value(r#"{"a":[1,2]}"#);
        st.format();
        assert_eq!(st.text(), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");

        st.set_value(r#"{"a":"#);
        st.format();
        assert_eq!(st.text(), r#"{"a":"#);
        assert!(!app.add_item_valid());
    }

    #[test]
    fn undo_add_container_removes_its_rows() {
        let mut app = app_with(r#"[1]"#);
        let root = app.tree.as_ref().unwrap().index.root;
        app.start_add_item(root);
        app.adding_item.as_mut().unwrap().set_value(r#"{"a": 1}"#);
        app.commit_add_item();
        assert_eq!(app.tree.as_ref().unwrap().added_items.len(), 2);

        app.undo();
        assert!(app.tree.as_ref().unwrap().added_items.is_empty());
        assert!(!app.is_dirty());

        app.redo();
        let t = app.tree.as_ref().unwrap();
        assert_eq!(t.added_items.len(), 2);
        assert_eq!(t.added_items[0].raw_value, r#"{"a": 1}"#);
        assert_eq!(t.added_items[1].key.as_deref(), Some("a"));
    }

    #[test]
    fn add_property_appends_keyed_item_to_object() {
        let mut app = app_with(r#"{"a": 1}"#);
        let root = app.tree.as_ref().unwrap().index.root;

        app.start_add_item(root);
        {
            let state = app.adding_item.as_mut().unwrap();
            state.key = "b".to_owned();
            state.set_value("2");
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
            state.set_value("2");
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
        app.adding_item.as_mut().unwrap().set_value("2");
        app.commit_add_item();
        let new_id = app.tree.as_ref().unwrap().selected.unwrap();

        app.toggle_delete(new_id);
        let t = app.tree.as_ref().unwrap();
        let out = export::json_with_edits(&t.index, t.index.root, &app.edit_overlay, &t.added_items);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([1]));
    }

    #[test]
    fn scroll_target_centres_and_reveals() {
        // Centre: row 100 of 1000 with 20px rows in a 400px viewport.
        let y = scroll_target(Some(100), None, 20.0, 0.0, 400.0, 1000).unwrap();
        assert_eq!(y, 100.0 * 20.0 - 200.0 + 10.0);
        // Reveal: already visible → no scroll.
        assert!(scroll_target(None, Some(5), 20.0, 0.0, 400.0, 1000).is_none());
        // Reveal below → scroll so the row's bottom edge is at the viewport bottom.
        assert_eq!(scroll_target(None, Some(30), 20.0, 0.0, 400.0, 1000), Some(31.0 * 20.0 - 400.0));
        // Reveal above → scroll so the row's top is at the viewport top.
        assert_eq!(scroll_target(None, Some(2), 20.0, 100.0, 400.0, 1000), Some(40.0));
    }
}
