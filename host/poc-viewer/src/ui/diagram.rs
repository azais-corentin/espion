//! The pipeline timing diagrams, drawn from [`Snapshot`]s (one a second).
//!
//! - The ribbon: the typical frame as one segmented bar, the waits between stages as gaps,
//!   and the latency. Always on screen.
//! - The staircase: one row per stage and three consecutive frames, so the overlap of the
//!   stages of successive frames shows. Shown when the timing strip is expanded.
//!
//! Solid bars are measured, dashed ones computed from the sensor mode; a whisker reaches to
//! the slowest sample of the window. A new snapshot glides into place in [`GLIDE`].

use std::time::{Duration, Instant};

use eframe::egui::{
    Align2, Color32, Pos2, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};
use poc_viewer::timing::{self, Snapshot, Stage, Step};

use super::theme::{self, BODY, FAINT, Hue, LINE, MUTED, RADIUS_SMALL, SMALL, TEXT, WAIT, hue};

/// How long a new snapshot takes to glide into place.
const GLIDE: Duration = Duration::from_millis(150);

/// What the diagrams show: the latest snapshot, eased in from the one before.
#[derive(Debug, Default)]
pub struct Diagram {
    target: Option<Snapshot>,
    /// The steps shown when the target arrived.
    from: Vec<Step>,
    since: Option<Instant>,
    /// The steps shown now.
    shown: Vec<Step>,
    total_us: f32,
    /// Length of the ribbon's axis, µs.
    ribbon_axis_us: f32,
}

impl Diagram {
    /// Takes `snapshot` if it is new, and moves the steps shown towards it. Returns whether
    /// they are still moving.
    pub fn update(&mut self, snapshot: Option<&Snapshot>, now: Instant) -> bool {
        match (snapshot, &self.target) {
            (None, _) => {
                self.target = None;
                self.shown.clear();
                return false;
            }
            (Some(s), Some(t)) if s.serial == t.serial => {}
            (Some(s), _) => {
                let same_stages = self.shown.len() == s.steps.len()
                    && self
                        .shown
                        .iter()
                        .zip(&s.steps)
                        .all(|(a, b)| a.stage == b.stage);
                self.from.clear();
                self.from
                    .extend_from_slice(if same_stages { &self.shown } else { &s.steps });
                self.target = Some(s.clone());
                self.since = Some(now);
                self.ribbon_axis_us = timing::axis_us(self.ribbon_axis_us, s.total_us);
            }
        }
        let Some(t) = &self.target else {
            return false;
        };
        let k = self
            .since
            .map_or(1.0, |at| (now - at).as_secs_f32() / GLIDE.as_secs_f32())
            .clamp(0.0, 1.0);
        let k = 1.0 - (1.0 - k).powi(3);
        self.shown.clear();
        self.shown
            .extend(t.steps.iter().zip(&self.from).map(|(to, from)| Step {
                start_us: from.start_us + (to.start_us - from.start_us) * k,
                len_us: from.len_us + (to.len_us - from.len_us) * k,
                slowest_us: from.slowest_us + (to.slowest_us - from.slowest_us) * k,
                ..*to
            }));
        self.total_us = self.shown.last().map_or(0.0, Step::end_us);
        k < 1.0
    }

    /// The snapshot shown, if any.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.target.as_ref()
    }

    /// The ribbon, as wide as the space left.
    pub fn ribbon(&self, ui: &mut Ui) {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(vec2(width, 112.0), Sense::hover());
        let Some(s) = &self.target else {
            ui.painter().text(
                rect.left_center(),
                Align2::LEFT_CENTER,
                "Timing appears once the board answers and frames arrive.",
                theme::sans(BODY),
                MUTED,
            );
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, "Pipeline timing"));
            return;
        };
        response.widget_info(|| WidgetInfo {
            label: Some("Pipeline timing".to_owned()),
            current_text_value: Some(summary(s)),
            ..WidgetInfo::new(Role::Figure)
        });
        let painter = ui.painter_at(rect);

        // The total on the right.
        let total = format!("{:.1}", self.total_us / 1e3);
        let big = painter.layout_no_wrap(total, theme::mono_strong(theme::HERO), TEXT);
        let caption = if s.complete {
            "ms, exposure to GPU"
        } else {
            "ms so far (partial)"
        };
        let small = painter.layout_no_wrap(caption.to_owned(), theme::sans(SMALL), MUTED);
        let side = big.size().x.max(small.size().x) + 8.0;
        let bar_y = rect.top() + 38.0;
        let bar_h = 30.0;
        let left = rect.left();
        let right = rect.right() - side - 20.0;
        let scale = (right - left) / self.ribbon_axis_us.max(1.0);
        let x = |us: f32| left + us.max(0.0) * scale;
        painter.galley(pos2(rect.right() - side, bar_y - 6.0), big, TEXT);
        painter.galley(pos2(rect.right() - side, bar_y + bar_h - 4.0), small, MUTED);

        // Waits, then the stages.
        let mut prev_end = 0.0;
        for st in &self.shown {
            if st.start_us - prev_end > 1.0 {
                let r = Rect::from_min_max(
                    pos2(x(prev_end) + 1.0, bar_y + 6.0),
                    pos2(x(st.start_us) - 1.0, bar_y + bar_h - 6.0),
                );
                dotted_rect(&painter, r, WAIT.line);
                let gap = (st.start_us - prev_end) / 1e3;
                if r.width() > 40.0 {
                    painter.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        format!("{gap:.1}"),
                        theme::mono(SMALL),
                        MUTED,
                    );
                }
            }
            prev_end = st.end_us();
        }
        let mut label_right = f32::NEG_INFINITY;
        for st in &self.shown {
            let h = hue(st.stage);
            let r = Rect::from_min_max(
                pos2(x(st.start_us) + 0.5, bar_y),
                pos2(
                    (x(st.end_us()) - 0.5).max(x(st.start_us) + 3.0),
                    bar_y + bar_h,
                ),
            );
            bar(&painter, r, h, st.measured, 1.0);
            let len = format!("{:.1}", st.len_us / 1e3);
            if r.width() >= 40.0 {
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    &len,
                    theme::mono(BODY),
                    h.text,
                );
            }
            // The name above; narrow stages get "name length" and a leader.
            let name = st.stage.name();
            let wide = r.width() >= 40.0;
            let text = if wide {
                name.to_owned()
            } else {
                format!("{name} {len}")
            };
            let g =
                painter.layout_no_wrap(text, theme::sans(SMALL), if wide { MUTED } else { TEXT });
            let mut at = (r.center().x - g.size().x / 2.0)
                .max(label_right + 8.0)
                .max(left);
            at = at.min(right - g.size().x);
            let y = if wide { bar_y - 18.0 } else { bar_y - 34.0 };
            if !wide {
                painter.vline(
                    r.center().x,
                    (y + g.size().y + 1.0)..=(bar_y - 2.0),
                    Stroke::new(1.0, h.line),
                );
            }
            label_right = at + g.size().x;
            painter.galley(pos2(at, y), g, TEXT);
            if st.measured && st.slowest_us > st.len_us * 1.02 {
                whisker(
                    &painter,
                    x(st.end_us()),
                    x(st.start_us + st.slowest_us),
                    bar_y + bar_h + 5.0,
                    h.line,
                );
            }
        }
        // Not measured: from the hand-off to the display.
        if s.complete {
            let x0 = x(self.total_us) + 3.0;
            dashed_h(
                &painter,
                x0,
                x0 + 18.0,
                bar_y + bar_h / 2.0,
                FAINT,
                2.0,
                3.0,
            );
        }
        // The frames that start meanwhile.
        let ty = bar_y + bar_h + 16.0;
        painter.hline(left..=right, ty, Stroke::new(1.0, LINE));
        let mut k = 0;
        while s.period_us > 0.0 && k < 64 {
            let us = k as f32 * s.period_us;
            if us > self.ribbon_axis_us {
                break;
            }
            painter.vline(x(us), (ty - 4.0)..=(ty + 4.0), Stroke::new(1.0, MUTED));
            k += 1;
        }
        let in_flight = (self.total_us / s.period_us.max(1.0)).ceil().max(1.0);
        painter.text(
            pos2(left, ty + 9.0),
            Align2::LEFT_TOP,
            format!(
                "A frame starts every {:.2} ms ({:.1} Hz): {in_flight:.0} in the pipeline at once",
                s.period_us / 1e3,
                1e6 / s.period_us.max(1.0),
            ),
            theme::sans(SMALL),
            MUTED,
        );
        if let Some(p) = response.hover_pos() {
            self.tooltip(ui, &response, p, x, bar_y..=bar_y + bar_h);
        }
    }

    /// The staircase, as wide as the space left, `height` tall.
    pub fn staircase(&self, ui: &mut Ui, height: f32) {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let Some(s) = &self.target else {
            return;
        };
        response.widget_info(|| WidgetInfo {
            label: Some("Pipeline timing by stage".to_owned()),
            current_text_value: Some(summary(s)),
            ..WidgetInfo::new(Role::Figure)
        });
        let painter = ui.painter_at(rect);
        let names = 120.0;
        let left = rect.left() + names + 16.0;
        let right = rect.right() - 8.0;
        let axis = s.axis_us.max(1.0);
        let scale = (right - left) / axis;
        let x = |us: f32| left + us * scale;
        let top = rect.top() + 26.0;
        let bottom = rect.bottom() - 52.0;
        let rows = self.shown.len().max(1);
        let pitch = (bottom - top) / rows as f32;
        let bar_h = (pitch - 8.0).clamp(10.0, 30.0);

        // Grid and axis.
        let tick = axis / 5.0;
        for i in 0..=5 {
            let us = i as f32 * tick;
            let gx = x(us);
            dashed_v(&painter, gx, top - 6.0, bottom + 2.0, LINE, 3.0, 4.0);
            let text = if i == 0 || i == 5 {
                format!("{} ms", trim(us / 1e3))
            } else {
                trim(us / 1e3)
            };
            painter.text(
                pos2(gx, bottom + 8.0),
                match i {
                    0 => Align2::LEFT_TOP,
                    5 => Align2::RIGHT_TOP,
                    _ => Align2::CENTER_TOP,
                },
                text,
                theme::mono(SMALL),
                MUTED,
            );
        }
        // Three frames, a period apart; frame labels over their exposures.
        for (k, alpha) in [(0usize, 1.0f32), (1, 0.55), (2, 0.35)] {
            let offset = k as f32 * s.period_us;
            if x(offset) > right {
                break;
            }
            painter.text(
                pos2(x(offset) + 2.0, rect.top() + 4.0),
                Align2::LEFT_TOP,
                format!("Frame {}", k + 1),
                theme::sans(SMALL),
                theme::fade(MUTED, alpha.max(0.6)),
            );
            for (i, st) in self.shown.iter().enumerate() {
                let y = top + i as f32 * pitch + (pitch - bar_h) / 2.0;
                let x0 = x(st.start_us + offset);
                if x0 > right {
                    continue;
                }
                let x1 = x(st.end_us() + offset).max(x0 + 3.0).min(right);
                let r = Rect::from_min_max(pos2(x0, y), pos2(x1, y + bar_h));
                let h = hue(st.stage);
                bar(&painter, r, h, st.measured, alpha);
                if r.width() >= 18.0 {
                    painter.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        format!("{}", k + 1),
                        theme::mono(SMALL),
                        theme::fade(h.text, alpha.max(0.7)),
                    );
                }
                if k == 0 && st.measured && st.slowest_us > st.len_us * 1.02 {
                    whisker(
                        &painter,
                        r.right(),
                        x(st.start_us + st.slowest_us).min(right),
                        r.center().y,
                        h.line,
                    );
                }
            }
        }
        // Names and medians.
        for (i, st) in self.shown.iter().enumerate() {
            let cy = top + i as f32 * pitch + pitch / 2.0;
            painter.text(
                pos2(rect.left(), cy),
                Align2::LEFT_CENTER,
                st.stage.name(),
                theme::sans(BODY),
                TEXT,
            );
            painter.text(
                pos2(rect.left() + names, cy),
                Align2::RIGHT_CENTER,
                format!("{:.2} ms", st.len_us / 1e3),
                theme::mono(SMALL),
                MUTED,
            );
        }
        // Frame 1's latency.
        let by = rect.bottom() - 24.0;
        let x1 = x(self.total_us).min(right);
        painter.add(Shape::line(
            vec![
                pos2(left, by - 6.0),
                pos2(left, by),
                pos2(x1, by),
                pos2(x1, by - 6.0),
            ],
            Stroke::new(1.2, MUTED),
        ));
        painter.text(
            pos2((left + x1) / 2.0, by + 4.0),
            Align2::CENTER_TOP,
            format!(
                "Frame 1: {:.1} ms from the start of the exposure to {}",
                self.total_us / 1e3,
                if s.complete {
                    "the GPU"
                } else {
                    "the last stage measured"
                }
            ),
            theme::sans(SMALL),
            MUTED,
        );
        if let Some(p) = response.hover_pos() {
            let row = ((p.y - top) / pitch).floor();
            if row >= 0.0
                && let Some(st) = self.shown.get(row as usize)
            {
                let st = *st;
                response
                    .clone()
                    .on_hover_ui_at_pointer(|ui| step_tip(ui, &st, &self.shown));
            }
        }
    }

    fn tooltip(
        &self,
        _ui: &Ui,
        response: &eframe::egui::Response,
        p: Pos2,
        x: impl Fn(f32) -> f32,
        band: std::ops::RangeInclusive<f32>,
    ) {
        if !band.contains(&p.y) {
            return;
        }
        if let Some(st) = self
            .shown
            .iter()
            .find(|st| (x(st.start_us)..=x(st.end_us()).max(x(st.start_us) + 3.0)).contains(&p.x))
        {
            let st = *st;
            response
                .clone()
                .on_hover_ui_at_pointer(|ui| step_tip(ui, &st, &self.shown));
        }
    }
}

/// The figures of one stage, for a tooltip.
fn step_tip(ui: &mut Ui, st: &Step, steps: &[Step]) {
    let before = steps
        .iter()
        .take_while(|s| s.stage != st.stage)
        .last()
        .map_or(0.0, Step::end_us);
    ui.label(theme_text(st.stage.name(), theme::strong(BODY), TEXT));
    let what = match st.stage {
        Stage::Exposure => "Computed: exposure lines x line time",
        Stage::Readout => "Computed: lines read x line time; ends at the capture",
        Stage::Encode => "Measured on the board: the hardware JPEG encode",
        Stage::Send => {
            "Measured: the board taking the frame to its last chunk arriving here (clocks \
             matched by the fastest datagram of the window)"
        }
        Stage::Decode => "Measured here: the JPEG decode",
        Stage::Draw => {
            "Measured here: the window's pass that took the picture, to the end of its upload"
        }
    };
    ui.label(theme_text(what, theme::sans(SMALL), MUTED));
    let line = |ui: &mut Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.label(theme_text(k, theme::sans(SMALL), MUTED));
            ui.label(theme_text(&v, theme::mono(SMALL), TEXT));
        });
    };
    line(ui, "Median", format!("{:.2} ms", st.len_us / 1e3));
    if st.measured {
        line(ui, "Slowest", format!("{:.2} ms", st.slowest_us / 1e3));
    }
    if st.start_us - before > 1.0 {
        line(
            ui,
            "Waited before",
            format!("{:.2} ms", (st.start_us - before) / 1e3),
        );
    }
    line(ui, "Starts at", format!("{:.2} ms", st.start_us / 1e3));
    if st.stage == Stage::Draw {
        ui.label(theme_text(
            "From here to the display (vsync, compositor, scan-out) is not measured.",
            theme::sans(SMALL),
            MUTED,
        ));
    }
}

fn theme_text(s: &str, font: eframe::egui::FontId, c: Color32) -> eframe::egui::RichText {
    eframe::egui::RichText::new(s).font(font).color(c)
}

/// The diagram in one line, for the tools and the screen reader.
fn summary(s: &Snapshot) -> String {
    let mut out = format!(
        "{:.2} ms{}:",
        s.total_us / 1e3,
        if s.complete {
            " to the GPU"
        } else {
            " (partial)"
        }
    );
    for st in &s.steps {
        out.push_str(&format!(
            " {} {:.2} at {:.2};",
            st.stage.name(),
            st.len_us / 1e3,
            st.start_us / 1e3
        ));
    }
    out.pop();
    out
}

/// A tick label: whole numbers without decimals.
fn trim(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// A stage's bar: solid when measured, dashed outline when computed.
fn bar(painter: &eframe::egui::Painter, r: Rect, h: Hue, measured: bool, alpha: f32) {
    if measured {
        painter.rect(
            r,
            RADIUS_SMALL,
            theme::fade(h.fill, alpha),
            Stroke::new(1.0, theme::fade(h.line, alpha)),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(r, RADIUS_SMALL, theme::fade(h.fill, 0.5 * alpha));
        dashed_rect(painter, r.shrink(0.5), theme::fade(h.line, alpha));
    }
}

fn dashed_rect(painter: &eframe::egui::Painter, r: Rect, c: Color32) {
    dashed_h(painter, r.left(), r.right(), r.top(), c, 5.0, 3.0);
    dashed_h(painter, r.left(), r.right(), r.bottom(), c, 5.0, 3.0);
    dashed_v(painter, r.left(), r.top(), r.bottom(), c, 5.0, 3.0);
    dashed_v(painter, r.right(), r.top(), r.bottom(), c, 5.0, 3.0);
}

fn dotted_rect(painter: &eframe::egui::Painter, r: Rect, c: Color32) {
    if r.width() < 2.0 {
        return;
    }
    dashed_h(painter, r.left(), r.right(), r.top(), c, 1.5, 3.0);
    dashed_h(painter, r.left(), r.right(), r.bottom(), c, 1.5, 3.0);
}

fn dashed_h(
    painter: &eframe::egui::Painter,
    x0: f32,
    x1: f32,
    y: f32,
    c: Color32,
    dash: f32,
    gap: f32,
) {
    painter.extend(Shape::dashed_line(
        &[pos2(x0, y), pos2(x1, y)],
        Stroke::new(1.0, c),
        dash,
        gap,
    ));
}

fn dashed_v(
    painter: &eframe::egui::Painter,
    x: f32,
    y0: f32,
    y1: f32,
    c: Color32,
    dash: f32,
    gap: f32,
) {
    painter.extend(Shape::dashed_line(
        &[pos2(x, y0), pos2(x, y1)],
        Stroke::new(1.0, c),
        dash,
        gap,
    ));
}

/// From the median's end to the slowest sample's end, with a cap.
fn whisker(painter: &eframe::egui::Painter, x0: f32, x1: f32, y: f32, c: Color32) {
    if x1 - x0 < 2.0 {
        return;
    }
    painter.hline(x0..=x1, y, Stroke::new(1.2, c));
    painter.vline(x1, (y - 3.5)..=(y + 3.5), Stroke::new(1.2, c));
}
