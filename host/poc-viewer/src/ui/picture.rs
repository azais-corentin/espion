//! The picture in its area: fitted, or zoomed with the wheel and moved by dragging, with the
//! pixel under the pointer.

use std::sync::{Arc, Mutex};

use eframe::egui::{
    Align2, CursorIcon, Pos2, Rect, Role, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2,
    vec2,
};
use espion_proto::cam::Encoding;
use poc_viewer::net::Frame;

use super::theme::{self, BODY, LINE, MUTED, RADIUS_SMALL, TEXT, WELL};
use crate::display::{ImageCallback, Shown, UploadTiming};

/// Most screen points per image pixel.
const SCALE_MAX: f32 = 64.0;

/// How the picture sits in its area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum View {
    /// As large as fits, centred.
    Fit,
    /// One image pixel per screen pixel, centred.
    Pixels,
    /// `scale` screen points per image pixel, the image point `centre` (pixels) at the centre
    /// of the area.
    Free { scale: f32, centre: Vec2 },
}

/// The pixel `(x, y)` of a RAW8 or packed RAW10 frame, at the frame's depth.
fn pixel(f: &Frame, x: usize, y: usize) -> Option<u16> {
    let w = usize::from(f.width);
    match f.encoding {
        Encoding::Raw10 => {
            let group = f.data.get(y * (w / 4 * 5) + x / 4 * 5..)?.get(..5)?;
            let low = (group[4] >> (2 * (x % 4))) & 3;
            Some(u16::from(group[x % 4]) << 2 | u16::from(low))
        }
        Encoding::Raw8 | Encoding::Jpeg => f.data.get(y * w + x).map(|&v| u16::from(v)),
    }
}

/// Shows `shown` in the space left and handles the pointer: the wheel zooms about the pointer,
/// dragging moves a zoomed picture, a double click fits it again. `view` is updated.
pub fn show(
    ui: &mut Ui,
    view: &mut View,
    shown: &Arc<Shown>,
    gain: f32,
    timing: &Arc<Mutex<UploadTiming>>,
) {
    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());
    let f = &shown.frame;
    let image = vec2(f32::from(f.width), f32::from(f.height));
    let fit = (area.width() / image.x).min(area.height() / image.y);
    let pixels = 1.0 / ui.ctx().pixels_per_point();
    let (mut scale, mut centre) = match *view {
        View::Fit => (fit, image / 2.0),
        View::Pixels => (pixels, image / 2.0),
        View::Free { scale, centre } => (scale, centre),
    };
    let to_image = |p: Pos2, scale: f32, centre: Vec2| centre + (p - area.center()) / scale;

    if let Some(p) = response.hover_pos() {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = pinch * (scroll * 0.004).exp();
        if factor != 1.0 {
            let under = to_image(p, scale, centre);
            scale = (scale * factor).clamp(fit.min(pixels), SCALE_MAX);
            // The image point under the pointer stays under it.
            centre = under - (p - area.center()) / scale;
            *view = View::Free { scale, centre };
        }
    }
    if response.dragged() && *view != View::Fit {
        centre -= response.drag_delta() / scale;
        *view = View::Free { scale, centre };
    }
    // The picture never leaves the area's centre.
    centre = centre.clamp(Vec2::ZERO, image);
    if response.double_clicked() || (matches!(view, View::Free { .. }) && scale <= fit) {
        *view = View::Fit;
    } else if let View::Free { .. } = view {
        *view = View::Free { scale, centre };
    }
    if *view != View::Fit {
        ui.ctx().set_cursor_icon(if response.dragged() {
            CursorIcon::Grabbing
        } else {
            CursorIcon::Grab
        });
    }
    let (scale, centre) = match *view {
        View::Fit => (fit, image / 2.0),
        View::Pixels => (pixels, image / 2.0),
        View::Free { scale, centre } => (scale, centre),
    };
    response.widget_info(|| WidgetInfo {
        label: Some("Picture".to_owned()),
        current_text_value: Some(format!("{}x{}, frame {}", f.width, f.height, f.number)),
        ..WidgetInfo::new(Role::Image)
    });

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, WELL);
    let rect = Rect::from_min_size(area.center() - centre * scale, image * scale);
    let vis = rect.intersect(area);
    if vis.width() > 0.0 && vis.height() > 0.0 {
        let uv0 = (vis.min - rect.min) / rect.size();
        let uv1 = (vis.max - rect.min) / rect.size();
        painter.add(
            ImageCallback {
                shown: shown.clone(),
                uv: [uv0.x, uv0.y, uv1.x, uv1.y],
                gain,
                timing: timing.clone(),
            }
            .paint(vis),
        );
    }

    // The pixel under the pointer.
    let Some(p) = response.hover_pos() else {
        return;
    };
    let at = to_image(p, scale, centre);
    if at.x < 0.0 || at.y < 0.0 || at.x >= image.x || at.y >= image.y {
        return;
    }
    let (x, y) = (at.x as usize, at.y as usize);
    let Some(value) = pixel(f, x, y) else {
        return;
    };
    let text = format!("x {x:>4}  y {y:>3}  {value:>4}");
    let galley = painter.layout_no_wrap(text, theme::mono(BODY), TEXT);
    let tag = Rect::from_min_size(
        pos2(area.left() + 10.0, area.bottom() - galley.size().y - 18.0),
        galley.size() + vec2(16.0, 8.0),
    );
    painter.rect(
        tag,
        RADIUS_SMALL,
        theme::fade(theme::PANEL, 0.9),
        Stroke::new(1.0, LINE),
        StrokeKind::Inside,
    );
    painter.galley(tag.min + vec2(8.0, 4.0), galley, TEXT);
    if scale >= 8.0 {
        // The pixel itself, once pixels are large enough to tell apart.
        let corner = rect.min + vec2(x as f32, y as f32) * scale;
        painter.rect_stroke(
            Rect::from_min_size(corner, Vec2::splat(scale)),
            0.0,
            Stroke::new(1.0, MUTED),
            StrokeKind::Inside,
        );
    }
}

/// What the area says while there is no frame.
pub fn empty(ui: &mut Ui, title: &str, hint: &str) {
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, WELL);
    painter.text(
        area.center() - vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        title,
        theme::strong(theme::FIGURE),
        TEXT,
    );
    painter.text(
        area.center() + vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        hint,
        theme::sans(BODY),
        MUTED,
    );
}
