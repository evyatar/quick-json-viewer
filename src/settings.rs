use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

const STORAGE_KEY: &str = "json_viewer_settings_v1";

// 0 = untried, 1 = success, 2 = failure
static SET_DEFAULT_STATUS: AtomicU8 = AtomicU8::new(0);

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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum FontFamily {
    Proportional,
    #[default]
    Monospace,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme:         Theme::Auto,
            font_family:   FontFamily::Monospace,
            font_size:     14.0,
            show_menu_bar: false,
            show_breadcrumbs: true,
        }
    }
}

impl Settings {
    pub fn load(storage: &dyn eframe::Storage) -> Self {
        eframe::get_value(storage, STORAGE_KEY).unwrap_or_default()
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, self);
    }

    pub fn apply_fonts(&self, ctx: &egui::Context) {
        let mut style = (*ctx.global_style()).clone();
        let family = match self.font_family {
            FontFamily::Proportional => egui::FontFamily::Proportional,
            FontFamily::Monospace => egui::FontFamily::Monospace,
        };
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(self.font_size, egui::FontFamily::Monospace),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(self.font_size, family.clone()),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(self.font_size, family),
        );
        ctx.set_global_style(style);
    }

    pub fn apply_theme(&self, ctx: &egui::Context, prefer_dark: bool) {
        let visuals = match self.theme {
            Theme::Dark => crate::theme::visuals(),
            Theme::Light => egui::Visuals::light(),
            Theme::Auto => {
                if prefer_dark {
                    crate::theme::visuals()
                } else {
                    egui::Visuals::light()
                }
            }
        };
        ctx.set_visuals(visuals);
    }

    pub fn key_font(&self) -> egui::FontId {
        egui::FontId::new(self.font_size, egui::FontFamily::Monospace)
    }

    pub fn val_font(&self) -> egui::FontId {
        let family = match self.font_family {
            FontFamily::Monospace => egui::FontFamily::Monospace,
            FontFamily::Proportional => egui::FontFamily::Proportional,
        };
        egui::FontId::new(self.font_size, family)
    }

    pub fn row_height(&self) -> f32 {
        self.font_size + 8.0
    }
}

pub fn show_settings_window(
    settings: &mut Settings,
    ctx: &egui::Context,
    open: &mut bool,
) {
    egui::Window::new("⚙  Settings")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .min_width(360.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // Pin this window's fonts to a fixed size: its widgets edit the
            // global font settings, and letting those changes resize the
            // window mid-drag shifts the slider under the cursor.
            let style = ui.style_mut();
            for text_style in [egui::TextStyle::Body, egui::TextStyle::Button, egui::TextStyle::Monospace] {
                style.text_styles.insert(
                    text_style,
                    egui::FontId::new(14.0, egui::FontFamily::Monospace),
                );
            }

            ui.add_space(8.0);

            // ── Appearance ───────────────────────────────────────────────────
            ui.heading("Appearance");
            ui.add_space(8.0);

            egui::Grid::new("appearance_grid")
                .num_columns(2)
                .spacing([24.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Theme");
                    egui::ComboBox::from_id_salt("theme_combo")
                        .width(160.0)
                        .selected_text(match settings.theme {
                            Theme::Auto  => "Auto",
                            Theme::Light => "Light",
                            Theme::Dark  => "Dark",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut settings.theme, Theme::Auto,  "Auto");
                            ui.selectable_value(&mut settings.theme, Theme::Light, "Light");
                            ui.selectable_value(&mut settings.theme, Theme::Dark,  "Dark");
                        });
                    ui.end_row();

                    ui.label("Font style");
                    egui::ComboBox::from_id_salt("font_combo")
                        .width(160.0)
                        .selected_text(match settings.font_family {
                            FontFamily::Proportional => "Proportional",
                            FontFamily::Monospace    => "Monospace",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut settings.font_family, FontFamily::Proportional, "Proportional");
                            ui.selectable_value(&mut settings.font_family, FontFamily::Monospace,    "Monospace");
                        });
                    ui.end_row();

                    ui.label("Font size");
                    ui.add(
                        egui::Slider::new(&mut settings.font_size, 10.0..=24.0)
                            .step_by(1.0)
                            .suffix(" px")
                            .fixed_decimals(0),
                    );
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(12.0);

            // ── Layout ───────────────────────────────────────────────────────
            ui.heading("Layout");
            ui.add_space(8.0);

            egui::Grid::new("layout_grid")
                .num_columns(2)
                .spacing([24.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Show menu bar");
                    ui.checkbox(&mut settings.show_menu_bar, "");
                    ui.end_row();

                    ui.label("Show breadcrumbs");
                    ui.checkbox(&mut settings.show_breadcrumbs, "");
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(12.0);

            // ── System ───────────────────────────────────────────────────────
            ui.heading("System");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Set as Default JSON Viewer").clicked() {
                    let ok = set_as_default_json_viewer();
                    SET_DEFAULT_STATUS.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
                }
                match SET_DEFAULT_STATUS.load(Ordering::Relaxed) {
                    1 => { ui.colored_label(egui::Color32::from_rgb(52, 199, 89), "✓ Set as default"); }
                    2 => { ui.colored_label(egui::Color32::from_rgb(255, 69, 58), "✗ Failed — run from .app bundle"); }
                    _ => {}
                }
            });

            ui.add_space(8.0);
        });
}
