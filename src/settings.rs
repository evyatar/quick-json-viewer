use std::path::PathBuf;

use iced::widget::{button, checkbox, column, container, pick_list, row, rule, slider, space, text};
use iced::{Element, Font, Length};
use serde::{Deserialize, Serialize};

use crate::theme;

const STORAGE_KEY: &str = "json_viewer_settings_v1";
const APP_DIR: &str = "quick-json-viewer";

#[cfg(target_os = "macos")]
#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSSetDefaultRoleHandlerForContentType(
        content_type: *const std::ffi::c_void,
        role: u32,
        handler: *const std::ffi::c_void,
    ) -> i32;
}

fn set_as_default_json_viewer() -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::NSString;
        unsafe {
            let uti = NSString::from_str("public.json");
            let bundle = NSString::from_str("com.evyatar.quick-json-viewer");
            LSSetDefaultRoleHandlerForContentType(
                &*uti as *const NSString as *const std::ffi::c_void,
                0xFFFF_FFFF, // kLSRolesAll
                &*bundle as *const NSString as *const std::ffi::c_void,
            ) == 0
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Auto, Theme::Light, Theme::Dark];
}

impl std::fmt::Display for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Theme::Auto => "Auto",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FontFamily {
    // Generic families — resolved by the system to whatever is installed.
    Proportional,
    #[default]
    Monospace,
    Serif,
    // Monospace fonts commonly found on macOS / Windows / Linux.
    Menlo,
    Monaco,
    SfMono,
    Consolas,
    CourierNew,
    JetBrainsMono,
    FiraCode,
    SourceCodePro,
    DejaVuSansMono,
    UbuntuMono,
    // Proportional fonts commonly found on macOS / Windows / Linux.
    Helvetica,
    Arial,
    Verdana,
    SegoeUi,
    Georgia,
    TimesNewRoman,
    DejaVuSans,
}

impl FontFamily {
    pub const ALL: [FontFamily; 20] = [
        FontFamily::Proportional,
        FontFamily::Monospace,
        FontFamily::Serif,
        FontFamily::Menlo,
        FontFamily::Monaco,
        FontFamily::SfMono,
        FontFamily::Consolas,
        FontFamily::CourierNew,
        FontFamily::JetBrainsMono,
        FontFamily::FiraCode,
        FontFamily::SourceCodePro,
        FontFamily::DejaVuSansMono,
        FontFamily::UbuntuMono,
        FontFamily::Helvetica,
        FontFamily::Arial,
        FontFamily::Verdana,
        FontFamily::SegoeUi,
        FontFamily::Georgia,
        FontFamily::TimesNewRoman,
        FontFamily::DejaVuSans,
    ];

    /// The iced font for this family. Named fonts fall back to the system
    /// default if they are not installed.
    pub fn font(self) -> Font {
        match self {
            FontFamily::Proportional   => Font::DEFAULT,
            FontFamily::Monospace      => Font::MONOSPACE,
            FontFamily::Serif          => Font { family: iced::font::Family::Serif, ..Font::DEFAULT },
            FontFamily::Menlo          => Font::with_name("Menlo"),
            FontFamily::Monaco         => Font::with_name("Monaco"),
            FontFamily::SfMono         => Font::with_name("SF Mono"),
            FontFamily::Consolas       => Font::with_name("Consolas"),
            FontFamily::CourierNew     => Font::with_name("Courier New"),
            FontFamily::JetBrainsMono  => Font::with_name("JetBrains Mono"),
            FontFamily::FiraCode       => Font::with_name("Fira Code"),
            FontFamily::SourceCodePro  => Font::with_name("Source Code Pro"),
            FontFamily::DejaVuSansMono => Font::with_name("DejaVu Sans Mono"),
            FontFamily::UbuntuMono     => Font::with_name("Ubuntu Mono"),
            FontFamily::Helvetica      => Font::with_name("Helvetica"),
            FontFamily::Arial          => Font::with_name("Arial"),
            FontFamily::Verdana        => Font::with_name("Verdana"),
            FontFamily::SegoeUi        => Font::with_name("Segoe UI"),
            FontFamily::Georgia        => Font::with_name("Georgia"),
            FontFamily::TimesNewRoman  => Font::with_name("Times New Roman"),
            FontFamily::DejaVuSans     => Font::with_name("DejaVu Sans"),
        }
    }
}

impl std::fmt::Display for FontFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FontFamily::Proportional   => "Proportional (system)",
            FontFamily::Monospace      => "Monospace (system)",
            FontFamily::Serif          => "Serif (system)",
            FontFamily::Menlo          => "Menlo",
            FontFamily::Monaco         => "Monaco",
            FontFamily::SfMono         => "SF Mono",
            FontFamily::Consolas       => "Consolas",
            FontFamily::CourierNew     => "Courier New",
            FontFamily::JetBrainsMono  => "JetBrains Mono",
            FontFamily::FiraCode       => "Fira Code",
            FontFamily::SourceCodePro  => "Source Code Pro",
            FontFamily::DejaVuSansMono => "DejaVu Sans Mono",
            FontFamily::UbuntuMono     => "Ubuntu Mono",
            FontFamily::Helvetica      => "Helvetica",
            FontFamily::Arial          => "Arial",
            FontFamily::Verdana        => "Verdana",
            FontFamily::SegoeUi        => "Segoe UI",
            FontFamily::Georgia        => "Georgia",
            FontFamily::TimesNewRoman  => "Times New Roman",
            FontFamily::DejaVuSans     => "DejaVu Sans",
        })
    }
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    pub theme:         Theme,
    pub font_family:   FontFamily,
    pub font_size:     f32,
    pub show_menu_bar: bool,
    #[serde(default = "default_true")]
    pub show_breadcrumbs: bool,
    /// When true, "Copy Value" copies minified (whitespace-stripped) JSON
    /// instead of the value's original on-disk formatting.
    #[serde(default)]
    pub copy_compact_json: bool,
    /// Version string of an update the user dismissed; the banner stays hidden
    /// for this version but reappears once a newer one is published.
    #[serde(default)]
    pub dismissed_update: Option<String>,
    /// Master switch for the AI assistant (BYOK). Off by default — the AI
    /// button, panel, and any network calls to LLM providers are disabled
    /// until the user turns this on in Settings.
    #[serde(default)]
    pub ai_enabled: bool,
    #[serde(default)]
    pub ai_provider: crate::ai::provider::ProviderKind,
    /// Model name; empty = the provider's default.
    #[serde(default)]
    pub ai_model: String,
    /// Base URL override; empty = the provider's default endpoint.
    #[serde(default)]
    pub ai_base_url: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme:         Theme::Auto,
            font_family:   FontFamily::Monospace,
            font_size:     14.0,
            show_menu_bar: false,
            show_breadcrumbs: true,
            copy_compact_json: false,
            dismissed_update: None,
            ai_enabled: false,
            ai_provider: crate::ai::provider::ProviderKind::default(),
            ai_model: String::new(),
            ai_base_url: String::new(),
        }
    }
}

// ─── persistence ─────────────────────────────────────────────────────────────

/// `~/Library/Application Support/quick-json-viewer` on macOS; the XDG-ish
/// `~/.config/quick-json-viewer` elsewhere.
fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(home.join("Library").join("Application Support").join(APP_DIR))
    } else {
        Some(home.join(".config").join(APP_DIR))
    }
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.json"))
}

/// Location of the settings blob written by earlier (eframe-based) releases:
/// a RON map of key → JSON string.
fn legacy_eframe_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(home.join("Library").join("Application Support").join("Quick JSON Viewer").join("app.ron"))
    } else {
        Some(home.join(".local").join("share").join("quick json viewer").join("app.ron"))
    }
}

/// Pull our JSON string out of eframe's RON store without a RON parser: find
/// the key, then decode the following string literal (Rust-style escapes).
fn extract_legacy_json(ron: &str) -> Option<String> {
    let key_pos = ron.find(&format!("\"{STORAGE_KEY}\""))?;
    let rest = &ron[key_pos + STORAGE_KEY.len() + 2..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                'u' => {
                    // \u{XXXX}
                    if chars.next()? != '{' { return None; }
                    let mut hex = String::new();
                    for h in chars.by_ref() {
                        if h == '}' { break; }
                        hex.push(h);
                    }
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

impl Settings {
    /// Load persisted settings, falling back to the previous eframe store and
    /// finally to defaults.
    pub fn load() -> Self {
        if let Some(path) = settings_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(s) = serde_json::from_str::<Settings>(&text) {
                    return s;
                }
            }
        }
        if let Some(path) = legacy_eframe_path() {
            if let Ok(ron) = std::fs::read_to_string(&path) {
                if let Some(json) = extract_legacy_json(&ron) {
                    if let Ok(s) = serde_json::from_str::<Settings>(&json) {
                        return s;
                    }
                }
            }
        }
        Settings::default()
    }

    /// Persist to disk. Best-effort — a failure here should never take the
    /// app down.
    pub fn save(&self) {
        let Some(path) = settings_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    /// Resolve the effective light/dark choice.
    pub fn is_dark(&self, prefer_dark: bool) -> bool {
        match self.theme {
            Theme::Dark  => true,
            Theme::Light => false,
            Theme::Auto  => prefer_dark,
        }
    }

    /// Font for keys, paths, and anything that must stay monospace.
    pub fn key_font(&self) -> Font {
        Font::MONOSPACE
    }

    /// Font for values and the general UI, following the family setting.
    pub fn val_font(&self) -> Font {
        self.font_family.font()
    }

    pub fn row_height(&self) -> f32 {
        self.font_size + 8.0
    }
}

// ─── settings dialog ─────────────────────────────────────────────────────────

/// Result of the last "Set as Default JSON Viewer" attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DefaultStatus {
    #[default]
    Untried,
    Ok,
    Failed,
}

#[derive(Clone, Debug)]
pub enum SettingsMsg {
    Theme(Theme),
    FontFamily(FontFamily),
    FontSize(f32),
    ShowMenuBar(bool),
    ShowBreadcrumbs(bool),
    CopyCompact(bool),
    SetAsDefault,
    CheckForUpdates,
    Ai(crate::ai::panel::AiSettingsMsg),
    Close,
}

/// Apply a dialog message to the settings. Returns `true` when the caller
/// should request an update check.
pub fn apply(settings: &mut Settings, default_status: &mut DefaultStatus, msg: &SettingsMsg) -> bool {
    match msg {
        SettingsMsg::Theme(t)           => settings.theme = *t,
        SettingsMsg::FontFamily(f)      => settings.font_family = *f,
        SettingsMsg::FontSize(s)        => settings.font_size = s.round().clamp(10.0, 24.0),
        SettingsMsg::ShowMenuBar(b)     => settings.show_menu_bar = *b,
        SettingsMsg::ShowBreadcrumbs(b) => settings.show_breadcrumbs = *b,
        SettingsMsg::CopyCompact(b)     => settings.copy_compact_json = *b,
        SettingsMsg::SetAsDefault => {
            *default_status = if set_as_default_json_viewer() { DefaultStatus::Ok } else { DefaultStatus::Failed };
        }
        SettingsMsg::CheckForUpdates => return true,
        SettingsMsg::Ai(_) | SettingsMsg::Close => {}
    }
    false
}

const DIALOG_FONT_SIZE: f32 = 14.0;

fn heading<'a>(s: &'a str) -> Element<'a, SettingsMsg> {
    text(s).size(18).into()
}

fn label<'a>(s: &'a str) -> Element<'a, SettingsMsg> {
    text(s).size(DIALOG_FONT_SIZE).width(Length::Fixed(170.0)).into()
}

fn section_gap<'a>() -> Element<'a, SettingsMsg> {
    column![
        space().height(12),
        rule::horizontal(1).style(theme::divider_style),
        space().height(12),
    ]
    .into()
}

/// The body of the Settings dialog. Fonts are pinned to a fixed size so that
/// dragging the font-size slider doesn't move the slider under the cursor.
pub fn view<'a>(
    settings: &'a Settings,
    default_status: DefaultStatus,
    ai_ui: &'a crate::ai::panel::AiSettingsUi,
    pal: &theme::Palette,
) -> Element<'a, SettingsMsg> {
    let appearance = column![
        row![
            label("Theme"),
            pick_list(&Theme::ALL[..], Some(settings.theme), SettingsMsg::Theme)
                .width(160)
                .text_size(DIALOG_FONT_SIZE)
                .style(theme::pick_list_style),
        ]
        .align_y(iced::Center),
        row![
            label("Font style"),
            pick_list(&FontFamily::ALL[..], Some(settings.font_family), SettingsMsg::FontFamily)
                .width(200)
                .text_size(DIALOG_FONT_SIZE)
                .style(theme::pick_list_style),
        ]
        .align_y(iced::Center),
        row![
            label("Font size"),
            slider(10.0..=24.0, settings.font_size, SettingsMsg::FontSize)
                .step(1.0_f32)
                .width(160)
                .style(theme::slider_style),
            text(format!("{} px", settings.font_size as i32)).size(DIALOG_FONT_SIZE),
        ]
        .spacing(10)
        .align_y(iced::Center),
    ]
    .spacing(10);

    let layout = column![
        row![
            label("Show menu bar"),
            checkbox(settings.show_menu_bar).on_toggle(SettingsMsg::ShowMenuBar),
        ]
        .align_y(iced::Center),
        row![
            label("Show breadcrumbs"),
            checkbox(settings.show_breadcrumbs).on_toggle(SettingsMsg::ShowBreadcrumbs),
        ]
        .align_y(iced::Center),
    ]
    .spacing(10);

    let clipboard = column![
        row![
            label("Copy compressed JSON"),
            checkbox(settings.copy_compact_json).on_toggle(SettingsMsg::CopyCompact),
        ]
        .align_y(iced::Center),
        text("\"Copy Value\" copies minified JSON instead of its original formatting")
            .size(12)
            .color(pal.text_muted),
    ]
    .spacing(6);

    let ai_section: Element<'a, SettingsMsg> =
        crate::ai::panel::settings_section(settings, ai_ui, pal).map(SettingsMsg::Ai);

    let default_note: Element<'a, SettingsMsg> = match default_status {
        DefaultStatus::Ok => text("Set as default").size(DIALOG_FONT_SIZE).color(theme::OK_GREEN).into(),
        DefaultStatus::Failed => text("Failed — run from .app bundle").size(DIALOG_FONT_SIZE).color(theme::ERR_RED).into(),
        DefaultStatus::Untried => space().width(0).into(),
    };

    let system = column![
        row![
            button(text("Set as Default JSON Viewer").size(DIALOG_FONT_SIZE))
                .style(theme::button_style)
                .on_press(SettingsMsg::SetAsDefault),
            default_note,
        ]
        .spacing(10)
        .align_y(iced::Center),
        button(text("Check for Updates").size(DIALOG_FONT_SIZE))
            .style(theme::button_style)
            .on_press(SettingsMsg::CheckForUpdates),
    ]
    .spacing(8);

    let body = column![
        row![
            row![text("⚙").size(24), text("Settings").size(20)].spacing(8).align_y(iced::Center),
            space().width(Length::Fill),
            button(text("✕").size(17))
                .style(theme::flat_button_style)
                .on_press(SettingsMsg::Close),
        ]
        .align_y(iced::Center),
        space().height(8),
        heading("Appearance"),
        space().height(8),
        appearance,
        section_gap(),
        heading("Layout"),
        space().height(8),
        layout,
        section_gap(),
        heading("Clipboard"),
        space().height(8),
        clipboard,
        section_gap(),
        ai_section,
        section_gap(),
        heading("System"),
        space().height(8),
        system,
        space().height(4),
    ]
    .width(Length::Fixed(420.0));

    container(body).into()
}
