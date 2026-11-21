//! The pipeline timing diagrams, drawn from [`Snapshot`]s (one a second).
//!
//! - The ribbon: one row per way of the frame on one time axis. The image row holds the
//!   sensor's stages and the picture's; with frames off the sensor's stages get a row of their
//!   own. The markers row starts with the detection under the readout. The waits between
//!   stages show as gaps, and each row has its latency on the right. Always on screen.
//! - The staircase: one row per stage, grouped by lane, and three consecutive frames, so the
//!   overlap of the stages of successive frames shows; the strips' boundaries on the readout,
//!   and core 1's figures under the axis. Shown when the timing strip is expanded.
//!
//! Solid bars are measured, dashed ones computed from the sensor mode; a whisker reaches to
//! the slowest sample of the window. A new snapshot glides into place in [`GLIDE`].

use std::time::{Duration, Instant};

use eframe::egui::{
    Align2, Color32, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};
use poc_viewer::timing::{self, Core1, Lane, Snapshot, Stage, Step};

use super::theme::{self, BODY, FAINT, Hue, LINE, MUTED, RADIUS_SMALL, SMALL, TEXT, WAIT, hue};

/// How long a new snapshot takes to glide into place.
const GLIDE: Duration = Duration::from_millis(150);

/// The ribbon: height of one row (names above the bar, the bar, the whisker), the bar's
/// height and its offset in the row, and the lane names' column.
const ROW_PITCH: f32 = 68.0;
const ROW_BAR: f32 = 26.0;
const ROW_BAR_Y: f32 = 34.0;
const LANE_NAMES: f32 = 72.0;
/// The ribbon's line of frame starts under the rows.
const TIMELINE: f32 = 34.0;

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

/// One row of the ribbon: the lane it is named after and its steps, in order.
struct Row {
    lane: Lane,
    steps: Vec<Step>,
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
        self.total_us = self.shown.iter().map(Step::end_us).fold(0.0, f32::max);
        k < 1.0
    }

    /// The snapshot shown, if any.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.target.as_ref()
    }

    /// The steps shown of `lane`.
    fn lane(&self, lane: Lane) -> impl Iterator<Item = &Step> {
        self.shown.iter().filter(move |s| s.stage.lane() == lane)
    }

    /// The end of the step shown of `stage`.
    fn end_of(&self, stage: Stage) -> Option<f32> {
        self.shown
            .iter()
            .find(|s| s.stage == stage)
            .map(Step::end_us)
    }

    /// The ribbon's rows: the image's (with the sensor's stages), or the sensor's alone when
    /// there is no image; then the markers'.
    fn rows(&self) -> Vec<Row> {
        let mut first = Row {
            lane: Lane::Sensor,
            steps: self.lane(Lane::Sensor).copied().collect(),
        };
        let image: Vec<Step> = self.lane(Lane::Image).copied().collect();
        if !image.is_empty() {
            first.lane = Lane::Image;
            first.steps.extend(image);
        }
        let markers: Vec<Step> = self.lane(Lane::Markers).copied().collect();
        let mut rows = vec![first];
        if !markers.is_empty() {
            rows.push(Row {
                lane: Lane::Markers,
                steps: markers,
            });
        }
        rows
    }

    /// A row's latency on the right: the figure in ms and what it measures.
    fn row_total(&self, s: &Snapshot, row: &Row, only: bool) -> Option<(f32, String)> {
        let end = row.steps.last().map(Step::end_us)?;
        match row.lane {
            Lane::Image => Some((
                end,
                if s.image_complete {
                    "ms, exposure to GPU".to_owned()
                } else {
                    "ms so far (partial)".to_owned()
                },
            )),
            Lane::Markers => {
                let here = self.end_of(Stage::MarkerSend)?;
                Some((
                    here,
                    match self.end_of(Stage::MarkerDraw) {
                        Some(d) => format!("ms, exposure to here; drawn at {:.1}", d / 1e3),
                        None => "ms, exposure to here".to_owned(),
                    },
                ))
            }
            Lane::Sensor => only.then(|| (end, "ms so far (partial)".to_owned())),
        }
    }

    /// The ribbon, as wide as the space left.
    pub fn ribbon(&self, ui: &mut Ui) {
        let width = ui.available_width();
        let rows = self.rows();
        let height = rows.len() as f32 * ROW_PITCH + TIMELINE;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let Some(s) = &self.target else {
            ui.painter().text(
                rect.left_center(),
                Align2::LEFT_CENTER,
                "Timing appears once the board answers and frames or blobs arrive.",
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

        // The latencies on the right, as wide as the widest.
        let only = rows.len() == 1;
        let totals: Vec<_> = rows
            .iter()
            .map(|row| {
                self.row_total(s, row, only).map(|(us, caption)| {
                    (
                        painter.layout_no_wrap(
                            format!("{:.1}", us / 1e3),
                            theme::mono_strong(theme::HERO),
                            TEXT,
                        ),
                        painter.layout_no_wrap(caption, theme::sans(SMALL), MUTED),
                    )
                })
            })
            .collect();
        let side = totals
            .iter()
            .flatten()
            .map(|(big, small)| big.size().x.max(small.size().x))
            .fold(0.0, f32::max)
            + 8.0;
        let left = rect.left() + LANE_NAMES;
        let right = rect.right() - side - 20.0;
        let scale = (right - left) / self.ribbon_axis_us.max(1.0);
        let x = |us: f32| left + us.max(0.0) * scale;

        let mut hovered = None;
        for (i, (row, total)) in rows.iter().zip(totals).enumerate() {
            let top = rect.top() + i as f32 * ROW_PITCH;
            let bar_y = top + ROW_BAR_Y;
            let band = bar_y..=bar_y + ROW_BAR;
            painter.text(
                pos2(rect.left(), bar_y + ROW_BAR / 2.0),
                Align2::LEFT_CENTER,
                row.lane.name(),
                theme::sans(BODY),
                MUTED,
            );
            if let Some((big, small)) = total {
                painter.galley(pos2(rect.right() - side, bar_y - 8.0), big, TEXT);
                painter.galley(
                    pos2(rect.right() - side, bar_y + ROW_BAR - 2.0),
                    small,
                    MUTED,
                );
            }
            self.ribbon_row(&painter, row, bar_y, left, right, x);
            if let Some(p) = response.hover_pos()
                && band.contains(&p.y)
            {
                hovered = row
                    .steps
                    .iter()
                    .rev()
                    .find(|st| {
                        (x(st.start_us)..=x(st.end_us()).max(x(st.start_us) + 3.0)).contains(&p.x)
                    })
                    .copied();
            }
        }

        // The frames that start meanwhile.
        let ty = rect.top() + rows.len() as f32 * ROW_PITCH;
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
        if let Some(st) = hovered {
            response
                .clone()
                .on_hover_ui_at_pointer(|ui| step_tip(ui, &st, &self.shown, s.core1.as_ref()));
        }
    }

    /// One ribbon row: its waits, its stages with their names above, whiskers, and a dashed
    /// tail after a draw (the display is not measured).
    fn ribbon_row(
        &self,
        painter: &eframe::egui::Painter,
        row: &Row,
        bar_y: f32,
        left: f32,
        right: f32,
        x: impl Fn(f32) -> f32,
    ) {
        let mut prev_end = row.steps.first().map_or(0.0, |s| s.start_us);
        for st in &row.steps {
            if st.start_us - prev_end > 1.0 {
                let r = Rect::from_min_max(
                    pos2(x(prev_end) + 1.0, bar_y + 6.0),
                    pos2(x(st.start_us) - 1.0, bar_y + ROW_BAR - 6.0),
                );
                dotted_rect(painter, r, WAIT.line);
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
        for st in &row.steps {
            let h = hue(st.stage);
            let r = Rect::from_min_max(
                pos2(x(st.start_us) + 0.5, bar_y),
                pos2(
                    (x(st.end_us()) - 0.5).max(x(st.start_us) + 3.0),
                    bar_y + ROW_BAR,
                ),
            );
            bar(painter, r, h, st.measured, 1.0);
            let len = format!("{:.1}", st.len_us / 1e3);
            let wide = r.width() >= 40.0;
            if wide {
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
            let text = if wide {
                name.to_owned()
            } else {
                format!("{name} {len}")
            };
            let g =
                painter.layout_no_wrap(text, theme::sans(SMALL), if wide { MUTED } else { TEXT });
            let at = (r.center().x - g.size().x / 2.0)
                .max(label_right + 8.0)
                .max(left)
                .min(right - g.size().x);
            let y = if wide { bar_y - 18.0 } else { bar_y - 32.0 };
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
                    painter,
                    x(st.end_us()),
                    x(st.start_us + st.slowest_us),
                    bar_y + ROW_BAR + 5.0,
                    h.line,
                );
            }
        }
        if let Some(last) = row.steps.last()
            && matches!(last.stage, Stage::Draw | Stage::MarkerDraw)
        {
            let x0 = x(last.end_us()) + 3.0;
            dashed_h(
                painter,
                x0,
                x0 + 18.0,
                bar_y + ROW_BAR / 2.0,
                FAINT,
                2.0,
                3.0,
            );
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
        let bottom = rect.bottom() - 76.0;

        // Rows: the sensor's stages, then each lane under its heading.
        const HEADING: f32 = 0.8;
        let headings = [Lane::Image, Lane::Markers]
            .into_iter()
            .filter(|&l| self.lane(l).next().is_some())
            .count();
        let units = self.shown.len().max(1) as f32 + HEADING * headings as f32;
        let pitch = (bottom - top) / units;
        let bar_h = (pitch - 8.0).clamp(8.0, 26.0);
        let mut rows: Vec<(f32, &Step)> = Vec::with_capacity(self.shown.len());
        let mut y = top;
        for lane in [Lane::Sensor, Lane::Image, Lane::Markers] {
            let mut steps = self.lane(lane).peekable();
            if lane != Lane::Sensor && steps.peek().is_some() {
                painter.text(
                    pos2(rect.left(), y + HEADING * pitch - 2.0),
                    Align2::LEFT_BOTTOM,
                    lane.name(),
                    theme::strong(SMALL),
                    MUTED,
                );
                painter.hline(
                    (rect.left() + 64.0)..=right,
                    y + HEADING * pitch / 2.0 + 2.0,
                    Stroke::new(1.0, LINE),
                );
                y += HEADING * pitch;
            }
            for st in steps {
                rows.push((y, st));
                y += pitch;
            }
        }

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
        // Three frames, a period apart; frame labels over their exposures where they fit
        // (the bars carry the frame's number anyway).
        let label_room = s.period_us * scale >= 56.0;
        for (k, alpha) in [(0usize, 1.0f32), (1, 0.55), (2, 0.35)] {
            let offset = k as f32 * s.period_us;
            if x(offset) > right {
                break;
            }
            if k == 0 || label_room {
                painter.text(
                    pos2(x(offset) + 2.0, rect.top() + 4.0),
                    Align2::LEFT_TOP,
                    format!("Frame {}", k + 1),
                    theme::sans(SMALL),
                    theme::fade(MUTED, alpha.max(0.6)),
                );
            }
            for &(ry, st) in &rows {
                let y = ry + (pitch - bar_h) / 2.0;
                let x0 = x(st.start_us + offset);
                if x0 > right {
                    continue;
                }
                let x1 = x(st.end_us() + offset).max(x0 + 3.0).min(right);
                let r = Rect::from_min_max(pos2(x0, y), pos2(x1, y + bar_h));
                let h = hue(st.stage);
                bar(&painter, r, h, st.measured, alpha);
                if r.width() >= 18.0 && bar_h >= 12.0 {
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
                // Frame 1's strips: where each ends on the readout, as detection takes them.
                if k == 0
                    && st.stage == Stage::Readout
                    && let Some(c) = &s.core1
                    && c.strips > 1
                    && c.strip_us * scale >= 3.0
                {
                    let line = Stroke::new(1.0, theme::fade(hue(Stage::Detect).line, 0.8));
                    for i in 1..c.strips {
                        let sx = x(st.start_us + f32::from(i) * c.strip_us);
                        if sx >= r.right() - 1.0 {
                            break;
                        }
                        painter.vline(sx, (r.top() + 2.0)..=(r.bottom() - 2.0), line);
                    }
                }
            }
        }
        // Names and medians.
        for &(ry, st) in &rows {
            let cy = ry + pitch / 2.0;
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
        // Frame 1's latencies: the bracket to the markers' arrival (else the image's end).
        let markers = self.end_of(Stage::MarkerSend);
        let drawn = self.end_of(Stage::MarkerDraw);
        let image = self.lane(Lane::Image).last().map(Step::end_us);
        let by = rect.bottom() - 46.0;
        let x1 = x(markers.or(image).unwrap_or(self.total_us)).min(right);
        painter.add(Shape::line(
            vec![
                pos2(left, by - 6.0),
                pos2(left, by),
                pos2(x1, by),
                pos2(x1, by - 6.0),
            ],
            Stroke::new(1.2, MUTED),
        ));
        let mut parts = Vec::new();
        if let Some(m) = markers {
            parts.push(match drawn {
                Some(d) => format!("markers here at {:.1} ms, drawn at {:.1}", m / 1e3, d / 1e3),
                None => format!("markers here at {:.1} ms", m / 1e3),
            });
        }
        if let Some(i) = image {
            parts.push(if s.image_complete {
                format!("picture to the GPU at {:.1} ms", i / 1e3)
            } else {
                format!("picture's last stage measured ends at {:.1} ms", i / 1e3)
            });
        }
        if parts.is_empty() {
            parts.push("no stage measured".to_owned());
        }
        painter.text(
            pos2(left, by + 4.0),
            Align2::LEFT_TOP,
            format!(
                "Frame 1, from the start of its exposure: {}",
                parts.join("; ")
            ),
            theme::sans(SMALL),
            MUTED,
        );
        if let Some(c) = &s.core1 {
            painter.text(
                pos2(left, rect.bottom() - 18.0),
                Align2::LEFT_TOP,
                core1_line(c),
                theme::sans(SMALL),
                if c.late_strips > 0 || c.frames_cut > 0 {
                    TEXT
                } else {
                    MUTED
                },
            );
        }
        if let Some(p) = response.hover_pos()
            && let Some(&(_, st)) = rows
                .iter()
                .find(|&&(ry, _)| (ry..ry + pitch).contains(&p.y))
        {
            let st = *st;
            response
                .clone()
                .on_hover_ui_at_pointer(|ui| step_tip(ui, &st, &self.shown, s.core1.as_ref()));
        }
    }
}

/// Core 1's figures in one line.
fn core1_line(c: &Core1) -> String {
    format!(
        "Core 1: {} strips of {} lines ({:.2} ms each). Per strip, longest: start {:.2} ms \
         after its block, detect {:.2}, copy wait {:.2}, held {:.2} of {:.2} allowed. Measure \
         {:.2} ms. Late strips {}, frames cut {} in the last 2 s.",
        c.strips,
        c.strip_lines,
        c.strip_us / 1e3,
        c.start_max_us / 1e3,
        c.detect_max_us / 1e3,
        c.copy_wait_max_us / 1e3,
        c.hold_max_us / 1e3,
        c.hold_budget_us / 1e3,
        c.measure_us / 1e3,
        c.late_strips,
        c.frames_cut,
    )
}

/// The figures of one stage, for a tooltip.
fn step_tip(ui: &mut Ui, st: &Step, steps: &[Step], core1: Option<&Core1>) {
    let lane = st.stage.lane();
    // The step before in its lane, or the sensor's last for a lane's first.
    let before = steps
        .iter()
        .take_while(|s| s.stage != st.stage)
        .filter(|s| s.stage.lane() == lane || s.stage.lane() == Lane::Sensor)
        .last()
        .map_or(0.0, Step::end_us);
    ui.label(theme_text(st.stage.label(), theme::strong(BODY), TEXT));
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
        Stage::Detect => {
            "Measured on the board: core 1's detector on the frame's strips, from its start on \
             the first strip (during the readout) to the release of the last"
        }
        Stage::Measure => {
            "Measured on the board: closing the frame and measuring its blobs on the assembled \
             frame, to the result's hand-off (the next frame's strips go first)"
        }
        Stage::MarkerSend => {
            "Measured: the board queuing the blobs to their arrival here (clocks matched by the \
             fastest datagram of the window)"
        }
        Stage::MarkerDraw => {
            "Measured here: the window's pass that first drew the blobs, to the renderer's \
             hand-off"
        }
    };
    ui.label(theme_text(what, theme::sans(SMALL), MUTED));
    let line = |ui: &mut Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.label(theme_text(k, theme::sans(SMALL), MUTED));
            ui.label(theme_text(&v, theme::mono(SMALL), TEXT));
        });
    };
    let ms = |us: f32| format!("{:.2} ms", us / 1e3);
    line(ui, "Median", ms(st.len_us));
    if st.measured {
        line(ui, "Slowest", ms(st.slowest_us));
    }
    if st.start_us - before > 1.0 {
        line(ui, "Waited before", ms(st.start_us - before));
    } else if before - st.start_us > 1.0 {
        line(ui, "Starts before the capture by", ms(before - st.start_us));
    }
    line(ui, "Starts at", ms(st.start_us));
    match (st.stage, core1) {
        (Stage::Detect, Some(c)) => {
            line(
                ui,
                "Strips",
                format!(
                    "{} of {} lines ({:.2} ms each)",
                    c.strips,
                    c.strip_lines,
                    c.strip_us / 1e3
                ),
            );
            line(
                ui,
                "Longest start after a strip's block",
                ms(c.start_max_us),
            );
            line(ui, "Longest detect on a strip", ms(c.detect_max_us));
            line(ui, "Longest copy wait", ms(c.copy_wait_max_us));
            line(
                ui,
                "Longest hold",
                format!("{} of {} allowed", ms(c.hold_max_us), ms(c.hold_budget_us)),
            );
            line(ui, "Late strips", c.late_strips.to_string());
            line(ui, "Frames cut", c.frames_cut.to_string());
        }
        (Stage::Measure, Some(c)) => line(ui, "Processor time (median)", ms(c.measure_us)),
        _ => {}
    }
    if matches!(st.stage, Stage::Draw | Stage::MarkerDraw) {
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
    let mut out = String::new();
    if let Some(us) = s.image_us {
        out.push_str(&format!(
            "Image {:.2} ms{}; ",
            us / 1e3,
            if s.image_complete {
                " to the GPU"
            } else {
                " (partial)"
            }
        ));
    }
    if let Some(us) = s.markers_us {
        out.push_str(&format!("Markers {:.2} ms to here", us / 1e3));
        if let Some(d) = s.markers_drawn_us {
            out.push_str(&format!(", drawn at {:.2}", d / 1e3));
        }
        out.push_str("; ");
    }
    out.push_str("steps:");
    for st in &s.steps {
        out.push_str(&format!(
            " {} {:.2} at {:.2};",
            st.stage.label(),
            st.len_us / 1e3,
            st.start_us / 1e3
        ));
    }
    out.pop();
    if let Some(c) = &s.core1 {
        out.push_str(&format!(". {}", core1_line(c)));
    }
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
