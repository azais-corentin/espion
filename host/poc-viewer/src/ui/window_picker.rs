//! The sensor's active area to scale, with the window to read: drag it to move it, drag its
//! lower right corner to resize it. Positions and sizes snap to the driver's grid.

use eframe::egui::{
    Align2, CursorIcon, Rect, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};
use espion_proto::cam::{CameraSettings, Origin};
use ov9281::limits;

use super::theme::{self, FAINT, Hue, LINE, MUTED, RADIUS_SMALL, SMALL, TEXT, WELL};
use poc_viewer::camera;

/// Size of the corner handle, points.
const HANDLE: f32 = 9.0;

/// `v` rounded to a multiple of `m` within `lo..=hi` (`lo` and `hi` multiples of `m`).
fn snap(v: f32, m: u16, lo: u16, hi: u16) -> u16 {
    let m = f32::from(m.max(1));
    ((v / m).round() * m).clamp(f32::from(lo), f32::from(hi)) as u16
}

/// The largest multiple of `m` not above `v`.
fn floor_to(v: u16, m: u16) -> u16 {
    v / m.max(1) * m.max(1)
}

/// The origin a centred window gets (as the driver centres it).
fn centred_origin(c: &CameraSettings) -> Origin {
    let s = limits::scale(camera::readout(c.readout));
    let (w, h) = (c.width.saturating_mul(s), c.height.saturating_mul(s));
    Origin {
        x: floor_to(
            limits::ACTIVE_WIDTH.saturating_sub(w) / 2,
            limits::ORIGIN_X_MULTIPLE,
        ),
        y: floor_to(
            limits::ACTIVE_HEIGHT.saturating_sub(h) / 2,
            limits::origin_y_multiple(camera::readout(c.readout)),
        ),
    }
}

/// Shows the picker for the edited settings `c`; moving the window sets `origin` and clears
/// `centred`, resizing sets the size. Returns whether anything changed.
pub fn show(
    ui: &mut Ui,
    hue: Hue,
    c: &mut CameraSettings,
    centred: &mut bool,
    origin: &mut Origin,
) -> bool {
    let readout = camera::readout(c.readout);
    let format = camera::format(c.format);
    let s = limits::scale(readout);
    let (aw, ah) = (limits::ACTIVE_WIDTH, limits::ACTIVE_HEIGHT);
    let width = ui.available_width();
    let k = width / f32::from(aw);
    let (rect, _) = ui.allocate_exact_size(vec2(width, f32::from(ah) * k), Sense::hover());
    let id = ui.make_persistent_id("sensor window");
    let enabled = ui.is_enabled();

    let shown_origin = if *centred { centred_origin(c) } else { *origin };
    let to_screen = |x: f32, y: f32| pos2(rect.left() + x * k, rect.top() + y * k);
    let win = Rect::from_min_max(
        to_screen(f32::from(shown_origin.x), f32::from(shown_origin.y)),
        to_screen(
            f32::from(shown_origin.x) + f32::from(c.width) * f32::from(s),
            f32::from(shown_origin.y) + f32::from(c.height) * f32::from(s),
        ),
    )
    .intersect(rect);

    let handle = Rect::from_center_size(win.right_bottom(), vec2(HANDLE * 2.0, HANDLE * 2.0));
    let sense = if enabled {
        Sense::drag()
    } else {
        Sense::hover()
    };
    // The handle last, so it is on top of the body where they overlap.
    let body = ui.interact(win, id.with("move"), sense);
    let resize = ui.interact(handle, id.with("size"), sense);
    let mut changed = false;
    let (max_w, max_h) = limits::max_size(readout);
    let (wm, hm) = (
        u16::from(limits::width_multiple(format)),
        u16::from(limits::height_multiple(format)),
    );
    if resize.dragged()
        && let Some(p) = resize.interact_pointer_pos()
    {
        let o = shown_origin;
        let w = ((p.x - rect.left()) / k - f32::from(o.x)) / f32::from(s);
        let h = ((p.y - rect.top()) / k - f32::from(o.y)) / f32::from(s);
        let room_w = (aw - o.x) / s;
        let room_h = (ah - o.y) / s;
        let nw = snap(
            w,
            wm,
            limits::MIN_WIDTH.next_multiple_of(wm),
            floor_to(max_w.min(room_w), wm),
        );
        let nh = snap(
            h,
            hm,
            limits::MIN_HEIGHT.next_multiple_of(hm),
            floor_to(max_h.min(room_h), hm),
        );
        if (nw, nh) != (c.width, c.height) {
            (c.width, c.height) = (nw, nh);
            if !*centred {
                *origin = o;
            }
            changed = true;
        }
    } else if body.dragged() {
        let d = body.drag_delta() / k;
        let base = if *centred { centred_origin(c) } else { *origin };
        // The drag accumulates in the frame-to-frame deltas; keep the unsnapped position.
        let acc = ui.data_mut(|m| {
            let a = m.get_temp_mut_or_insert_with(id.with("acc"), || {
                (f32::from(base.x), f32::from(base.y))
            });
            a.0 += d.x;
            a.1 += d.y;
            *a
        });
        let span_w = c.width.saturating_mul(s);
        let span_h = c.height.saturating_mul(s);
        let ym = limits::origin_y_multiple(readout);
        let o = Origin {
            x: snap(
                acc.0,
                limits::ORIGIN_X_MULTIPLE,
                0,
                floor_to(aw.saturating_sub(span_w), limits::ORIGIN_X_MULTIPLE),
            ),
            y: snap(acc.1, ym, 0, floor_to(ah.saturating_sub(span_h), ym)),
        };
        if *centred || o != *origin {
            *origin = o;
            *centred = false;
            changed = true;
        }
    }
    if body.drag_stopped() || !body.dragged() {
        ui.data_mut(|m| m.remove::<(f32, f32)>(id.with("acc")));
    }
    if enabled {
        if resize.hovered() || resize.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe);
        } else if body.hovered() || body.dragged() {
            ui.ctx().set_cursor_icon(if body.dragged() {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            });
        }
    }
    let shown_origin = if *centred { centred_origin(c) } else { *origin };
    let (w, h) = (c.width, c.height);
    let label = format!(
        "{w}x{h} at {}, {}{}",
        shown_origin.x,
        shown_origin.y,
        if *centred { " (centred)" } else { "" }
    );
    body.widget_info(|| WidgetInfo {
        label: Some("Sensor window".to_owned()),
        current_text_value: Some(label.clone()),
        ..WidgetInfo::new(Role::Canvas)
    });
    resize.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, "Sensor window size"));

    let painter = ui.painter();
    painter.rect(
        rect,
        RADIUS_SMALL,
        WELL,
        Stroke::new(1.0, LINE),
        StrokeKind::Inside,
    );
    // A grid every 160 sensor pixels.
    for x in (160..aw).step_by(160) {
        let sx = rect.left() + f32::from(x) * k;
        painter.vline(sx, rect.y_range(), Stroke::new(1.0, theme::fade(LINE, 0.5)));
    }
    for y in (160..ah).step_by(160) {
        let sy = rect.top() + f32::from(y) * k;
        painter.hline(rect.x_range(), sy, Stroke::new(1.0, theme::fade(LINE, 0.5)));
    }
    let active = body.hovered() || body.dragged() || resize.dragged();
    let line = if enabled { hue.line } else { FAINT };
    painter.rect(
        win,
        2.0,
        theme::fade(hue.fill, if active { 0.9 } else { 0.7 }),
        Stroke::new(if active { 1.5 } else { 1.0 }, line),
        StrokeKind::Inside,
    );
    painter.text(
        win.center(),
        Align2::CENTER_CENTER,
        format!("{w}x{h}"),
        theme::mono(SMALL),
        if enabled { hue.text } else { FAINT },
    );
    let hr = Rect::from_center_size(win.right_bottom() - vec2(3.0, 3.0), vec2(HANDLE, HANDLE));
    painter.rect_filled(hr, 2.0, if enabled { TEXT } else { FAINT });
    painter.text(
        rect.right_top() + vec2(-6.0, 5.0),
        Align2::RIGHT_TOP,
        "1280 x 800",
        theme::mono(SMALL),
        MUTED,
    );
    changed
}
