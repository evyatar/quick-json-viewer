//! Design tokens + custom navy theme for the "JSON Inspector" look, plus the
//! iced style functions shared by the whole chrome.

use iced::widget::{button, container, pick_list, rule, scrollable, slider, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

// ── Backgrounds ──────────────────────────────────────────────────────────
pub const BG_APP:         Color = rgb(0x0B, 0x11, 0x20); // main tree background
pub const BG_PANEL:       Color = rgb(0x0D, 0x14, 0x24); // header bar + status bar background
pub const BG_BREADCRUMBS: Color = rgb(0x0F, 0x17, 0x29); // breadcrumbs strip background
pub const BG_SEARCH:      Color = rgb(0x16, 0x1F, 0x36); // search pill background
pub const BORDER:         Color = rgb(0x1F, 0x2A, 0x45); // subtle borders / separators
pub const INDENT_GUIDE:   Color = rgb(0x19, 0x22, 0x3C); // tree indent guide lines

// ── Text ─────────────────────────────────────────────────────────────────
pub const TEXT_PRIMARY: Color = rgb(0xD5, 0xDE, 0xED);
pub const TEXT_MUTED:   Color = rgb(0x70, 0x81, 0xA0);
pub const TEXT_FAINT:   Color = rgb(0x4C, 0x58, 0x74);

// ── Interaction ──────────────────────────────────────────────────────────
pub const ACCENT:         Color = rgb(0x3D, 0x7E, 0xFF); // blue accent (selection bar, JSON badge, focus)
pub const SELECTION_BG:   Color = rgb(0x1A, 0x23, 0x42); // selected row fill
pub const TEXT_SELECT_BG: Color = rgb(0x2E, 0x5C, 0xB8); // selected text in text fields
pub const HOVER_BG:       Color = rgb(0x12, 0x1A, 0x2F); // hovered row fill

// ── JSON syntax (classic VS Code-style palette — easy type distinction) ─────
pub const KEY:         Color = rgb(156, 220, 254); // object key names (cyan)
pub const ARRAY_INDEX: Color = rgb(150, 200, 150); // array index numbers (green)
pub const PUNCT:       Color = rgb(0x5A, 0x67, 0x83); // colons / separators
pub const CONTAINER:   Color = rgb(0x5A, 0x67, 0x83); // "{ 3 }" / "[ 43 ]" child-count text
pub const NUMBER:      Color = rgb(181, 206, 168); // light green
pub const STRING:      Color = rgb(206, 145, 120); // tan
pub const BOOL:        Color = rgb(86, 156, 214);  // blue
pub const NULL:        Color = rgb(160, 160, 160); // gray

/// Yellow highlight behind search matches (20% opacity).
pub const MATCH_BG: Color = rgba(255, 235, 100, 15);
/// Light-theme variant of the search-match highlight (20% opacity).
pub const MATCH_BG_LIGHT: Color = rgba(255, 200, 0, 15);

/// Color for nodes marked as deleted (pending deletion).
pub const DELETED: Color = rgb(0xE5, 0x53, 0x4B);

/// Color for array items added interactively and not yet saved.
pub const NEW: Color = rgb(0x3F, 0xB9, 0x50);

/// Color for values changed between compared files (amber).
pub const CHANGED: Color = rgb(0xE3, 0xB3, 0x41);

/// Update-available orange / "up to date" green used in the About dialog.
pub const WARN_ORANGE: Color = rgb(255, 159, 10);
pub const OK_GREEN:    Color = rgb(52, 199, 89);
pub const ERR_RED:     Color = rgb(255, 69, 58);

// ── Diff row tints (low-alpha so they layer over the row background) ─────────
pub const DIFF_ADDED_BG:   Color = rgba(0x3F, 0xB9, 0x50, 60); // green
pub const DIFF_REMOVED_BG: Color = rgba(0xE5, 0x53, 0x4B, 60); // red
pub const DIFF_CHANGED_BG: Color = rgba(0xE3, 0xB3, 0x41, 55); // amber
pub const DIFF_EMPTY_BG:   Color = rgba(0x80, 0x80, 0x80, 22); // gap cell
pub const DIFF_SELECTION_OVERLAY: Color = rgba(0x3D, 0x7E, 0xFF, 36);

// ── Light-theme content colors ───────────────────────────────────────────────
pub const LIGHT_KEY:         Color = rgb(0, 90, 158);
pub const LIGHT_ARRAY_INDEX: Color = rgb(40, 120, 40);
pub const LIGHT_STRING:      Color = rgb(163, 21, 21);
pub const LIGHT_NUMBER:      Color = rgb(9, 134, 88);
pub const LIGHT_BOOL:        Color = rgb(0, 0, 210);
pub const LIGHT_CONTAINER:   Color = rgb(100, 100, 100);
pub const LIGHT_PUNCT:       Color = rgb(120, 120, 120);
pub const LIGHT_SELECTION_BG: Color = rgb(0xB8, 0xD4, 0xFF);

// ── Runtime chrome palette ──────────────────────────────────────────────────
// The tree/diff *content* colors above are chosen inline per row. The
// surrounding chrome — panels, headers, breadcrumbs, status bar, dividers,
// tabs — pulls its colors from this palette so it tracks the active
// light/dark theme instead of staying navy in light mode.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Palette {
    pub dark:            bool,
    pub bg_app:          Color,
    pub bg_panel:        Color,
    pub bg_breadcrumbs:  Color,
    pub bg_search:       Color,
    pub border:          Color,
    pub text_primary:    Color,
    pub text_muted:      Color,
    pub text_faint:      Color,
    pub accent:          Color,
    pub key:             Color,
    pub selection_bg:    Color,
    pub hover_bg:        Color,
    /// Active tab / toggle pill — a filled background with high-contrast text.
    pub tab_active_bg:   Color,
    pub tab_active_fg:   Color,
    pub tab_inactive_fg: Color,
    /// Generic widget (button / input) background and its hovered variant.
    pub widget_bg:       Color,
    pub widget_hover_bg: Color,
    /// Alternating-row tint for zebra-striped tables (help dialogs).
    pub stripe_bg:       Color,
}

impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        if dark { Self::DARK } else { Self::LIGHT }
    }

    /// Palette matching the active iced theme.
    pub fn of(theme: &Theme) -> Self {
        Self::for_dark(theme.extended_palette().is_dark)
    }

    const DARK: Palette = Palette {
        dark:            true,
        bg_app:          BG_APP,
        bg_panel:        BG_PANEL,
        bg_breadcrumbs:  BG_BREADCRUMBS,
        bg_search:       BG_SEARCH,
        border:          BORDER,
        text_primary:    TEXT_PRIMARY,
        text_muted:      TEXT_MUTED,
        text_faint:      TEXT_FAINT,
        accent:          ACCENT,
        key:             KEY,
        selection_bg:    SELECTION_BG,
        hover_bg:        HOVER_BG,
        tab_active_bg:   rgb(0x1C, 0x27, 0x42),
        tab_active_fg:   ACCENT,
        tab_inactive_fg: TEXT_MUTED,
        widget_bg:       BG_SEARCH,
        widget_hover_bg: rgb(0x1C, 0x27, 0x42),
        stripe_bg:       rgba(0xFF, 0xFF, 0xFF, 1),
    };

    const LIGHT: Palette = Palette {
        dark:            false,
        bg_app:          rgb(0xFF, 0xFF, 0xFF),
        bg_panel:        rgb(0xEC, 0xEF, 0xF4),
        bg_breadcrumbs:  rgb(0xF0, 0xF3, 0xF8),
        bg_search:       rgb(0xFF, 0xFF, 0xFF),
        border:          rgb(0xD3, 0xD9, 0xE3),
        text_primary:    rgb(0x1A, 0x22, 0x30),
        text_muted:      rgb(0x5A, 0x6B, 0x85),
        text_faint:      rgb(0x8A, 0x97, 0xAD),
        accent:          ACCENT,
        key:             rgb(0x00, 0x5A, 0x9E),
        selection_bg:    rgb(0xDC, 0xE7, 0xF7),
        hover_bg:        rgb(0xEA, 0xED, 0xF2),
        tab_active_bg:   rgb(0xDA, 0xE6, 0xF9),
        tab_active_fg:   rgb(0x0A, 0x4F, 0xB8),
        tab_inactive_fg: rgb(0x5A, 0x6B, 0x85),
        widget_bg:       rgb(0xF4, 0xF6, 0xFA),
        widget_hover_bg: rgb(0xE4, 0xE9, 0xF2),
        stripe_bg:       rgba(0x00, 0x00, 0x00, 12),
    };
}

/// The iced theme for the chosen light/dark mode.
pub fn iced_theme(dark: bool) -> Theme {
    if dark {
        Theme::custom(
            "JSON Inspector Dark",
            iced::theme::Palette {
                background: BG_APP,
                text:       TEXT_PRIMARY,
                primary:    ACCENT,
                success:    NEW,
                warning:    CHANGED,
                danger:     DELETED,
            },
        )
    } else {
        Theme::custom(
            "JSON Inspector Light",
            iced::theme::Palette {
                background: Color::WHITE,
                text:       Palette::LIGHT.text_primary,
                primary:    ACCENT,
                success:    OK_GREEN,
                warning:    WARN_ORANGE,
                danger:     ERR_RED,
            },
        )
    }
}

// ─── style functions ─────────────────────────────────────────────────────────

const RADIUS: f32 = 6.0;

fn no_shadow() -> Shadow {
    Shadow::default()
}

/// Standard chrome button: subtle filled background, 1px border.
pub fn button_style(theme: &Theme, status: button::Status) -> button::Style {
    let pal = Palette::of(theme);
    let (bg, fg) = match status {
        button::Status::Active   => (pal.widget_bg, pal.text_muted),
        button::Status::Hovered  => (pal.widget_hover_bg, pal.text_primary),
        button::Status::Pressed  => (pal.selection_bg, pal.text_primary),
        button::Status::Disabled => (pal.widget_bg, pal.text_faint),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: fg,
        border: Border { color: pal.border, width: 1.0, radius: RADIUS.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Accent-filled call-to-action button (Save, Apply, Export).
pub fn accent_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let pal = Palette::of(theme);
    let bg = match status {
        button::Status::Active   => pal.accent,
        button::Status::Hovered  => rgb(0x5A, 0x92, 0xFF),
        button::Status::Pressed  => rgb(0x2F, 0x69, 0xDB),
        button::Status::Disabled => pal.accent.scale_alpha(0.4),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: RADIUS.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Frameless text-only button (menu items, badges, breadcrumb segments).
pub fn flat_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let pal = Palette::of(theme);
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(pal.widget_hover_bg)),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: match status {
            button::Status::Disabled => pal.text_faint,
            _ => pal.text_primary,
        },
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 4.0.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// A tab / toggle rendered as a pill. When `active` it gets a filled
/// background with high-contrast text; when inactive it is plain muted text.
pub fn tab_button_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let pal = Palette::of(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (bg, fg, border) = if active {
            (Some(Background::Color(pal.tab_active_bg)), pal.tab_active_fg, pal.tab_active_fg)
        } else if hovered {
            (Some(Background::Color(pal.widget_hover_bg)), pal.text_primary, pal.border)
        } else {
            (None, pal.tab_inactive_fg, pal.border)
        };
        button::Style {
            background: bg,
            text_color: fg,
            border: Border { color: border, width: 1.0, radius: RADIUS.into() },
            shadow: no_shadow(),
            snap: true,
        }
    }
}

/// Menu-item button: full-width, highlighted on hover with the accent.
pub fn menu_item_style(theme: &Theme, status: button::Status) -> button::Style {
    let pal = Palette::of(theme);
    let (bg, fg) = match status {
        button::Status::Hovered | button::Status::Pressed => (Some(Background::Color(pal.accent)), Color::WHITE),
        button::Status::Disabled => (None, pal.text_faint),
        button::Status::Active => (None, pal.text_primary),
    };
    button::Style {
        background: bg,
        text_color: fg,
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 4.0.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Panel (toolbar / status bar) background.
pub fn panel_style(theme: &Theme) -> container::Style {
    let pal = Palette::of(theme);
    container::Style {
        text_color: Some(pal.text_primary),
        background: Some(Background::Color(pal.bg_panel)),
        border: Border::default(),
        shadow: no_shadow(),
        snap: true,
    }
}

/// Breadcrumbs / compare-options strip background.
pub fn strip_style(theme: &Theme) -> container::Style {
    let pal = Palette::of(theme);
    container::Style {
        text_color: Some(pal.text_primary),
        background: Some(Background::Color(pal.bg_breadcrumbs)),
        border: Border::default(),
        shadow: no_shadow(),
        snap: true,
    }
}

/// Thin border line — used as the 1px divider between panels.
pub fn divider_style(theme: &Theme) -> rule::Style {
    rule::Style {
        color: Palette::of(theme).border,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }
}

/// Floating card: modal dialogs, context menus, dropdown menus.
pub fn card_style(theme: &Theme) -> container::Style {
    let pal = Palette::of(theme);
    container::Style {
        text_color: Some(pal.text_primary),
        background: Some(Background::Color(pal.bg_panel)),
        border: Border { color: pal.border, width: 1.0, radius: 8.0.into() },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 18.0,
        },
        snap: true,
    }
}

/// Translucent backdrop behind modal dialogs.
pub fn backdrop_style(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: None,
        background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))),
        border: Border::default(),
        shadow: no_shadow(),
        snap: true,
    }
}

/// Rounded search-pill / code-block / keycap frame.
pub fn pill_style(theme: &Theme) -> container::Style {
    let pal = Palette::of(theme);
    container::Style {
        text_color: Some(pal.text_primary),
        background: Some(Background::Color(pal.bg_search)),
        border: Border { color: pal.border, width: 1.0, radius: 8.0.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Keycap-styled chip for a keyboard shortcut.
pub fn keycap_style(theme: &Theme) -> container::Style {
    let pal = Palette::of(theme);
    container::Style {
        text_color: Some(pal.text_primary),
        background: Some(Background::Color(pal.bg_search)),
        border: Border { color: pal.border, width: 1.0, radius: 4.0.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Solid fill with a given color, no border.
pub fn fill_style(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        text_color: None,
        background: Some(Background::Color(color)),
        border: Border::default(),
        shadow: no_shadow(),
        snap: true,
    }
}

/// Rounded box filled with `color` — chat bubbles, highlighted sections.
pub fn bubble_style(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        text_color: None,
        background: Some(Background::Color(color)),
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 6.0.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Rounded box with only an outline in `color`.
pub fn outline_style(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        text_color: None,
        background: None,
        border: Border { color, width: 1.0, radius: 6.0.into() },
        shadow: no_shadow(),
        snap: true,
    }
}

/// Text field inside the toolbar search pill — frameless, transparent.
pub fn search_input_style(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    let pal = Palette::of(theme);
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: pal.text_muted,
        placeholder: pal.text_faint,
        value: pal.text_primary,
        selection: if pal.dark { TEXT_SELECT_BG } else { LIGHT_SELECTION_BG },
    }
}

/// Regular framed text field.
pub fn input_style(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let pal = Palette::of(theme);
    let border_color = match status {
        text_input::Status::Focused { .. } => pal.accent,
        text_input::Status::Hovered => pal.text_muted,
        _ => pal.border,
    };
    text_input::Style {
        background: Background::Color(pal.bg_search),
        border: Border { color: border_color, width: 1.0, radius: RADIUS.into() },
        icon: pal.text_muted,
        placeholder: pal.text_faint,
        value: pal.text_primary,
        selection: if pal.dark { TEXT_SELECT_BG } else { LIGHT_SELECTION_BG },
    }
}

pub fn pick_list_style(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let pal = Palette::of(theme);
    let border_color = match status {
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => pal.text_muted,
        _ => pal.border,
    };
    pick_list::Style {
        text_color: pal.text_primary,
        placeholder_color: pal.text_faint,
        handle_color: pal.text_muted,
        background: Background::Color(pal.bg_search),
        border: Border { color: border_color, width: 1.0, radius: RADIUS.into() },
    }
}

pub fn slider_style(theme: &Theme, _status: slider::Status) -> slider::Style {
    let pal = Palette::of(theme);
    slider::Style {
        rail: slider::Rail {
            backgrounds: (Background::Color(pal.accent), Background::Color(pal.border)),
            width: 4.0,
            border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 2.0.into() },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 7.0 },
            background: Background::Color(pal.accent),
            border_width: 1.0,
            border_color: pal.bg_panel,
        },
    }
}

/// Slim, unobtrusive scrollbars matching the chrome.
pub fn scrollable_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let pal = Palette::of(theme);
    let hovered = !matches!(status, scrollable::Status::Active { .. });
    let scroller_color = if hovered { pal.text_muted } else { pal.text_faint.scale_alpha(0.7) };
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(scroller_color),
            border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 4.0.into() },
        },
    };
    scrollable::Style {
        container: container::Style {
            text_color: None,
            background: None,
            border: Border::default(),
            shadow: no_shadow(),
            snap: true,
        },
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(pal.bg_panel),
            border: Border { color: pal.border, width: 1.0, radius: 20.0.into() },
            shadow: no_shadow(),
            icon: pal.text_primary,
        },
    }
}
