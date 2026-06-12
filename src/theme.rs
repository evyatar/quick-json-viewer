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

// ── Badges ───────────────────────────────────────────────────────────────
pub const BADGE_BG:     Color32 = Color32::from_rgb(0x1A, 0x22, 0x38); // neutral badge background (e.g. ARRAY[43])
pub const BADGE_BORDER: Color32 = Color32::from_rgb(0x2A, 0x36, 0x56);

/// Yellow highlight behind search matches (unmultiplied rgba 255,235,100,76).
pub const MATCH_BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 235, 100, 76);

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

    visuals.selection.bg_fill = SELECTION_BG;
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
