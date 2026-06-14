//! Design tokens + custom dark visuals for the "JSON Inspector" navy theme.

use egui::Color32;

// ── Backgrounds ──────────────────────────────────────────────────────────
pub const BG_APP:         Color32 = Color32::from_rgb(0x0B, 0x11, 0x20); // main tree background
pub const BG_PANEL:       Color32 = Color32::from_rgb(0x0D, 0x14, 0x24); // header bar + status bar background
pub const BG_BREADCRUMBS: Color32 = Color32::from_rgb(0x0F, 0x17, 0x29); // breadcrumbs strip background
pub const BG_SEARCH:      Color32 = Color32::from_rgb(0x16, 0x1F, 0x36); // search pill background
pub const BORDER:         Color32 = Color32::from_rgb(0x1F, 0x2A, 0x45); // subtle borders / separators
pub const INDENT_GUIDE:   Color32 = Color32::from_rgb(0x19, 0x22, 0x3C); // tree indent guide lines

// ── Text ─────────────────────────────────────────────────────────────────
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xD5, 0xDE, 0xED);
pub const TEXT_MUTED:   Color32 = Color32::from_rgb(0x70, 0x81, 0xA0);
pub const TEXT_FAINT:   Color32 = Color32::from_rgb(0x4C, 0x58, 0x74);

// ── Interaction ──────────────────────────────────────────────────────────
pub const ACCENT:       Color32 = Color32::from_rgb(0x3D, 0x7E, 0xFF); // blue accent (selection bar, JSON badge, focus)
pub const SELECTION_BG: Color32 = Color32::from_rgb(0x1A, 0x23, 0x42); // selected row fill
pub const TEXT_SELECT_BG: Color32 = Color32::from_rgb(0x2E, 0x5C, 0xB8); // selected text in text fields
pub const HOVER_BG:     Color32 = Color32::from_rgb(0x12, 0x1A, 0x2F); // hovered row fill

// ── JSON syntax (classic VS Code-style palette — easy type distinction) ─────
pub const KEY:         Color32 = Color32::from_rgb(156, 220, 254); // object key names (cyan)
pub const ARRAY_INDEX: Color32 = Color32::from_rgb(150, 200, 150); // array index numbers (green)
pub const PUNCT:       Color32 = Color32::from_rgb(0x5A, 0x67, 0x83); // colons / separators
pub const CONTAINER:   Color32 = Color32::from_rgb(0x5A, 0x67, 0x83); // "{ 3 }" / "[ 43 ]" child-count text
pub const NUMBER:      Color32 = Color32::from_rgb(181, 206, 168); // light green
pub const STRING:      Color32 = Color32::from_rgb(206, 145, 120); // tan
pub const BOOL:        Color32 = Color32::from_rgb(86, 156, 214);  // blue
pub const NULL:        Color32 = Color32::from_rgb(160, 160, 160); // gray

/// Yellow highlight behind search matches (unmultiplied rgba 255,235,100,76).
pub const MATCH_BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 235, 100, 76);

// ── Diff row tints (low-alpha so they layer over the row background) ─────────
pub const DIFF_ADDED_BG:   Color32 = Color32::from_rgba_unmultiplied_const(0x3F, 0xB9, 0x50, 60); // green
pub const DIFF_REMOVED_BG: Color32 = Color32::from_rgba_unmultiplied_const(0xE5, 0x53, 0x4B, 60); // red
pub const DIFF_CHANGED_BG: Color32 = Color32::from_rgba_unmultiplied_const(0xE3, 0xB3, 0x41, 55); // amber
pub const DIFF_EMPTY_BG:   Color32 = Color32::from_rgba_unmultiplied_const(0x80, 0x80, 0x80, 22); // gap cell

// ── Runtime chrome palette ──────────────────────────────────────────────────
// The tree/diff *content* colors above are chosen inline per row (see
// `key_parts` / `value_parts`). The surrounding chrome — panels, headers,
// breadcrumbs, status bar, dividers, tabs — pulls its colors from this palette
// so it tracks the active light/dark theme instead of staying navy in light
// mode.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg_panel:        Color32,
    pub bg_breadcrumbs:  Color32,
    pub bg_search:       Color32,
    pub border:          Color32,
    pub text_primary:    Color32,
    pub text_muted:      Color32,
    pub text_faint:      Color32,
    pub accent:          Color32,
    pub key:             Color32,
    pub selection_bg:    Color32,
    pub hover_bg:        Color32,
    /// Active tab / toggle pill — a filled background with high-contrast text.
    pub tab_active_bg:   Color32,
    pub tab_active_fg:   Color32,
    pub tab_inactive_fg: Color32,
}

impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        if dark { Self::DARK } else { Self::LIGHT }
    }

    const DARK: Palette = Palette {
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
        tab_active_bg:   Color32::from_rgb(0x1C, 0x27, 0x42),
        tab_active_fg:   ACCENT,
        tab_inactive_fg: TEXT_MUTED,
    };

    const LIGHT: Palette = Palette {
        bg_panel:        Color32::from_rgb(0xEC, 0xEF, 0xF4),
        bg_breadcrumbs:  Color32::from_rgb(0xF0, 0xF3, 0xF8),
        bg_search:       Color32::from_rgb(0xFF, 0xFF, 0xFF),
        border:          Color32::from_rgb(0xD3, 0xD9, 0xE3),
        text_primary:    Color32::from_rgb(0x1A, 0x22, 0x30),
        text_muted:      Color32::from_rgb(0x5A, 0x6B, 0x85),
        text_faint:      Color32::from_rgb(0x8A, 0x97, 0xAD),
        accent:          ACCENT,
        key:             Color32::from_rgb(0x00, 0x5A, 0x9E),
        selection_bg:    Color32::from_rgb(0xDC, 0xE7, 0xF7),
        hover_bg:        Color32::from_rgb(0xEA, 0xED, 0xF2),
        tab_active_bg:   Color32::from_rgb(0xDA, 0xE6, 0xF9),
        tab_active_fg:   Color32::from_rgb(0x0A, 0x4F, 0xB8),
        tab_inactive_fg: Color32::from_rgb(0x5A, 0x6B, 0x85),
    };
}

/// Custom dark navy visuals for the whole chrome.
pub fn visuals() -> egui::Visuals {
    use egui::{CornerRadius, Stroke};

    // Slightly lighter navy for hovered widget backgrounds.
    let hovered_widget_bg = Color32::from_rgb(0x1C, 0x27, 0x42);
    let corner_radius = CornerRadius::same(6);

    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill      = BG_APP;
    visuals.window_fill     = BG_APP;
    visuals.extreme_bg_color = BG_SEARCH; // TextEdit background
    visuals.faint_bg_color   = HOVER_BG;

    visuals.selection.bg_fill = TEXT_SELECT_BG;
    visuals.selection.stroke  = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.hyperlink_color   = ACCENT;

    visuals.widgets.noninteractive.bg_stroke       = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke.color = TEXT_PRIMARY;

    visuals.widgets.inactive.bg_fill         = BG_SEARCH;
    visuals.widgets.inactive.weak_bg_fill    = BG_SEARCH;
    visuals.widgets.inactive.fg_stroke.color = TEXT_MUTED;
    visuals.widgets.inactive.corner_radius   = corner_radius;

    visuals.widgets.hovered.bg_fill         = hovered_widget_bg;
    visuals.widgets.hovered.weak_bg_fill    = hovered_widget_bg;
    visuals.widgets.hovered.fg_stroke.color = TEXT_PRIMARY;
    visuals.widgets.hovered.corner_radius   = corner_radius;

    visuals.widgets.active.bg_fill         = SELECTION_BG;
    visuals.widgets.active.fg_stroke.color = TEXT_PRIMARY;
    visuals.widgets.active.corner_radius   = corner_radius;

    visuals.widgets.open.bg_fill         = BG_SEARCH;
    visuals.widgets.open.fg_stroke.color = TEXT_PRIMARY;
    visuals.widgets.open.corner_radius   = corner_radius;

    visuals
}
