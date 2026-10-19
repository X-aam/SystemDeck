//! Visual design: light theme with a navy navigation rail and teal accent,
//! plus painter-drawn icons (the bundled fonts have no icon glyphs).

use eframe::egui::{self, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Vec2};
use systemdeck_core::EntryKind;

use crate::sessions::Status;

use std::sync::atomic::{AtomicBool, Ordering};

pub const ACCENT: Color32 = Color32::from_rgb(56, 194, 178);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(36, 166, 152);
pub const OK: Color32 = Color32::from_rgb(46, 184, 114);
pub const WARN: Color32 = Color32::from_rgb(236, 164, 44);
pub const DANGER: Color32 = Color32::from_rgb(232, 84, 92);

static DARK: AtomicBool = AtomicBool::new(false);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

fn pick(light: (u8, u8, u8), dark: (u8, u8, u8)) -> Color32 {
    let (r, g, b) = if is_dark() { dark } else { light };
    Color32::from_rgb(r, g, b)
}

/// Card / column background.
pub fn panel() -> Color32 {
    pick((255, 255, 255), (30, 34, 41))
}
/// Window background behind cards.
pub fn app_bg() -> Color32 {
    pick((244, 247, 250), (22, 25, 31))
}
pub fn border() -> Color32 {
    pick((228, 233, 239), (48, 54, 64))
}
pub fn text() -> Color32 {
    pick((52, 60, 72), (222, 227, 234))
}
pub fn text_weak() -> Color32 {
    pick((146, 156, 168), (134, 145, 160))
}
pub fn row_selected() -> Color32 {
    pick((236, 245, 248), (32, 60, 64))
}
pub fn row_hover() -> Color32 {
    pick((248, 250, 252), (38, 43, 52))
}
pub fn subtle() -> Color32 {
    pick((250, 251, 253), (34, 38, 46))
}
pub fn danger_bg() -> Color32 {
    pick((253, 240, 241), (64, 32, 36))
}

pub fn apply(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = panel();
    v.window_fill = panel();
    v.faint_bg_color = app_bg();
    v.extreme_bg_color = pick((246, 248, 251), (24, 27, 33));
    v.code_bg_color = app_bg();
    v.hyperlink_color = ACCENT_DARK;
    v.selection.bg_fill = pick((196, 236, 231), (28, 96, 90));
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT_DARK);
    v.window_corner_radius = CornerRadius::same(12);
    v.window_stroke = Stroke::new(1.0_f32, border());
    v.menu_corner_radius = CornerRadius::same(8);
    v.striped = false;

    let radius = CornerRadius::same(7);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = radius;
        w.fg_stroke = Stroke::new(1.0_f32, text());
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, border());
    v.widgets.noninteractive.bg_fill = panel();
    v.widgets.noninteractive.weak_bg_fill = panel();
    let inactive = pick((240, 243, 247), (44, 50, 60));
    v.widgets.inactive.weak_bg_fill = inactive;
    v.widgets.inactive.bg_fill = inactive;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    let hovered = pick((230, 236, 242), (54, 61, 73));
    v.widgets.hovered.weak_bg_fill = hovered;
    v.widgets.hovered.bg_fill = hovered;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, pick((208, 216, 226), (76, 86, 100)));
    let active = pick((218, 226, 235), (64, 72, 86));
    v.widgets.active.weak_bg_fill = active;
    v.widgets.active.bg_fill = active;
    v.widgets.open.weak_bg_fill = hovered;
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 5.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.window_margin = egui::Margin::same(16);
    use egui::{FontFamily::Proportional, TextStyle};
    style.text_styles.insert(TextStyle::Heading, FontId::new(19.0, Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(13.5, Proportional));
    style.text_styles.insert(TextStyle::Button, FontId::new(13.5, Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(11.0, Proportional));
    ctx.set_style(style);
}

pub fn kind_color(kind: EntryKind) -> Color32 {
    match kind {
        EntryKind::Rdp => Color32::from_rgb(64, 132, 238),
        EntryKind::Ssh => Color32::from_rgb(46, 176, 120),
        EntryKind::Vnc => Color32::from_rgb(150, 98, 222),
        EntryKind::Telnet => Color32::from_rgb(232, 142, 52),
        EntryKind::Serial => Color32::from_rgb(170, 118, 80),
        EntryKind::Web => Color32::from_rgb(32, 164, 190),
        EntryKind::RemoteSupport => Color32::from_rgb(220, 72, 120),
        EntryKind::Credential => Color32::from_rgb(214, 168, 36),
        EntryKind::Note => Color32::from_rgb(128, 136, 148),
    }
}

pub fn status_color(status: &Status) -> Color32 {
    match status {
        Status::Connected => OK,
        Status::Connecting => WARN,
        Status::Closed(_) | Status::Failed(_) => DANGER,
    }
}

pub fn status_text(status: &Status) -> String {
    match status {
        Status::Connected => "Connected".into(),
        Status::Connecting => "Connecting...".into(),
        Status::Closed(m) | Status::Failed(m) => m.clone(),
    }
}

/// Filled circle with centred short text (kind badge or initials).
pub fn avatar(painter: &Painter, center: Pos2, radius: f32, fill: Color32, text: &str) {
    painter.circle_filled(center, radius, fill);
    let size = if text.chars().count() > 3 { radius * 0.55 } else { radius * 0.62 };
    painter.text(center, egui::Align2::CENTER_CENTER, text, FontId::proportional(size), Color32::WHITE);
}

/// Rounded "pill" with text, e.g. counters and LIVE markers.
pub fn pill(painter: &Painter, right_center: Pos2, text: &str, fill: Color32, color: Color32) -> Rect {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(10.5), color);
    let size = egui::vec2((galley.size().x + 12.0).max(20.0), 18.0);
    let rect = Rect::from_min_size(right_center - egui::vec2(size.x, size.y / 2.0), size);
    painter.rect_filled(rect, 9.0, fill);
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    rect
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Play,
    Folder,
    Pencil,
    More,
    Plus,
    Import,
    Search,
    Sync,
    Close,
    Check,
}

pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) / 2.0;
    let stroke = Stroke::new((s * 0.16).max(1.4), color);
    match icon {
        Icon::Play => {
            let pts = vec![c + egui::vec2(-s * 0.35, -s * 0.5), c + egui::vec2(s * 0.55, 0.0), c + egui::vec2(-s * 0.35, s * 0.5)];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Icon::Folder => {
            let body = Rect::from_center_size(c + egui::vec2(0.0, s * 0.1), egui::vec2(s * 1.4, s * 0.95));
            let tab = Rect::from_min_size(body.min - egui::vec2(0.0, s * 0.22), egui::vec2(s * 0.6, s * 0.3));
            painter.rect_filled(tab, 2.0, color);
            painter.rect_stroke(body, 2.0, stroke, egui::StrokeKind::Middle);
        }
        Icon::Pencil => {
            let a = c + egui::vec2(-s * 0.55, s * 0.55);
            let b = c + egui::vec2(s * 0.45, -s * 0.45);
            painter.line_segment([a + egui::vec2(s * 0.12, -s * 0.12), b], Stroke::new(s * 0.32, color));
            painter.add(Shape::convex_polygon(vec![a, a + egui::vec2(s * 0.3, 0.0), a + egui::vec2(0.0, -s * 0.3)], color, Stroke::NONE));
        }
        Icon::More => {
            for dy in [-0.5, 0.0, 0.5] {
                painter.circle_filled(c + egui::vec2(0.0, s * dy), s * 0.13, color);
            }
        }
        Icon::Plus => {
            painter.line_segment([c - egui::vec2(s * 0.5, 0.0), c + egui::vec2(s * 0.5, 0.0)], stroke);
            painter.line_segment([c - egui::vec2(0.0, s * 0.5), c + egui::vec2(0.0, s * 0.5)], stroke);
        }
        Icon::Import => {
            painter.line_segment([c - egui::vec2(0.0, s * 0.6), c + egui::vec2(0.0, s * 0.15)], stroke);
            painter.line_segment([c + egui::vec2(-s * 0.3, -s * 0.12), c + egui::vec2(0.0, s * 0.18)], stroke);
            painter.line_segment([c + egui::vec2(s * 0.3, -s * 0.12), c + egui::vec2(0.0, s * 0.18)], stroke);
            painter.line_segment([c + egui::vec2(-s * 0.6, s * 0.3), c + egui::vec2(-s * 0.6, s * 0.6)], stroke);
            painter.line_segment([c + egui::vec2(-s * 0.6, s * 0.6), c + egui::vec2(s * 0.6, s * 0.6)], stroke);
            painter.line_segment([c + egui::vec2(s * 0.6, s * 0.6), c + egui::vec2(s * 0.6, s * 0.3)], stroke);
        }
        Icon::Search => {
            painter.circle_stroke(c - egui::vec2(s * 0.15, s * 0.15), s * 0.42, stroke);
            painter.line_segment([c + egui::vec2(s * 0.17, s * 0.17), c + egui::vec2(s * 0.55, s * 0.55)], stroke);
        }
        Icon::Sync => {
            let n = 14;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let t = 0.35 + i as f32 / n as f32 * 4.9;
                    c + egui::vec2(t.cos(), t.sin()) * s * 0.55
                })
                .collect();
            let end = *pts.last().unwrap();
            painter.add(Shape::line(pts, stroke));
            painter.add(Shape::convex_polygon(
                vec![end + egui::vec2(-s * 0.28, -s * 0.05), end + egui::vec2(s * 0.12, -s * 0.3), end + egui::vec2(s * 0.12, s * 0.12)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Check => {
            painter.add(Shape::line(
                vec![c + egui::vec2(-s * 0.5, 0.0), c + egui::vec2(-s * 0.12, s * 0.38), c + egui::vec2(s * 0.55, -s * 0.42)],
                Stroke::new((s * 0.22).max(1.6), color),
            ));
        }
        Icon::Close => {
            painter.line_segment([c - Vec2::splat(s * 0.4), c + Vec2::splat(s * 0.4)], stroke);
            painter.line_segment([c + egui::vec2(-s * 0.4, s * 0.4), c + egui::vec2(s * 0.4, -s * 0.4)], stroke);
        }
    }
}

/// Round icon button as in the reference design. `primary` = filled accent.
pub fn round_button(ui: &mut egui::Ui, icon: Icon, primary: bool, diameter: f32, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    let painter = ui.painter();
    let hovered = resp.hovered();
    let (fill, stroke, fg) = if primary {
        (if hovered { ACCENT_DARK } else { ACCENT }, Stroke::NONE, Color32::WHITE)
    } else {
        (if hovered { app_bg() } else { panel() }, Stroke::new(1.0_f32, border()), if hovered { text() } else { text_weak() })
    };
    painter.circle(rect.center(), diameter / 2.0, fill, stroke);
    paint_icon(painter, Rect::from_center_size(rect.center(), Vec2::splat(diameter * 0.42)), icon, fg);
    resp.on_hover_text(tooltip)
}

pub fn initials(name: &str) -> String {
    let parts: Vec<&str> = name.split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-').filter(|p| !p.is_empty()).collect();
    let s: String = match parts.as_slice() {
        [] => "ME".into(),
        [one] => one.chars().take(2).collect(),
        [first, .., last] => format!("{}{}", first.chars().next().unwrap_or(' '), last.chars().next().unwrap_or(' ')),
    };
    s.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("Kelly Hartmann"), "KH");
        assert_eq!(initials("admin"), "AD");
        assert_eq!(initials("j.smith"), "JS");
        assert_eq!(initials(""), "ME");
    }
}
