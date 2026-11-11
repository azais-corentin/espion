//! The window's controls, painted here. Each one carries a distinct accessible name (its
//! `name`, or `name: option` for the options of a choice), so the `egui` MCP tools find it.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align, Align2, Color32, Key, Layout, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, emath, pos2, vec2,
};

use super::theme::{
    self, BODY, CONTROL, FAINT, FAULT, FAULT_FILL, GAP, HOVER, Hue, LINE, MUTED, RADIUS,
    RADIUS_SMALL, RAISED, SMALL, TEXT,
};

/// Width of the names' column in a [`row`].
const NAME_WIDTH: f32 = 92.0;

/// A name on the left, its control in the rest of the row.
pub fn row<R>(ui: &mut Ui, name: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(NAME_WIDTH, CONTROL), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            name,
            theme::sans(BODY),
            MUTED,
        );
        add(ui)
    })
    .inner
}

/// A section of the rail: a tick in the stage's hue, the title, then the contents.
pub fn section<R>(ui: &mut Ui, title: &str, hue: Hue, add: impl FnOnce(&mut Ui) -> R) -> R {
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_owned(), theme::strong(BODY), TEXT);
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), galley.size().y + 2.0),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Heading, true, title));
    let tick = Rect::from_min_size(
        rect.left_top() + vec2(0.0, 2.0),
        vec2(3.0, rect.height() - 4.0),
    );
    ui.painter().rect_filled(tick, 1.5, hue.line);
    ui.painter()
        .galley(pos2(rect.left() + 10.0, rect.top() + 1.0), galley, TEXT);
    let r = add(ui);
    ui.add_space(GAP + 4.0);
    r
}

/// One option of a [`segmented`] choice.
pub struct Choice<'a, T> {
    pub value: T,
    pub text: &'a str,
    /// Why it cannot be chosen now (shown when the pointer rests on it).
    pub refused: Option<String>,
}

impl<'a, T> Choice<'a, T> {
    pub const fn new(value: T, text: &'a str) -> Self {
        Self {
            value,
            text,
            refused: None,
        }
    }
}

/// One of several options, in rows of `columns`. The chosen one takes the stage's hue.
/// Returns whether the choice changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    name: &str,
    hue: Hue,
    value: &mut T,
    choices: &[Choice<'_, T>],
    columns: usize,
) -> bool {
    let columns = columns.clamp(1, choices.len().max(1));
    let rows = choices.len().div_ceil(columns);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, rows as f32 * CONTROL), Sense::hover());
    let id = ui.make_persistent_id(("choice", name));
    ui.painter().rect_filled(rect, RADIUS, RAISED);
    let cell = vec2(width / columns as f32, CONTROL);
    let enabled = ui.is_enabled();
    let mut changed = false;
    for (i, c) in choices.iter().enumerate() {
        let at = rect.min + vec2((i % columns) as f32 * cell.x, (i / columns) as f32 * cell.y);
        let r = Rect::from_min_size(at, cell);
        let allowed = enabled && c.refused.is_none();
        let chosen = *value == c.value;
        let sense = if allowed {
            Sense::click()
        } else {
            Sense::hover()
        };
        let mut response = ui.interact(r, id.with(i), sense);
        response.widget_info(|| {
            WidgetInfo::selected(
                Role::RadioButton,
                allowed,
                chosen,
                format!("{name}: {}", c.text),
            )
        });
        if response.clicked() && !chosen {
            *value = c.value;
            changed = true;
            response.mark_changed();
        }
        let inner = r.shrink(2.0);
        let painter = ui.painter();
        let (font, color) = if chosen {
            painter.rect(
                inner,
                RADIUS_SMALL,
                hue.fill,
                Stroke::new(1.0, hue.line),
                StrokeKind::Inside,
            );
            (theme::strong(BODY), hue.text)
        } else if !allowed {
            (theme::sans(BODY), FAINT)
        } else if response.hovered() {
            painter.rect_filled(inner, RADIUS_SMALL, HOVER);
            (theme::sans(BODY), TEXT)
        } else {
            (theme::sans(BODY), MUTED)
        };
        painter.text(r.center(), Align2::CENTER_CENTER, c.text, font, color);
        if let Some(why) = &c.refused {
            response.on_hover_text(why);
        }
    }
    changed
}

/// An on/off switch after its name. Returns whether it was flipped.
pub fn switch(ui: &mut Ui, name: &str, hue: Hue, on: &mut bool) -> Response {
    let size = vec2(34.0, 18.0);
    ui.horizontal(|ui| {
        let width = ui.available_width();
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, CONTROL), Sense::click());
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, *on, name));
        let painter = ui.painter();
        painter.text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            name,
            theme::sans(BODY),
            if enabled { TEXT } else { FAINT },
        );
        let track = Rect::from_min_size(
            pos2(rect.right() - size.x, rect.center().y - size.y / 2.0),
            size,
        );
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let fill = lerp_color(RAISED, hue.fill, t);
        let line = lerp_color(LINE, hue.line, t);
        painter.rect(
            track,
            size.y / 2.0,
            fill,
            Stroke::new(1.0, line),
            StrokeKind::Inside,
        );
        let x = emath::lerp(track.left() + 9.0..=track.right() - 9.0, t);
        let knob = if response.hovered() { 6.5 } else { 6.0 };
        painter.circle_filled(
            pos2(x, track.center().y),
            knob,
            if enabled {
                lerp_color(MUTED, TEXT, t)
            } else {
                FAINT
            },
        );
        response
    })
    .inner
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// What a [`slider`] moves over.
pub struct Scale {
    pub range: RangeInclusive<f64>,
    /// Values are multiples of this above the range's start.
    pub step: f64,
    /// Equal distances multiply the value (for ranges over decades).
    pub logarithmic: bool,
}

impl Scale {
    /// Where `v` sits along the track, 0..=1.
    fn position(&self, v: f64) -> f64 {
        let (a, b) = (*self.range.start(), *self.range.end());
        if b <= a {
            return 0.0;
        }
        let t = if self.logarithmic {
            (v / a).ln() / (b / a).ln()
        } else {
            (v - a) / (b - a)
        };
        t.clamp(0.0, 1.0)
    }

    /// The value at `t` (0..=1) along the track, on the grid.
    fn value_at(&self, t: f64) -> f64 {
        let (a, b) = (*self.range.start(), *self.range.end());
        let v = if self.logarithmic {
            a * (b / a).powf(t)
        } else {
            a + (b - a) * t
        };
        self.snap(v)
    }

    fn snap(&self, v: f64) -> f64 {
        let (a, b) = (*self.range.start(), *self.range.end());
        let v = if self.step > 0.0 {
            a + ((v - a) / self.step).round() * self.step
        } else {
            v
        };
        v.clamp(a, b.max(a))
    }
}

/// A slider: its name and `shown` (the value with its unit) above the track. Dragging or
/// clicking the track sets the value; with the keyboard focus the arrow keys move it by one
/// step (ten with Shift). Returns whether the value changed.
pub fn slider(
    ui: &mut Ui,
    name: &str,
    hue: Hue,
    value: &mut f64,
    scale: &Scale,
    shown: &str,
) -> bool {
    let enabled = ui.is_enabled();
    let width = ui.available_width();
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(width, 20.0 + 18.0), Sense::click_and_drag());
    let before = *value;
    let knob_r = 6.0;
    let track = Rect::from_min_max(
        pos2(rect.left() + knob_r, rect.bottom() - 11.0),
        pos2(rect.right() - knob_r, rect.bottom() - 7.0),
    );
    if enabled {
        if (response.dragged() || response.clicked())
            && let Some(p) = response.interact_pointer_pos()
        {
            let t = f64::from((p.x - track.left()) / track.width().max(1.0));
            *value = scale.value_at(t.clamp(0.0, 1.0));
        }
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        if response.has_focus() {
            let (left, right, shift) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp),
                    i.modifiers.shift,
                )
            });
            let by = scale.step.max(f64::MIN_POSITIVE) * if shift { 10.0 } else { 1.0 };
            if left {
                *value = scale.snap(*value - by);
            }
            if right {
                *value = scale.snap(*value + by);
            }
        }
    }
    let changed = *value != before;
    if changed {
        response.mark_changed();
    }
    let v = *value;
    response.widget_info(|| WidgetInfo::slider(enabled, v, name));

    let painter = ui.painter();
    let top = pos2(rect.left(), rect.top() + 9.0);
    painter.text(top, Align2::LEFT_CENTER, name, theme::sans(BODY), MUTED);
    painter.text(
        pos2(rect.right(), top.y),
        Align2::RIGHT_CENTER,
        shown,
        theme::mono(BODY),
        if enabled { TEXT } else { FAINT },
    );
    let x = emath::lerp(track.left()..=track.right(), scale.position(v) as f32);
    painter.rect_filled(track, 2.0, LINE);
    let filled = Rect::from_min_max(track.min, pos2(x, track.max.y));
    painter.rect_filled(filled, 2.0, if enabled { hue.line } else { FAINT });
    let active = response.hovered() || response.dragged();
    let centre = pos2(x, track.center().y);
    painter.circle_filled(
        centre,
        if active && enabled {
            knob_r + 1.0
        } else {
            knob_r
        },
        if enabled { TEXT } else { FAINT },
    );
    if response.has_focus() {
        painter.circle_stroke(centre, knob_r + 3.0, Stroke::new(1.0, hue.line));
    }
    changed
}

/// A number to drag or type, as wide as the space left, named `name` for the tools (its
/// visible name is the caller's: a [`row`] or the text beside it). Returns whether it changed.
pub fn number<N: emath::Numeric>(
    ui: &mut Ui,
    name: &str,
    value: &mut N,
    range: RangeInclusive<N>,
    speed: f64,
    decimals: usize,
    width: f32,
) -> bool {
    let enabled = ui.is_enabled();
    let response = ui.add_sized(
        [width, CONTROL],
        egui::DragValue::new(value)
            .range(range)
            .speed(speed)
            .fixed_decimals(decimals),
    );
    let v = value.to_f64();
    response.widget_info(|| WidgetInfo {
        label: Some(name.to_owned()),
        ..WidgetInfo::drag_value(enabled, v)
    });
    response.changed()
}

/// A button as wide as the space left; `hue` fills it (the one that acts), `None` leaves it
/// quiet.
pub fn button(ui: &mut Ui, text: &str, hue: Option<Hue>) -> Response {
    let enabled = ui.is_enabled();
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), CONTROL + 2.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    let (fill, line, color) = match (enabled, hue) {
        (false, _) => (RAISED, LINE, FAINT),
        (true, Some(h)) if response.hovered() => (h.line, h.line, theme::BG),
        (true, Some(h)) => (h.fill, h.line, h.text),
        (true, None) if response.hovered() => (HOVER, MUTED, TEXT),
        (true, None) => (RAISED, LINE, TEXT),
    };
    let rect = if response.is_pointer_button_down_on() {
        rect.shrink(1.0)
    } else {
        rect
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        RADIUS,
        fill,
        Stroke::new(1.0, line),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::strong(BODY),
        color,
    );
    response
}

/// A small button that fits its text, for bars.
pub fn chip(ui: &mut Ui, text: &str, on: bool) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::sans(BODY), TEXT);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 20.0, CONTROL), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, text));
    let (fill, line, color) = if on {
        (theme::CHOSEN, MUTED, TEXT)
    } else if response.hovered() {
        (HOVER, MUTED, TEXT)
    } else {
        (RAISED, LINE, MUTED)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        RADIUS,
        fill,
        Stroke::new(1.0, line),
        StrokeKind::Inside,
    );
    painter.galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, color);
    response
}

/// What a [`notice`] is about.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something to know before acting.
    Caution,
    /// A fault or a refusal.
    Fault,
}

/// A message in the flow of a panel, wrapped to its width.
pub fn notice(ui: &mut Ui, tone: Tone, text: &str) {
    let (fill, bar, color) = match tone {
        Tone::Caution => (RAISED, MUTED, TEXT),
        Tone::Fault => (FAULT_FILL, FAULT, theme::FAULT),
    };
    let frame = egui::Frame::new()
        .fill(fill)
        .corner_radius(RADIUS_SMALL)
        .inner_margin(egui::Margin {
            left: 11,
            right: 8,
            top: 6,
            bottom: 6,
        });
    let r = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(text)
                    .color(color)
                    .font(theme::sans(BODY)),
            );
        })
        .response;
    let bar_rect = Rect::from_min_size(r.rect.min, vec2(3.0, r.rect.height()));
    ui.painter().rect_filled(bar_rect, 1.5, bar);
}

/// A line of small muted text, wrapped: what the driver chose, a hint.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .color(MUTED)
            .font(theme::sans(SMALL)),
    );
}

/// A counter: its name on the left, its value in fixed-width digits on the right (under the
/// name when both do not fit on one line). The value is named by the name for the tools.
pub fn stat(ui: &mut Ui, name: &str, value: &str) {
    stat_toned(ui, name, value, false);
}

/// A [`stat`] whose value is a fault when `fault`.
pub fn stat_toned(ui: &mut Ui, name: &str, value: &str, fault: bool) {
    let width = ui.available_width();
    let painter = ui.painter().clone();
    let name_g = painter.layout_no_wrap(name.to_owned(), theme::sans(BODY), MUTED);
    let value_g = painter.layout_no_wrap(
        value.to_owned(),
        theme::mono(BODY),
        if fault { FAULT } else { TEXT },
    );
    let one_line = name_g.size().x + 12.0 + value_g.size().x <= width;
    let line = 20.0;
    let height = if one_line { line } else { 2.0 * line };
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let named =
        Align2::LEFT_CENTER.anchor_size(pos2(rect.left(), rect.top() + line / 2.0), name_g.size());
    let shown = Align2::RIGHT_CENTER.anchor_size(
        pos2(rect.right(), rect.bottom() - line / 2.0),
        value_g.size(),
    );
    painter.galley(named.min, name_g, MUTED);
    painter.galley(shown.min, value_g, TEXT);
    let id = ui.make_persistent_id(("stat", name));
    let name_response = ui.interact(named, id, Sense::hover());
    name_response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
    let value_response = ui.interact(shown, id.with("value"), Sense::hover());
    value_response.widget_info(|| WidgetInfo::labeled(Role::Label, true, value));
    value_response.labelled_by(name_response.id);
}

/// A hairline across the panel.
pub fn rule(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, LINE));
}

/// Lays `add` out from the right edge.
pub fn from_right<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(Layout::right_to_left(Align::Center), add)
        .inner
}

/// The last minute of a figure, one value a second.
#[derive(Debug, Clone)]
pub struct History {
    values: [f32; Self::LEN],
    /// Values held.
    len: usize,
    /// Where the next value goes.
    next: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            values: [0.0; Self::LEN],
            len: 0,
            next: 0,
        }
    }
}

impl History {
    const LEN: usize = 60;

    pub fn push(&mut self, v: f32) {
        self.values[self.next] = v;
        self.next = (self.next + 1) % Self::LEN;
        self.len = (self.len + 1).min(Self::LEN);
    }

    /// The values held, oldest first.
    fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.next + Self::LEN - self.len) % Self::LEN;
        (0..self.len).map(move |i| self.values[(start + i) % Self::LEN])
    }
}

/// A figure of the status bar: its name, its value, and the last minute behind it as a line
/// from zero to its largest value.
pub fn figure(ui: &mut Ui, name: &str, value: &str, unit: &str, history: &History, hue: Hue) {
    let painter = ui.painter().clone();
    let name_g = painter.layout_no_wrap(name.to_owned(), theme::sans(SMALL), MUTED);
    let value_g = painter.layout_no_wrap(value.to_owned(), theme::mono_strong(theme::FIGURE), TEXT);
    let unit_g = painter.layout_no_wrap(unit.to_owned(), theme::sans(SMALL), MUTED);
    let spark = vec2(56.0, 20.0);
    let text_w = name_g
        .size()
        .x
        .max(value_g.size().x + 4.0 + unit_g.size().x);
    let size = vec2(text_w.max(70.0) + 10.0 + spark.x, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo {
        label: Some(name.to_owned()),
        current_text_value: Some(format!("{value} {unit}")),
        ..WidgetInfo::new(Role::Status)
    });
    painter.galley(rect.left_top(), name_g, MUTED);
    let base = rect.bottom() - 1.0;
    let value_at = pos2(rect.left(), base - value_g.size().y);
    let unit_at = pos2(
        rect.left() + value_g.size().x + 4.0,
        base - unit_g.size().y - 1.5,
    );
    painter.galley(value_at, value_g, TEXT);
    painter.galley(unit_at, unit_g, MUTED);

    let area = Rect::from_min_size(
        pos2(rect.right() - spark.x, rect.bottom() - spark.y - 3.0),
        spark,
    );
    let top = history.iter().fold(0.0f32, f32::max);
    if history.len >= 2 && top > 0.0 {
        let n = History::LEN as f32 - 1.0;
        let first = History::LEN - history.len;
        let points: Vec<_> = history
            .iter()
            .enumerate()
            .map(|(i, v)| {
                pos2(
                    area.left() + (first + i) as f32 / n * area.width(),
                    area.bottom() - (v / top).clamp(0.0, 1.0) * area.height(),
                )
            })
            .collect();
        if let Some(&last) = points.last() {
            painter.line(points, Stroke::new(1.2, theme::fade(hue.line, 0.75)));
            painter.circle_filled(last, 2.0, hue.line);
        }
    }
    painter.hline(area.x_range(), area.bottom(), Stroke::new(1.0, LINE));
}

/// Minimum size helper for bars.
pub const fn bar_height() -> f32 {
    46.0
}
