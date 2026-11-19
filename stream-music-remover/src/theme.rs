//! Look and feel: colours, Windows fonts (Segoe UI + Segoe icon font) and the custom widgets
//! (cards, buttons, choice cards, progress bars) used by the app.

use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Stroke};
use std::sync::Arc;

// ----- colours -----
pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);
pub const SURFACE_0: Color32 = Color32::from_rgb(0x0e, 0x10, 0x13);
pub const SURFACE_1: Color32 = Color32::from_rgb(0x1a, 0x1d, 0x23);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x22, 0x26, 0x2d);
pub const SURFACE_3: Color32 = Color32::from_rgb(0x2d, 0x32, 0x3b);
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x38);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x4a, 0x51, 0x5d);
pub const TEXT: Color32 = Color32::from_rgb(0xec, 0xee, 0xf2);
pub const MUTED: Color32 = Color32::from_rgb(0xa4, 0xab, 0xb6);
pub const ACCENT: Color32 = Color32::from_rgb(0x5b, 0x8c, 0xff);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x74, 0x9e, 0xff);
pub const ACCENT_TINT: Color32 = Color32::from_rgb(0x1d, 0x27, 0x3d);
pub const GOOD: Color32 = Color32::from_rgb(0x52, 0xcc, 0x8c);
pub const WARN: Color32 = Color32::from_rgb(0xf2, 0xb8, 0x4b);
pub const BAD: Color32 = Color32::from_rgb(0xf0, 0x6a, 0x6a);
pub const BAD_TEXT: Color32 = Color32::from_rgb(0xff, 0x9a, 0x9a);

// ----- icons (Segoe Fluent Icons / Segoe MDL2 Assets code points) -----
pub const ICON_SETTINGS: &str = "\u{E713}";
pub const ICON_ADD: &str = "\u{E710}";
pub const ICON_DROP: &str = "\u{E896}";
pub const ICON_FOLDER: &str = "\u{E838}";
pub const ICON_VIDEO: &str = "\u{E714}";
pub const ICON_AUDIO: &str = "\u{E8D6}";
pub const ICON_MIC: &str = "\u{E720}";
pub const ICON_CLOSE: &str = "\u{E711}";
pub const ICON_CHECK: &str = "\u{E73E}";
pub const ICON_RETRY: &str = "\u{E72C}";
pub const ICON_ERROR: &str = "\u{E783}";
pub const ICON_INFO: &str = "\u{E946}";
pub const ICON_STOP: &str = "\u{E71A}";
pub const ICON_PLAY: &str = "\u{E768}";
pub const ICON_HEADPHONES: &str = "\u{E7F6}";
pub const ICON_FILE: &str = "\u{E8A5}";
pub const ICON_CHEVRON_RIGHT: &str = "\u{E76C}";
pub const ICON_CHEVRON_DOWN: &str = "\u{E70D}";

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let font_dir = std::path::Path::new(&windir).join("Fonts");
    let load = |names: &[&str]| names.iter().find_map(|n| std::fs::read(font_dir.join(n)).ok());

    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut proportional = defaults.clone();
    let mut semibold = defaults.clone();
    let mut icons = defaults.clone();
    if let Some(data) = load(&["segoeui.ttf"]) {
        fonts.font_data.insert("segoe".into(), Arc::new(egui::FontData::from_owned(data)));
        proportional.insert(0, "segoe".into());
        semibold.insert(0, "segoe".into());
    }
    if let Some(data) = load(&["seguisb.ttf"]) {
        fonts.font_data.insert("segoe_sb".into(), Arc::new(egui::FontData::from_owned(data)));
        semibold.insert(0, "segoe_sb".into());
    }
    if let Some(data) = load(&["SegoeIcons.ttf", "segmdl2.ttf"]) {
        fonts.font_data.insert("icons".into(), Arc::new(egui::FontData::from_owned(data)));
        icons.insert(0, "icons".into());
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Name("semibold".into()), semibold);
    fonts.families.insert(FontFamily::Name("icons".into()), icons);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.global_style()).clone();
    use egui::TextStyle::*;
    style.text_styles = [
        (Heading, FontId::new(20.0, FontFamily::Name("semibold".into()))),
        (Body, FontId::proportional(14.5)),
        (Button, FontId::proportional(14.5)),
        (Small, FontId::proportional(12.5)),
        (Monospace, FontId::monospace(13.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.icon_width = 18.0;
    style.spacing.icon_width_inner = 10.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE_1;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.extreme_bg_color = SURFACE_0;
    v.faint_bg_color = SURFACE_2;
    v.override_text_color = None;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.error_fg_color = BAD;
    v.warn_fg_color = WARN;
    v.disabled_alpha = 0.45;
    let r = egui::CornerRadius::same(6);
    for (w, fill, stroke) in [
        (&mut v.widgets.noninteractive, SURFACE_1, BORDER),
        (&mut v.widgets.inactive, SURFACE_3, BORDER_STRONG),
        (&mut v.widgets.hovered, Color32::from_rgb(0x38, 0x3e, 0x49), MUTED),
        (&mut v.widgets.active, Color32::from_rgb(0x42, 0x49, 0x55), TEXT),
        (&mut v.widgets.open, SURFACE_3, BORDER_STRONG),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, stroke);
        w.corner_radius = r;
        w.fg_stroke = Stroke::new(1.0, TEXT);
    }
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    ctx.set_global_style(style);
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

pub fn icon(glyph: &str, size: f32, color: Color32) -> RichText {
    RichText::new(glyph).font(icon_font(size)).color(color)
}

fn label_job(glyph: Option<&str>, text: &str, size: f32, color: Color32, bold: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if let Some(g) = glyph {
        job.append(g, 0.0, egui::TextFormat { font_id: icon_font(size - 0.5), color, valign: egui::Align::Center, ..Default::default() });
        job.append("  ", 0.0, egui::TextFormat { font_id: FontId::proportional(size * 0.6), color, ..Default::default() });
    }
    let font = if bold { semibold(size) } else { FontId::proportional(size) };
    job.append(text, 0.0, egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() });
    job
}

/// A section card with a subtle border.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE_1)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(semibold(19.0)).color(TEXT));
}

/// A full-width row with a title, a muted description and a chevron; toggles `open` when clicked.
pub fn expander_row(ui: &mut egui::Ui, title: &str, desc: &str, open: &mut bool) -> bool {
    let resp = egui::Frame::new()
        .fill(SURFACE_2)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(title).font(semibold(14.5)).color(TEXT));
                    ui.label(RichText::new(desc).size(13.0).color(MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(icon(if *open { ICON_CHEVRON_DOWN } else { ICON_CHEVRON_RIGHT }, 12.0, MUTED));
                });
            });
        })
        .response;
    let id = ui.make_persistent_id(("expander", title));
    let click = ui.interact(resp.rect, id, egui::Sense::click());
    let (t, o) = (title.to_string(), *open);
    click.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, o, t.clone()));
    if click.hovered() {
        ui.painter().rect_stroke(resp.rect, 8.0, Stroke::new(1.0, BORDER_STRONG), egui::StrokeKind::Inside);
    }
    if click.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        *open = !*open;
    }
    if *open {
        ui.add_space(6.0);
    }
    *open
}

/// Section title with an optional "locked while processing" hint on the right.
pub fn section_header(ui: &mut egui::Ui, text: &str, locked: bool) {
    ui.horizontal(|ui| {
        section_title(ui, text);
        if locked {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("Can't be changed while processing").size(13.0).color(MUTED));
                ui.label(icon("\u{E72E}", 12.0, MUTED)); // lock
            });
        }
    });
}

pub fn primary_button(ui: &mut egui::Ui, glyph: Option<&str>, text: &str, enabled: bool) -> egui::Response {
    let (fill, fg) = if enabled { (ACCENT, Color32::WHITE) } else { (SURFACE_3, MUTED) };
    let b = egui::Button::new(label_job(glyph, text, 14.5, fg, true)).fill(fill).stroke(Stroke::NONE).corner_radius(8.0).min_size(egui::vec2(0.0, 34.0));
    let r = ui.add_enabled(enabled, b);
    if enabled && r.hovered() {
        ui.painter().rect_stroke(r.rect, 8.0, Stroke::new(1.0, ACCENT_HOVER), egui::StrokeKind::Inside);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn big_primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let (fill, fg) = if enabled { (ACCENT, Color32::WHITE) } else { (SURFACE_3, MUTED) };
    let b = egui::Button::new(RichText::new(text).font(semibold(16.0)).color(fg)).fill(fill).stroke(Stroke::new(1.0, if enabled { ACCENT } else { BORDER_STRONG })).corner_radius(10.0).min_size(egui::vec2(220.0, 46.0));
    // Keep the disabled state legible (grey, not faded to near-invisible).
    let r = ui.scope(|ui| {
        ui.visuals_mut().disabled_alpha = 0.9;
        ui.add_enabled(enabled, b)
    }).inner;
    if enabled && r.hovered() {
        ui.painter().rect_stroke(r.rect, 10.0, Stroke::new(1.5, ACCENT_HOVER), egui::StrokeKind::Inside);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn secondary_button(ui: &mut egui::Ui, glyph: Option<&str>, text: &str, enabled: bool) -> egui::Response {
    let b = egui::Button::new(label_job(glyph, text, 14.0, TEXT, false)).fill(SURFACE_3).stroke(Stroke::new(1.0, BORDER_STRONG)).corner_radius(8.0).min_size(egui::vec2(0.0, 32.0));
    ui.add_enabled(enabled, b).on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).font(semibold(14.5)).color(Color32::WHITE)).fill(Color32::from_rgb(0xc8, 0x45, 0x45)).corner_radius(8.0).min_size(egui::vec2(90.0, 34.0));
    ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn link_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).size(13.5).color(ACCENT)).frame(false);
    let r = ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand);
    if r.hovered() {
        let y = r.rect.bottom() - 4.0;
        ui.painter().hline(r.rect.x_range().shrink(6.0), y, Stroke::new(1.0, ACCENT));
    }
    r
}

/// A small frameless icon button; `tooltip` doubles as its accessible name.
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, tooltip: &str) -> egui::Response {
    let b = egui::Button::new(icon(glyph, 13.0, MUTED)).frame(false).min_size(egui::vec2(30.0, 30.0));
    let r = ui.add(b);
    let label = tooltip.to_string();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    if r.hovered() {
        ui.painter().rect_filled(r.rect, 6.0, Color32::from_white_alpha(14));
        ui.painter().text(r.rect.center(), egui::Align2::CENTER_CENTER, glyph, icon_font(13.0), TEXT);
    }
    r.on_hover_text(tooltip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(13.0), fg);
    let size = galley.size() + egui::vec2(20.0, 6.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, bg);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
}

pub fn progress_bar(ui: &mut egui::Ui, frac: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 8.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, SURFACE_3);
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        let fill = egui::Rect::from_min_size(rect.min, egui::vec2((rect.width() * f).max(8.0), rect.height()));
        ui.painter().rect_filled(fill, 4.0, ACCENT);
    }
}

/// A rounded rectangle outline drawn with dashes (for drop targets).
pub fn dashed_rect(painter: &egui::Painter, rect: egui::Rect, stroke: Stroke, radius: f32) {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let mut pts = Vec::new();
    let corners = [
        (egui::pos2(rect.right() - r, rect.top() + r), -90.0f32),
        (egui::pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (egui::pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (egui::pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    pts.push(egui::pos2(rect.left() + r, rect.top()));
    for (c, start) in corners {
        for k in 0..=8 {
            let a = (start + 90.0 * k as f32 / 8.0).to_radians();
            pts.push(c + egui::vec2(a.cos(), a.sin()) * r);
        }
    }
    pts.push(egui::pos2(rect.left() + r, rect.top()));
    painter.extend(egui::Shape::dashed_line(&pts, stroke, 6.0, 4.0));
}

/// A large selectable card acting as a radio button.
pub fn choice_card(ui: &mut egui::Ui, width: f32, selected: bool, glyph: &str, title: &str, desc: &str, badge: Option<&str>) -> egui::Response {
    let inner = egui::Frame::new()
        .fill(if selected { ACCENT_TINT } else { SURFACE_2 })
        .stroke(Stroke::new(if selected { 1.5 } else { 1.0 }, if selected { ACCENT } else { BORDER }))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(width - 26.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    // radio dot
                    let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    ui.painter().circle_stroke(r.center(), 8.0, Stroke::new(1.5, if selected { ACCENT } else { BORDER_STRONG }));
                    if selected {
                        ui.painter().circle_filled(r.center(), 4.5, ACCENT);
                    }
                    ui.label(icon(glyph, 15.0, if selected { ACCENT } else { MUTED }));
                    ui.label(RichText::new(title).font(semibold(15.0)).color(TEXT));
                    if let Some(b) = badge {
                        let k = if ui.is_enabled() { 1.0 } else { 0.45 };
                        pill(ui, b, GOOD.gamma_multiply(k), GOOD.gamma_multiply(0.16 * k));
                    }
                });
                egui::Frame::new().inner_margin(egui::Margin { left: 26, ..Default::default() }).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(desc).size(13.0).color(MUTED)).wrap());
                });
            });
        });
    let id = ui.make_persistent_id(("choice", title));
    let resp = ui.interact(inner.response.rect, id, egui::Sense::click());
    let t = title.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, t.clone()));
    if resp.hovered() && !selected {
        ui.painter().rect_stroke(inner.response.rect, 10.0, Stroke::new(1.0, MUTED), egui::StrokeKind::Inside);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Draws an 18px checkbox: accent-filled with a white tick when on, an outlined box when off.
fn paint_check(ui: &egui::Ui, rect: egui::Rect, on: bool, hovered: bool) {
    let b = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0));
    if on {
        ui.painter().rect_filled(b, 4.0, if hovered { ACCENT_HOVER } else { ACCENT });
        let p = |x: f32, y: f32| egui::pos2(b.left() + x, b.top() + y);
        ui.painter().line(vec![p(4.5, 9.5), p(7.5, 12.5), p(13.5, 6.0)], Stroke::new(2.0, Color32::WHITE));
    } else {
        ui.painter().rect_stroke(b, 4.0, Stroke::new(1.5, if hovered { MUTED } else { BORDER_STRONG }), egui::StrokeKind::Inside);
    }
}

/// A custom checkbox with a text label.
pub fn checkbox(ui: &mut egui::Ui, value: &mut bool, text: RichText) -> egui::Response {
    let galley = egui::WidgetText::from(text.clone()).into_galley(ui, Some(egui::TextWrapMode::Wrap), ui.available_width() - 28.0, egui::TextStyle::Body);
    let size = egui::vec2(28.0 + galley.size().x, galley.size().y.max(22.0));
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let on = *value;
    let label = text.text().to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), on, label.clone()));
    let box_rect = egui::Rect::from_min_size(rect.min, egui::vec2(18.0, 22.0));
    paint_check(ui, box_rect, on, resp.hovered());
    ui.painter().galley(egui::pos2(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A checkbox with a bold title and a muted explanation underneath.
pub fn option_row(ui: &mut egui::Ui, value: &mut bool, title: &str, desc: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        checkbox(ui, value, RichText::new(title).font(semibold(15.0)).color(TEXT));
        egui::Frame::new().inner_margin(egui::Margin { left: 28, ..Default::default() }).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(desc).size(13.5).color(MUTED)).wrap());
        });
    });
    ui.add_space(10.0);
}

/// A row of joined toggle buttons; returns true when the selection changed.
pub fn segmented(ui: &mut egui::Ui, id: &str, selected: &mut usize, options: &[&str]) -> bool {
    let mut changed = false;
    let frame = egui::Frame::new().fill(SURFACE_0).stroke(Stroke::new(1.0, BORDER)).corner_radius(8.0).inner_margin(egui::Margin::same(3));
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (i, opt) in options.iter().enumerate() {
                let on = *selected == i;
                let galley = ui.painter().layout_no_wrap(opt.to_string(), if on { semibold(14.0) } else { FontId::proportional(14.0) }, if on { TEXT } else { MUTED });
                let size = egui::vec2(galley.size().x + 28.0, 30.0);
                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                let o = opt.to_string();
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, o.clone()));
                if on {
                    ui.painter().rect_filled(rect, 6.0, SURFACE_3);
                    ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, BORDER_STRONG), egui::StrokeKind::Inside);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 6.0, SURFACE_2);
                }
                ui.painter().galley(rect.center() - galley.size() / 2.0, galley, TEXT);
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                    *selected = i;
                    changed = true;
                }
            }
        });
    });
    let _ = id;
    changed
}

/// A low-emphasis button: text only until hovered, then a subtle background.
pub fn ghost_button(ui: &mut egui::Ui, glyph: Option<&str>, text: &str) -> egui::Response {
    let b = egui::Button::new(label_job(glyph, text, 14.0, MUTED, false)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, BORDER)).corner_radius(8.0).min_size(egui::vec2(0.0, 32.0));
    let r = ui.add(b);
    if r.hovered() {
        ui.painter().rect_filled(r.rect, 8.0, Color32::from_white_alpha(12));
        ui.painter().rect_stroke(r.rect, 8.0, Stroke::new(1.0, BORDER_STRONG), egui::StrokeKind::Inside);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Thin chevron for dropdowns.
pub fn chevron_icon(ui: &egui::Ui, rect: egui::Rect, _v: &egui::style::WidgetVisuals, _open: bool) {
    let c = rect.center();
    ui.painter().line(vec![c + egui::vec2(-4.5, -2.0), c + egui::vec2(0.0, 2.5), c + egui::vec2(4.5, -2.0)], Stroke::new(1.5, MUTED));
}

pub fn dialog_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_1)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(14.0)
        .inner_margin(egui::Margin::same(22))
        .shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(140) })
}
