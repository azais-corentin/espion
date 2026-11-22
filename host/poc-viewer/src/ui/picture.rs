//! The picture in its area: fitted, or zoomed with the wheel and moved by dragging, with the
//! pixel under the pointer, and the board's blobs drawn over it in image coordinates (so they
//! follow the zoom and the pan). With frames off, a dark canvas of the sensor's size takes the
//! picture's place under the blobs.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use eframe::egui::{
    Align2, Color32, CursorIcon, Pos2, Rect, Role, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
    pos2, vec2,
};
use espion_proto::cam::Encoding;
use poc_viewer::{
    blobs::{TrailPoint, px},
    net::{BlobFrame, Frame},
    screen_target::check::{FrameMatch, Place},
};

use super::theme::{self, BODY, LINE, MUTED, RADIUS_SMALL, SMALL, TEXT, WELL};
use crate::display::{ImageCallback, Mask, OverlayStamp, Shown, UploadTiming};

/// Most screen points per image pixel.
const SCALE_MAX: f32 = 64.0;

/// Trail points drawn at most (an even sample of the trail beyond).
const TRAIL_MAX: usize = 6000;

/// The canvas under the blobs when no frames come.
const CANVAS: Color32 = Color32::from_rgb(0x16, 0x17, 0x1a);

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

/// What the area shows under the blobs.
pub enum Content<'a> {
    /// A frame, with the threshold mask if on.
    Frame(&'a Arc<Shown>, Option<Mask>),
    /// A dark canvas of this size (frames off).
    Canvas { width: u16, height: u16 },
}

/// The blobs drawn over the picture.
pub struct Overlay<'a> {
    /// The blobs of the frame shown (or the newest, on the canvas), each a cross and a circle.
    pub blobs: Option<&'a BlobFrame>,
    /// The trail, faded by age.
    pub trail: &'a [TrailPoint],
    pub colour: Color32,
    /// The screen target: the part of the page's viewport on the sensor (polylines, sensor
    /// pixels) and what the page drew for the frame shown, against its blobs.
    pub target: Option<TargetOverlay<'a>>,
}

pub struct TargetOverlay<'a> {
    pub outline: &'a [Vec<(f64, f64)>],
    pub matched: Option<&'a FrameMatch>,
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

/// Shows `content` in the space left with `overlay` over it, and handles the pointer: the
/// wheel zooms about the pointer, dragging moves a zoomed picture, a double click fits it
/// again. `view` is updated.
pub fn show(
    ui: &mut Ui,
    view: &mut View,
    content: &Content<'_>,
    gain: f32,
    overlay: &Overlay<'_>,
    timing: &Arc<Mutex<UploadTiming>>,
) {
    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());
    let (w, h) = match content {
        Content::Frame(s, _) => (s.frame.width, s.frame.height),
        Content::Canvas { width, height } => (*width, *height),
    };
    let image = vec2(f32::from(w), f32::from(h));
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
        current_text_value: Some(match content {
            Content::Frame(s, _) => format!("{w}x{h}, frame {}", s.frame.number),
            Content::Canvas { .. } => format!("{w}x{h}, frames off: blobs only"),
        }),
        ..WidgetInfo::new(Role::Image)
    });

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, WELL);
    let rect = Rect::from_min_size(area.center() - centre * scale, image * scale);
    match content {
        Content::Frame(shown, mask) => {
            let vis = rect.intersect(area);
            if vis.width() > 0.0 && vis.height() > 0.0 {
                let uv0 = (vis.min - rect.min) / rect.size();
                let uv1 = (vis.max - rect.min) / rect.size();
                painter.add(
                    ImageCallback {
                        shown: (*shown).clone(),
                        uv: [uv0.x, uv0.y, uv1.x, uv1.y],
                        gain,
                        mask: *mask,
                        timing: timing.clone(),
                    }
                    .paint(vis),
                );
            }
        }
        Content::Canvas { .. } => {
            painter.rect(
                rect,
                0.0,
                CANVAS,
                Stroke::new(1.0, LINE),
                StrokeKind::Outside,
            );
            painter.text(
                rect.left_top() + vec2(10.0, 10.0),
                Align2::LEFT_TOP,
                "Frames off: the board's blobs only",
                theme::sans(BODY),
                MUTED,
            );
        }
    }

    // Image pixel (x, y), its centre at (x + 0.5, y + 0.5) of the texture: the detector puts
    // pixel centres on integers.
    let at = |x: f64, y: f64| rect.min + vec2(x as f32 + 0.5, y as f32 + 0.5) * scale;
    if let Some(t) = &overlay.target {
        paint_target(ui, &painter, area, t, &at, scale);
    }
    paint_overlay(ui, &painter, area, overlay, at, scale);
    if let Some(b) = overlay.blobs {
        painter.add(
            OverlayStamp {
                seq: b.seq,
                timing: timing.clone(),
            }
            .paint(area),
        );
    }

    // The pixel under the pointer.
    let Content::Frame(shown, _) = content else {
        return;
    };
    let f = &shown.frame;
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

/// The screen target under the blobs: the outline of the page's viewport, then each marker the
/// page drew (a ring of twice its sigma, joined to its blob by a line; a missed one in the
/// fault colour, one at the edge dashed), each an accessible `Drawn marker N: ..`.
fn paint_target(
    ui: &Ui,
    painter: &eframe::egui::Painter,
    area: Rect,
    t: &TargetOverlay<'_>,
    at: &impl Fn(f64, f64) -> Pos2,
    scale: f32,
) {
    let edge = Stroke::new(1.0, theme::fade(MUTED, 0.7));
    for line in t.outline {
        let points: Vec<Pos2> = line.iter().map(|&(u, v)| at(u, v)).collect();
        painter.add(eframe::egui::Shape::line(points, edge));
    }
    let Some(m) = t.matched else {
        return;
    };
    let shadow = Stroke::new(3.0, Color32::from_black_alpha(140));
    for (i, (mk, place, blob)) in m.markers.iter().enumerate() {
        if *place == Place::Outside {
            continue;
        }
        let p = at(mk.u, mk.v);
        if !area.expand(30.0).contains(p) {
            continue;
        }
        let missed = blob.is_none() && *place == Place::Inside;
        let colour = if missed { theme::FAULT } else { TEXT };
        let ring = Stroke::new(1.0, colour);
        let radius = (2.0 * mk.sigma as f32 * scale).max(8.0);
        if *place == Place::Edge {
            let dash: Vec<Pos2> = (0..=48)
                .map(|k| {
                    let a = k as f32 / 48.0 * std::f32::consts::TAU;
                    p + vec2(a.cos(), a.sin()) * radius
                })
                .collect();
            painter.add(eframe::egui::Shape::dashed_line(&dash, ring, 3.0, 3.0));
        } else {
            painter.circle_stroke(p, radius, shadow);
            painter.circle_stroke(p, radius, ring);
        }
        let state = match (place, blob) {
            (_, Some(j)) => {
                let b = &m.blobs[*j];
                let q = at(b.x, b.y);
                painter.line_segment([p, q], Stroke::new(1.0, TEXT));
                format!("found, off by {:.3} px", (b.x - mk.u).hypot(b.y - mk.v))
            }
            (Place::Edge, None) => "at the edge, not found".to_owned(),
            _ => "missed".to_owned(),
        };
        let name = format!(
            "Drawn marker {}: x {:.2} y {:.2}, {state}",
            i + 1,
            mk.u,
            mk.v
        );
        ui.interact(
            Rect::from_center_size(p, Vec2::splat(10.0)),
            ui.make_persistent_id(("drawn marker", i)),
            Sense::hover(),
        )
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    }
}

/// The trail (each marker's centres as dots joined in order, fading with age; the blobs' IDs
/// tell the markers apart), then each blob: a cross at its centre, a circle of twice its
/// width, its ID; each blob an accessible `Marker ID: x .. y ..`.
fn paint_overlay(
    ui: &Ui,
    painter: &eframe::egui::Painter,
    area: Rect,
    overlay: &Overlay<'_>,
    at: impl Fn(f64, f64) -> Pos2,
    scale: f32,
) {
    let colour = overlay.colour;
    let step = overlay.trail.len().div_ceil(TRAIL_MAX).max(1);
    let mut last: HashMap<u32, Pos2> = HashMap::new();
    for t in overlay.trail.iter().step_by(step) {
        let p = at(t.x, t.y);
        let faded = theme::fade(colour, 0.8 * (1.0 - t.age));
        let from = last.insert(t.id, p);
        if let Some(q) = from
            && (area.contains(p) || area.contains(q))
        {
            painter.line_segment([q, p], Stroke::new(1.0, faded));
        }
        if area.contains(p) {
            painter.circle_filled(p, 1.6, faded);
        }
    }
    let Some(frame) = overlay.blobs else {
        return;
    };
    let stroke = Stroke::new(1.5, colour);
    let shadow = Stroke::new(3.5, Color32::from_black_alpha(160));
    for b in &frame.blobs {
        let (x, y) = (px(b.x), px(b.y));
        let p = at(x, y);
        if !area.expand(20.0).contains(p) {
            continue;
        }
        let radius = (2.0 * px(u32::from(b.width)) as f32 * scale).max(5.0);
        let arm = 7.0;
        for s in [shadow, stroke] {
            painter.line_segment([p - vec2(arm, 0.0), p + vec2(arm, 0.0)], s);
            painter.line_segment([p - vec2(0.0, arm), p + vec2(0.0, arm)], s);
            painter.circle_stroke(p, radius, s);
        }
        // The ID on a dark backing: it is drawn in the trails' colour, over the trails.
        let label = painter.layout_no_wrap(b.id.to_string(), theme::mono(SMALL), colour);
        let corner = p + vec2(radius * 0.72 + 3.0, -radius * 0.72 - 3.0);
        let place = Align2::LEFT_BOTTOM.anchor_size(corner, label.size());
        painter.rect_filled(place.expand(1.5), 2.0, Color32::from_black_alpha(190));
        painter.galley(place.min, label, colour);
        let id = ui.make_persistent_id(("marker", b.id));
        let name = format!("Marker {}: x {x:.3} y {y:.3}", b.id);
        ui.interact(
            Rect::from_center_size(p, Vec2::splat(14.0)),
            id,
            Sense::hover(),
        )
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
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
