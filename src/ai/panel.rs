//! Chat side panel + AI settings section. All chat state lives in
//! `AiPanelState` on the App; applying a reviewed changeset is returned to
//! the caller (main.rs), which owns the edit overlay and undo stacks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, container, markdown, pick_list, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Element, Font, Length};
use serde_json::Value;

use crate::index::JsonIndex;
use crate::settings::Settings;
use crate::theme;

use super::keystore;
use super::provider::{self, ProviderConfig, ProviderKind};
use super::session::{self, AiMsg, ChatEntry};
use super::tools::{EditAction, ProposedEdit};

pub const AI_INPUT_ID: &str = "ai_input";

// ─── settings section ────────────────────────────────────────────────────────

/// Transient UI state for the settings window's AI section (the key text
/// buffer never touches the persisted settings blob).
#[derive(Default)]
pub struct AiSettingsUi {
    key_input:  String,
    key_status: Option<Result<&'static str, String>>,
    /// Cached "a key exists in the Keychain" check, per provider.
    key_cached: std::cell::Cell<Option<(ProviderKind, bool)>>,
    /// Set when the settings window was opened *because* the user asked for
    /// the assistant without having configured it: the AI section is
    /// highlighted until the window closes.
    pub focus:  bool,
}

impl AiSettingsUi {
    /// Whether an API key for `kind` is stored. Cached, so the toolbar can
    /// call it every frame without hitting the platform credential store.
    pub fn key_present(&self, kind: ProviderKind) -> bool {
        match self.key_cached.get() {
            Some((k, present)) if k == kind => present,
            _ => {
                let present = keystore::get_key(kind.key_account()).is_some();
                self.key_cached.set(Some((kind, present)));
                present
            }
        }
    }

    fn invalidate(&self) {
        self.key_cached.set(None);
    }
}

#[derive(Clone, Debug)]
pub enum AiSettingsMsg {
    Enabled(bool),
    Provider(ProviderKind),
    Model(String),
    BaseUrl(String),
    KeyInput(String),
    SaveKey,
    RemoveKey,
}

pub fn apply_settings_msg(settings: &mut Settings, state: &mut AiSettingsUi, msg: AiSettingsMsg) {
    match msg {
        AiSettingsMsg::Enabled(b) => settings.ai_enabled = b,
        AiSettingsMsg::Provider(kind) => {
            if settings.ai_provider != kind {
                settings.ai_provider = kind;
                state.invalidate();
                state.key_status = None;
            }
        }
        AiSettingsMsg::Model(m) => settings.ai_model = m,
        AiSettingsMsg::BaseUrl(u) => settings.ai_base_url = u,
        AiSettingsMsg::KeyInput(k) => state.key_input = k,
        AiSettingsMsg::SaveKey => {
            let kind = settings.ai_provider;
            if !state.key_input.trim().is_empty() {
                state.key_status = Some(
                    keystore::set_key(kind.key_account(), state.key_input.trim())
                        .map(|_| "Saved to Keychain"),
                );
                state.key_input.clear();
                state.invalidate();
            }
        }
        AiSettingsMsg::RemoveKey => {
            keystore::delete_key(settings.ai_provider.key_account());
            state.key_status = Some(Ok("Key removed"));
            state.invalidate();
        }
    }
}

const FS: f32 = 14.0;

fn label<'a>(s: &'a str) -> Element<'a, AiSettingsMsg> {
    text(s).size(FS).width(Length::Fixed(170.0)).into()
}

pub fn settings_section<'a>(
    settings: &'a Settings,
    state: &'a AiSettingsUi,
    pal: &theme::Palette,
) -> Element<'a, AiSettingsMsg> {
    let body = settings_body(settings, state, pal);
    if state.focus {
        // Highlight the section when the user got here by clicking the AI
        // button on an unconfigured install.
        container(body)
            .padding(8)
            .style(theme::outline_style(pal.accent))
            .into()
    } else {
        body
    }
}

fn settings_body<'a>(
    settings: &'a Settings,
    state: &'a AiSettingsUi,
    pal: &theme::Palette,
) -> Element<'a, AiSettingsMsg> {
    let mut col = column![
        text("AI Assistant").size(18),
        space().height(8),
        row![
            label("Enable AI features"),
            checkbox(settings.ai_enabled).on_toggle(AiSettingsMsg::Enabled),
        ]
        .align_y(iced::Center),
        text("Bring your own API key. When enabled, snippets of the open file are sent to the provider you configure below.")
            .size(12)
            .color(pal.text_muted),
    ]
    .spacing(8);

    if !settings.ai_enabled {
        return col.into();
    }

    let hint = match settings.ai_provider {
        ProviderKind::Anthropic => "https://api.anthropic.com",
        ProviderKind::OpenAiCompatible => "https://api.openai.com/v1",
    };
    col = col.push(
        row![
            label("Provider"),
            pick_list(
                &[ProviderKind::Anthropic, ProviderKind::OpenAiCompatible][..],
                Some(settings.ai_provider),
                AiSettingsMsg::Provider,
            )
            .width(200)
            .text_size(FS)
            .style(theme::pick_list_style),
        ]
        .align_y(iced::Center),
    );
    col = col.push(
        row![
            label("Model"),
            text_input(settings.ai_provider.default_model(), &settings.ai_model)
                .on_input(AiSettingsMsg::Model)
                .size(FS)
                .width(200)
                .style(theme::input_style),
        ]
        .align_y(iced::Center),
    );
    col = col.push(
        row![
            label("Base URL"),
            text_input(hint, &settings.ai_base_url)
                .on_input(AiSettingsMsg::BaseUrl)
                .size(FS)
                .width(200)
                .style(theme::input_style),
        ]
        .align_y(iced::Center),
    );

    // ── API key (stored in the macOS Keychain, never in settings) ──
    let kind = settings.ai_provider;
    let present = state.key_present(kind);
    let can_save = !state.key_input.trim().is_empty();
    let mut key_row = row![
        label("API key"),
        text_input(if present { "•••••• (saved)" } else { "paste key" }, &state.key_input)
            .on_input(AiSettingsMsg::KeyInput)
            .secure(true)
            .size(FS)
            .width(180)
            .style(theme::input_style),
        button(text("Save").size(FS))
            .style(theme::button_style)
            .on_press_maybe(can_save.then_some(AiSettingsMsg::SaveKey)),
    ]
    .spacing(6)
    .align_y(iced::Center);
    if present {
        key_row = key_row.push(
            button(text("Remove").size(FS))
                .style(theme::button_style)
                .on_press(AiSettingsMsg::RemoveKey),
        );
    }
    col = col.push(key_row);

    match &state.key_status {
        Some(Ok(msg)) => {
            col = col.push(text(*msg).size(12).color(theme::OK_GREEN));
        }
        Some(Err(e)) => {
            col = col.push(text(format!("✗ {e}")).size(12).color(theme::ERR_RED));
        }
        None => {}
    }
    col = col.push(
        text("The key is stored in the macOS Keychain. When you use the assistant, parts of the open file are sent to the provider.")
            .size(12)
            .color(pal.text_faint),
    );
    col.into()
}

// ─── chat panel state ────────────────────────────────────────────────────────

/// A transcript entry plus, for assistant messages, its parsed markdown.
struct Item {
    entry: ChatEntry,
    md:    Option<markdown::Content>,
}

impl Item {
    fn new(entry: ChatEntry) -> Self {
        let md = match &entry {
            ChatEntry::Assistant(t) => Some(markdown::Content::parse(t)),
            _ => None,
        };
        Self { entry, md }
    }
}

pub struct AiPanelState {
    pub open:       bool,
    /// Width of the side panel in logical pixels (user-resizable).
    pub width:      f32,
    input:          String,
    transcript:     Vec<Item>,
    /// Provider-native message history for the current conversation.
    history:        Vec<Value>,
    /// Provider the current history was built for — switching providers
    /// resets the conversation (message formats are incompatible).
    history_kind:   Option<ProviderKind>,
    rx:             Option<std::sync::mpsc::Receiver<AiMsg>>,
    cancel:         Arc<AtomicBool>,
    /// Pending changeset awaiting review: (edit, include-checkbox).
    proposal:       Vec<(ProposedEdit, bool)>,
}

impl Default for AiPanelState {
    fn default() -> Self {
        Self {
            open:          false,
            width:         360.0,
            input:         String::new(),
            transcript:    Vec::new(),
            history:       Vec::new(),
            history_kind:  None,
            rx:            None,
            cancel:        Arc::new(AtomicBool::new(false)),
            proposal:      Vec::new(),
        }
    }
}

impl AiPanelState {
    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Drain messages from the agent thread. Called from the app's tick.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(AiMsg::Assistant(t)) => self.transcript.push(Item::new(ChatEntry::Assistant(t))),
                Ok(AiMsg::ToolNote(t))  => self.transcript.push(Item::new(ChatEntry::Note(t))),
                Ok(AiMsg::Proposal(edits)) => {
                    self.proposal = edits.into_iter().map(|e| (e, true)).collect();
                }
                Ok(AiMsg::Error(e)) => self.transcript.push(Item::new(ChatEntry::Error(e))),
                Ok(AiMsg::Done { history }) => {
                    self.history = history;
                    self.rx = None;
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    return;
                }
            }
        }
    }

    fn send(&mut self, settings: &Settings, index: &Arc<JsonIndex>, file_name: &str) {
        let text = self.input.trim().to_owned();
        if text.is_empty() || self.busy() {
            return;
        }
        let kind = settings.ai_provider;
        let Some(api_key) = keystore::get_key(kind.key_account()) else {
            self.transcript.push(Item::new(ChatEntry::Error(
                "No API key configured — add one in Settings → AI Assistant.".to_owned(),
            )));
            return;
        };
        if self.history_kind != Some(kind) {
            self.history.clear();
            self.history_kind = Some(kind);
        }
        self.input.clear();
        self.transcript.push(Item::new(ChatEntry::User(text.clone())));
        self.history.push(provider::user_message(kind, &text));

        let model = if settings.ai_model.trim().is_empty() {
            kind.default_model().to_owned()
        } else {
            settings.ai_model.trim().to_owned()
        };
        let cfg = ProviderConfig {
            kind,
            api_key,
            model,
            base_url: settings.ai_base_url.clone(),
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        self.rx = Some(session::spawn_turn(
            cfg,
            Arc::clone(index),
            file_name.to_owned(),
            self.history.clone(),
            Arc::clone(&self.cancel),
        ));
    }

    /// Append a status note to the transcript (used by the App when applying
    /// a changeset partially fails, e.g. after a reload).
    pub fn note(&mut self, text: String) {
        self.transcript.push(Item::new(ChatEntry::Note(text)));
    }

    /// Reset the conversation (e.g. via the panel's Clear button).
    fn clear(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
        self.transcript.clear();
        self.history.clear();
        self.proposal.clear();
    }
}

// ─── panel messages / update ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum AiPanelMsg {
    Input(String),
    Send,
    Stop,
    Clear,
    Close,
    ToggleEdit(usize, bool),
    Apply,
    Reject,
    Link(String),
}

/// Apply a panel message. Returns `Some(edits)` when the user clicked Apply on
/// the reviewed changeset — the caller applies them through the overlay.
pub fn update(
    state: &mut AiPanelState,
    msg: AiPanelMsg,
    settings: &Settings,
    index: Option<&Arc<JsonIndex>>,
    file_name: &str,
) -> Option<Vec<ProposedEdit>> {
    match msg {
        AiPanelMsg::Input(s) => state.input = s,
        AiPanelMsg::Send => {
            if let Some(index) = index {
                state.send(settings, index, file_name);
            }
        }
        AiPanelMsg::Stop => state.cancel.store(true, Ordering::Relaxed),
        AiPanelMsg::Clear => state.clear(),
        AiPanelMsg::Close => state.open = false,
        AiPanelMsg::ToggleEdit(i, on) => {
            if let Some((_, c)) = state.proposal.get_mut(i) {
                *c = on;
            }
        }
        AiPanelMsg::Apply => {
            let edits: Vec<ProposedEdit> = state
                .proposal
                .iter()
                .filter(|(_, c)| *c)
                .map(|(e, _)| e.clone())
                .collect();
            if !edits.is_empty() {
                state.proposal.clear();
                state.transcript.push(Item::new(ChatEntry::Note(
                    "Edits applied — review them in the tree, then Save.".to_owned(),
                )));
                return Some(edits);
            }
        }
        AiPanelMsg::Reject => state.proposal.clear(),
        AiPanelMsg::Link(url) => crate::open_in_browser(&url),
    }
    None
}

// ─── panel UI ────────────────────────────────────────────────────────────────

pub fn view<'a>(
    state: &'a AiPanelState,
    settings: &'a Settings,
    has_doc: bool,
    pal: &theme::Palette,
    font: Font,
    fs: f32,
) -> Element<'a, AiPanelMsg> {
    let small = (fs - 2.0).max(10.0);

    let mut header = row![
        row![text("✨").size(fs), text("AI Assistant").size(fs).font(font)]
            .spacing(5)
            .align_y(iced::Center),
        space().width(Length::Fill),
    ]
    .spacing(6)
    .align_y(iced::Center);
    if !state.transcript.is_empty() {
        header = header.push(
            button(text("Clear").size(small).font(font))
                .style(theme::button_style)
                .on_press(AiPanelMsg::Clear),
        );
    }
    header = header.push(
        button(text("✕").size((fs * 1.3).round()).font(font))
            .style(theme::flat_button_style)
            .on_press(AiPanelMsg::Close),
    );

    let mut col = column![
        space().height(6),
        header,
        text(format!("Sends parts of this file to {}.", settings.ai_provider.label()))
            .size(small)
            .color(pal.text_faint),
        space().height(4),
        rule::horizontal(1).style(theme::divider_style),
    ]
    .spacing(4)
    .height(Length::Fill);

    if !has_doc {
        col = col.push(space().height(12));
        col = col.push(text("Open a JSON document to use the assistant.").size(fs).font(font).color(pal.text_muted));
        return col.into();
    }

    // ── transcript ──
    let mut body = column![space().height(6)].spacing(4).width(Length::Fill);
    if state.transcript.is_empty() {
        body = body.push(
            text(
                "Ask questions (“which orders have a negative total?”), run complex queries, \
                 or request bulk edits (“uppercase every country field”). Proposed edits \
                 always wait for your review.",
            )
            .size(fs)
            .font(font)
            .color(pal.text_muted),
        );
    }
    for item in &state.transcript {
        body = body.push(transcript_entry(item, pal, font, fs));
    }
    if !state.proposal.is_empty() {
        body = body.push(proposal_card(&state.proposal, pal, fs));
    }

    let transcript = scrollable(container(body).padding([0, 6]).width(Length::Fill))
        .height(Length::Fill)
        .anchor_bottom()
        .style(theme::scrollable_style);
    col = col.push(transcript);

    // ── input row ──
    let can_send = !state.busy() && !state.input.trim().is_empty();
    let mut input_row = row![].spacing(6).align_y(iced::Center);
    if state.busy() {
        input_row = input_row.push(text("…").size(fs).color(pal.text_muted));
        input_row = input_row.push(
            button(text("Stop").size(small).font(font))
                .style(theme::button_style)
                .on_press(AiPanelMsg::Stop),
        );
    }
    input_row = input_row.push(
        text_input("Ask about or edit this file…", &state.input)
            .id(AI_INPUT_ID)
            .on_input(AiPanelMsg::Input)
            .on_submit_maybe(can_send.then_some(AiPanelMsg::Send))
            .size(fs)
            .font(font)
            .width(Length::Fill)
            .style(theme::input_style),
    );
    input_row = input_row.push(
        button(text("Send").size(small).font(font))
            .style(theme::button_style)
            .on_press_maybe(can_send.then_some(AiPanelMsg::Send)),
    );
    col = col.push(container(input_row).padding([6, 0]));

    col.into()
}

fn transcript_entry<'a>(item: &'a Item, pal: &theme::Palette, font: Font, fs: f32) -> Element<'a, AiPanelMsg> {
    let small = (fs - 2.0).max(10.0);
    match &item.entry {
        ChatEntry::User(t) => container(text(t.as_str()).size(fs).font(font).color(pal.text_primary))
            .padding(8)
            .width(Length::Fill)
            .style(theme::bubble_style(pal.selection_bg))
            .into(),
        ChatEntry::Assistant(t) => {
            let content: Element<'a, AiPanelMsg> = match &item.md {
                Some(md) => {
                    let style = markdown::Style {
                        font,
                        inline_code_highlight: iced::advanced::text::Highlight {
                            background: iced::Background::Color(pal.bg_search),
                            border: iced::border::rounded(4),
                        },
                        inline_code_padding: iced::padding::left(2).right(2),
                        inline_code_color: pal.key,
                        inline_code_font: Font::MONOSPACE,
                        code_block_font: Font::MONOSPACE,
                        link_color: pal.accent,
                    };
                    let settings = markdown::Settings::with_text_size(fs, style);
                    markdown::view(md.items(), settings).map(AiPanelMsg::Link)
                }
                None => text(t.as_str()).size(fs).font(font).into(),
            };
            container(content)
                .padding(8)
                .width(Length::Fill)
                .style(theme::bubble_style(pal.hover_bg))
                .into()
        }
        ChatEntry::Note(t) => text(format!("· {t}")).size(small).font(font).color(pal.text_muted).into(),
        ChatEntry::Error(t) => text(t.as_str()).size(fs).font(font).color(theme::DELETED).into(),
    }
}

/// Render the pending changeset with per-edit include checkboxes.
fn proposal_card<'a>(
    proposal: &'a [(ProposedEdit, bool)],
    pal: &theme::Palette,
    fs: f32,
) -> Element<'a, AiPanelMsg> {
    let small = (fs - 2.0).max(10.0);
    let mut col = column![
        text(format!("Proposed edits ({})", proposal.len())).size(fs),
        space().height(4),
    ]
    .spacing(4)
    .width(Length::Fill);

    for (i, (edit, checked)) in proposal.iter().enumerate() {
        col = col.push(
            row![
                checkbox(*checked).on_toggle(move |on| AiPanelMsg::ToggleEdit(i, on)),
                text(edit.path.as_str()).size(small).font(Font::MONOSPACE).color(pal.key),
            ]
            .spacing(6)
            .align_y(iced::Center),
        );
        let detail: Element<'a, AiPanelMsg> = match &edit.action {
            EditAction::SetValue(v) => text(format!("{} → {}", edit.old, v)).size(small).font(Font::MONOSPACE).into(),
            EditAction::RenameKey(k) => text(format!("key: {} → {}", edit.old, k)).size(small).font(Font::MONOSPACE).into(),
            EditAction::Delete => text(format!("delete ({})", edit.old))
                .size(small)
                .font(Font::MONOSPACE)
                .color(theme::DELETED)
                .into(),
            EditAction::AddItem { key, value } => {
                let what = match key {
                    Some(k) => format!("add {k}: {value}"),
                    None    => format!("append {value}"),
                };
                text(format!("{what}  → {}", edit.old)).size(small).font(Font::MONOSPACE).color(theme::NEW).into()
            }
        };
        col = col.push(container(detail).padding([0, 24]));
    }

    let n = proposal.iter().filter(|(_, c)| *c).count();
    col = col.push(space().height(6));
    col = col.push(
        row![
            button(text(format!("Apply {n} edit(s)")).size(small))
                .style(theme::accent_button_style)
                .on_press_maybe((n > 0).then_some(AiPanelMsg::Apply)),
            button(text("Reject").size(small))
                .style(theme::button_style)
                .on_press(AiPanelMsg::Reject),
        ]
        .spacing(6),
    );

    container(col)
        .padding(8)
        .width(Length::Fill)
        .style(theme::outline_style(pal.accent))
        .into()
}
