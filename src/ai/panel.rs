//! Chat side panel + AI settings section. All chat state lives in
//! `AiPanelState` on the App; applying a reviewed changeset is returned to
//! the caller (main.rs), which owns the edit overlay and undo stacks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::index::JsonIndex;
use crate::settings::Settings;
use crate::theme;

use super::keystore;
use super::provider::{self, ProviderConfig, ProviderKind};
use super::session::{self, AiMsg, ChatEntry};
use super::tools::{EditAction, ProposedEdit};

// ─── settings section ────────────────────────────────────────────────────────

/// Transient UI state for the settings window's AI section (the key text
/// buffer never touches the persisted settings blob).
#[derive(Default)]
pub struct AiSettingsUi {
    key_input:  String,
    key_status: Option<Result<&'static str, String>>,
    /// Cached "a key exists in the Keychain" check, per provider.
    key_cached: Option<(ProviderKind, bool)>,
}

impl AiSettingsUi {
    fn key_present(&mut self, kind: ProviderKind) -> bool {
        match self.key_cached {
            Some((k, present)) if k == kind => present,
            _ => {
                let present = keystore::get_key(kind.key_account()).is_some();
                self.key_cached = Some((kind, present));
                present
            }
        }
    }

    fn invalidate(&mut self) {
        self.key_cached = None;
    }
}

pub fn settings_section(ui: &mut egui::Ui, settings: &mut Settings, state: &mut AiSettingsUi) {
    ui.heading("AI Assistant");
    ui.add_space(8.0);

    egui::Grid::new("ai_grid")
        .num_columns(2)
        .spacing([24.0, 10.0])
        .show(ui, |ui| {
            ui.label("Enable AI features");
            ui.checkbox(&mut settings.ai_enabled, "")
                .on_hover_text("Bring your own API key. When enabled, snippets of the open file are sent to the provider you configure below.");
            ui.end_row();

            if settings.ai_enabled {
                ui.label("Provider");
                egui::ComboBox::from_id_salt("ai_provider_combo")
                    .width(160.0)
                    .selected_text(settings.ai_provider.label())
                    .show_ui(ui, |ui| {
                        for kind in [ProviderKind::Anthropic, ProviderKind::OpenAiCompatible] {
                            if ui
                                .selectable_value(&mut settings.ai_provider, kind, kind.label())
                                .changed()
                            {
                                state.invalidate();
                                state.key_status = None;
                            }
                        }
                    });
                ui.end_row();

                ui.label("Model");
                ui.add(
                    egui::TextEdit::singleline(&mut settings.ai_model)
                        .desired_width(200.0)
                        .hint_text(settings.ai_provider.default_model()),
                );
                ui.end_row();

                ui.label("Base URL");
                let hint = match settings.ai_provider {
                    ProviderKind::Anthropic => "https://api.anthropic.com",
                    ProviderKind::OpenAiCompatible => "https://api.openai.com/v1",
                };
                ui.add(
                    egui::TextEdit::singleline(&mut settings.ai_base_url)
                        .desired_width(200.0)
                        .hint_text(hint),
                );
                ui.end_row();
            }
        });

    if !settings.ai_enabled {
        return;
    }

    ui.add_space(8.0);

    // ── API key (stored in the macOS Keychain, never in settings) ──
    let kind = settings.ai_provider;
    let present = state.key_present(kind);
    ui.horizontal(|ui| {
        ui.label("API key");
        ui.add(
            egui::TextEdit::singleline(&mut state.key_input)
                .desired_width(180.0)
                .password(true)
                .hint_text(if present { "•••••• (saved)" } else { "paste key" }),
        );
        if ui
            .add_enabled(!state.key_input.trim().is_empty(), egui::Button::new("Save"))
            .clicked()
        {
            state.key_status = Some(
                keystore::set_key(kind.key_account(), state.key_input.trim())
                    .map(|_| "Saved to Keychain"),
            );
            state.key_input.clear();
            state.invalidate();
        }
        if present && ui.button("Remove").clicked() {
            keystore::delete_key(kind.key_account());
            state.key_status = Some(Ok("Key removed"));
            state.invalidate();
        }
    });
    match &state.key_status {
        Some(Ok(msg)) => {
            ui.label(egui::RichText::new(*msg).small().color(egui::Color32::from_rgb(52, 199, 89)));
        }
        Some(Err(e)) => {
            ui.label(egui::RichText::new(format!("✗ {e}")).small().color(egui::Color32::from_rgb(255, 69, 58)));
        }
        None => {}
    }
    ui.label(
        egui::RichText::new("The key is stored in the macOS Keychain. When you use the assistant, parts of the open file are sent to the provider.")
            .small()
            .weak(),
    );
}

// ─── chat panel state ────────────────────────────────────────────────────────

pub struct AiPanelState {
    pub open:       bool,
    input:          String,
    transcript:     Vec<ChatEntry>,
    /// Provider-native message history for the current conversation.
    history:        Vec<Value>,
    /// Provider the current history was built for — switching providers
    /// resets the conversation (message formats are incompatible).
    history_kind:   Option<ProviderKind>,
    rx:             Option<std::sync::mpsc::Receiver<AiMsg>>,
    cancel:         Arc<AtomicBool>,
    /// Pending changeset awaiting review: (edit, include-checkbox).
    proposal:       Vec<(ProposedEdit, bool)>,
    scroll_to_end:  bool,
}

impl Default for AiPanelState {
    fn default() -> Self {
        Self {
            open:          false,
            input:         String::new(),
            transcript:    Vec::new(),
            history:       Vec::new(),
            history_kind:  None,
            rx:            None,
            cancel:        Arc::new(AtomicBool::new(false)),
            proposal:      Vec::new(),
            scroll_to_end: false,
        }
    }
}

impl AiPanelState {
    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Drain messages from the agent thread. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(AiMsg::Assistant(t)) => {
                    self.transcript.push(ChatEntry::Assistant(t));
                    self.scroll_to_end = true;
                }
                Ok(AiMsg::ToolNote(t)) => {
                    self.transcript.push(ChatEntry::Note(t));
                    self.scroll_to_end = true;
                }
                Ok(AiMsg::Proposal(edits)) => {
                    self.proposal = edits.into_iter().map(|e| (e, true)).collect();
                    self.scroll_to_end = true;
                }
                Ok(AiMsg::Error(e)) => {
                    self.transcript.push(ChatEntry::Error(e));
                    self.scroll_to_end = true;
                }
                Ok(AiMsg::Done { history }) => {
                    self.history = history;
                    self.rx = None;
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
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
            self.transcript.push(ChatEntry::Error(
                "No API key configured — add one in Settings → AI Assistant.".to_owned(),
            ));
            return;
        };
        if self.history_kind != Some(kind) {
            self.history.clear();
            self.history_kind = Some(kind);
        }
        self.input.clear();
        self.transcript.push(ChatEntry::User(text.clone()));
        self.scroll_to_end = true;
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
        self.transcript.push(ChatEntry::Note(text));
        self.scroll_to_end = true;
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

// ─── panel UI ────────────────────────────────────────────────────────────────

/// Render the panel body. Returns `Some(edits)` when the user clicked Apply
/// on the reviewed changeset — the caller applies them through the overlay.
pub fn show(
    state: &mut AiPanelState,
    ui: &mut egui::Ui,
    settings: &Settings,
    index: Option<&Arc<JsonIndex>>,
    file_name: &str,
) -> Option<Vec<ProposedEdit>> {
    let pal = theme::Palette::for_dark(ui.visuals().dark_mode);
    let mut apply: Option<Vec<ProposedEdit>> = None;

    // Header
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("✨ AI Assistant").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("X").on_hover_text("Close").clicked() {
                state.open = false;
            }
            if !state.transcript.is_empty() && ui.button("Clear").on_hover_text("New conversation").clicked() {
                state.clear();
            }
        });
    });
    ui.label(
        egui::RichText::new(format!(
            "Sends parts of this file to {}.",
            settings.ai_provider.label()
        ))
        .small()
        .color(pal.text_faint),
    );
    ui.separator();

    if index.is_none() {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Open a JSON document to use the assistant.").color(pal.text_muted));
        return None;
    }

    // ── Input row at the bottom; transcript fills the rest ──
    let mut send_clicked = false;
    egui::Panel::bottom("ai_input_row")
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 6)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if state.busy() {
                    ui.spinner();
                    if ui.button("Stop").clicked() {
                        state.cancel.store(true, Ordering::Relaxed);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_send = !state.busy() && !state.input.trim().is_empty();
                    if ui.add_enabled(can_send, egui::Button::new("Send")).clicked() {
                        send_clicked = true;
                    }
                    let te = egui::TextEdit::singleline(&mut state.input)
                        .hint_text("Ask about or edit this file…")
                        .desired_width(ui.available_width());
                    let resp = ui.add(te);
                    if resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && can_send
                    {
                        send_clicked = true;
                        resp.request_focus();
                    }
                });
            });
        });

    egui::CentralPanel::default()
        .frame(egui::Frame::new())
        .show_inside(ui, |ui| {
            let stick = state.scroll_to_end;
            state.scroll_to_end = false;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    if state.transcript.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "Ask questions (“which orders have a negative total?”), run \
                                 complex queries, or request bulk edits (“uppercase every \
                                 country field”). Proposed edits always wait for your review.",
                            )
                            .color(pal.text_muted),
                        );
                    }
                    for entry in &state.transcript {
                        transcript_entry(ui, &pal, entry);
                    }
                    if !state.proposal.is_empty() {
                        if let Some(edits) = proposal_card(ui, &pal, &mut state.proposal) {
                            apply = Some(edits);
                        }
                    }
                    if stick {
                        ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                    }
                });
        });

    if send_clicked {
        if let Some(index) = index {
            state.send(settings, index, file_name);
        }
    }
    if apply.is_some() {
        state.proposal.clear();
        state
            .transcript
            .push(ChatEntry::Note("Edits applied — review them in the tree, then Save.".to_owned()));
    }
    apply
}

fn transcript_entry(ui: &mut egui::Ui, pal: &theme::Palette, entry: &ChatEntry) {
    ui.add_space(4.0);
    match entry {
        ChatEntry::User(t) => {
            egui::Frame::new()
                .fill(pal.selection_bg)
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(t).color(pal.text_primary));
                });
        }
        ChatEntry::Assistant(t) => {
            egui::Frame::new()
                .fill(pal.hover_bg)
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    super::markdown::render(ui, pal, t);
                });
        }
        ChatEntry::Note(t) => {
            ui.label(egui::RichText::new(format!("· {t}")).small().color(pal.text_muted));
        }
        ChatEntry::Error(t) => {
            ui.label(egui::RichText::new(t).color(theme::DELETED));
        }
    }
}

/// Render the pending changeset. Returns the checked edits when Apply is
/// clicked; clears the proposal on Reject.
fn proposal_card(
    ui: &mut egui::Ui,
    pal: &theme::Palette,
    proposal: &mut Vec<(ProposedEdit, bool)>,
) -> Option<Vec<ProposedEdit>> {
    let mut result = None;
    let mut reject = false;
    ui.add_space(6.0);
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0_f32, pal.accent))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(format!("Proposed edits ({})", proposal.len())).strong(),
            );
            ui.add_space(4.0);
            for (edit, checked) in proposal.iter_mut() {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(checked, "");
                    ui.label(
                        egui::RichText::new(&edit.path).monospace().small().color(pal.key),
                    );
                });
                ui.indent(&edit.path, |ui| match &edit.action {
                    EditAction::SetValue(v) => {
                        ui.label(
                            egui::RichText::new(format!("{} → {}", edit.old, v))
                                .monospace()
                                .small(),
                        );
                    }
                    EditAction::RenameKey(k) => {
                        ui.label(
                            egui::RichText::new(format!("key: {} → {}", edit.old, k))
                                .monospace()
                                .small(),
                        );
                    }
                    EditAction::Delete => {
                        ui.label(
                            egui::RichText::new(format!("delete ({})", edit.old))
                                .monospace()
                                .small()
                                .color(theme::DELETED),
                        );
                    }
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let n = proposal.iter().filter(|(_, c)| *c).count();
                let apply_btn = egui::Button::new(
                    egui::RichText::new(format!("Apply {n} edit(s)")).color(egui::Color32::WHITE),
                )
                .fill(pal.accent);
                if ui.add_enabled(n > 0, apply_btn).clicked() {
                    result = Some(
                        proposal
                            .iter()
                            .filter(|(_, c)| *c)
                            .map(|(e, _)| e.clone())
                            .collect(),
                    );
                }
                if ui.button("Reject").clicked() {
                    reject = true;
                }
            });
        });
    if reject {
        proposal.clear();
    }
    result
}
