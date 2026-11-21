//! The pipeline timing diagrams, drawn from [`Snapshot`]s (one a second).
//!
//! - The ribbon (the timing strip collapsed): one row per way of the frame on one time axis in
//!   ms: the sensor's (exposure, readout), the image's and the markers', each starting where
//!   the readout feeds it. A way that is off keeps its row and says why, so the layout never
//!   changes with the settings. Each stage is a bar with its name and median above it, or
//!   below it where names would collide; the waits between stages are grey bands named
//!   "wait"; each way has its latency on the right. Under the axis, the frames that start
//!   meanwhile.
//! - The staircase (expanded): one row per stage, grouped by way, each with three sub-rows:
//!   the typical frame and the same one and two frame periods later, so the overlap of
//!   successive frames shows without bars covering each other; the ends of core 1's strips on
//!   the readout, and core 1's figures under the axis.
//!
//! Measured bars are solid, computed ones hatched; a whisker reaches to the slowest sample of
//! the window (an arrow where it leaves the axis); what follows a draw (the display) is not
//! measured, a dotted tail. Every bar and wait has a tooltip and is a node of its own for the
//! screen reader. A new snapshot and a new axis length glide into place in [`GLIDE`]; while
//! the pointer is over a diagram it holds still, so its figures and tooltips can be read.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, Color32, Galley, Layout, Painter, Rect, Role, Sense, Stroke, StrokeKind,
    TextFormat, Ui, WidgetInfo, pos2, text::LayoutJob, vec2,
};
use poc_viewer::timing::{Axis, Core1, Detection, Lane, Snapshot, Stage, Step, WINDOW};

use super::theme::{
    self, BODY, FAINT, FAULT, FIGURE, HERO, Hue, LINE, MUTED, NEUTRAL, RADIUS_SMALL, SMALL, TEXT,
    WAIT, hue,
};

/// How long a new snapshot takes to glide into place.
const GLIDE: Duration = Duration::from_millis(150);

/// The ribbon: the lane names' column, the latencies' column, a label's line, the space
/// between a label and its bar, a bar, and one row (labels above and below the bar).
const NAMES: f32 = 84.0;
const SIDE: f32 = 232.0;
const LABEL: f32 = 15.0;
const LEAD: f32 = 6.0;
const BAR: f32 = 18.0;
const ROW: f32 = LABEL + LEAD + BAR + LEAD + LABEL + 6.0;
/// Under the rows: the axis with its labels, the frame starts, the sentence on the period.
const AXIS: f32 = 22.0;
const STARTS: f32 = 18.0;
const FOOT: f32 = 18.0;
/// Height of the ribbon.
pub const RIBBON_HEIGHT: f32 = 3.0 * ROW + AXIS + STARTS + FOOT;

/// The staircase: the stage names' and medians' columns, one stage's row, a frame's bar in
/// it, a lane's heading, and the frames drawn.
const STAIR_NAMES: f32 = 96.0;
const STAIR_VALUE: f32 = 74.0;
const PITCH: f32 = 28.0;
const SUB: f32 = 7.0;
const HEADING: f32 = 22.0;
const FRAMES: usize = 3;
/// The frames' opacity: the typical frame, then one and two periods later.
const FRAME_ALPHA: [f32; FRAMES] = [1.0, 0.8, 0.62];

/// The lanes, in the order of the rows.
const LANES: [Lane; 3] = [Lane::Sensor, Lane::Image, Lane::Markers];

/// A time in ms as every figure of the timing shows it: two decimals under 10 ms, one above,
/// and "<0.01" for what would round to nothing.
pub fn ms(us: f32) -> String {
    let v = us / 1e3;
    if v != 0.0 && v.abs() < 0.005 {
        "<0.01".to_owned()
    } else if v.abs() < 10.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.1}")
    }
}

/// What the diagrams show: the latest snapshot, eased in from the one before.
#[derive(Debug, Default)]
pub struct Diagram {
    target: Option<Snapshot>,
    /// The steps shown when the target arrived.
    from: Vec<Step>,
    since: Option<Instant>,
    /// The steps shown now.
    shown: Vec<Step>,
    /// The axes of the ribbon and of the staircase, and their lengths shown, gliding from
    /// `axes_from` when the target arrived.
    ribbon_axis: Axis,
    stair_axis: Axis,
    axes_from: [f32; 2],
    axes: [f32; 2],
    /// The pointer is over a diagram: it holds still.
    hold: bool,
    /// A newer snapshot waits while the diagram holds.
    waiting: bool,
    /// Height of the staircase last drawn.
    stair_height: f32,
}

/// What a part of the ribbon under the pointer is.
#[derive(Clone, Copy)]
enum Part {
    Step(Step),
    /// The wait before a step, µs long.
    Wait(Step, f32),
}

impl Diagram {
    /// Takes `snapshot` if it is new (unless the diagram holds), and moves the steps shown
    /// towards it. Returns whether they are still moving.
    pub fn update(&mut self, snapshot: Option<&Snapshot>, now: Instant) -> bool {
        let Some(s) = snapshot else {
            *self = Self {
                stair_height: self.stair_height,
                ..Self::default()
            };
            return false;
        };
        if self.target.as_ref().is_none_or(|t| t.serial != s.serial) {
            if self.hold && self.target.is_some() {
                self.waiting = true;
            } else {
                self.take(s, now);
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
        let ease = |a: f32, b: f32| a + (b - a) * k;
        self.shown.clear();
        self.shown
            .extend(t.steps.iter().zip(&self.from).map(|(to, from)| Step {
                start_us: ease(from.start_us, to.start_us),
                len_us: ease(from.len_us, to.len_us),
                slowest_us: ease(from.slowest_us, to.slowest_us),
                tail_us: ease(from.tail_us, to.tail_us),
                ..*to
            }));
        self.axes = [
            ease(self.axes_from[0], self.ribbon_axis.len_us()),
            ease(self.axes_from[1], self.stair_axis.len_us()),
        ];
        k < 1.0
    }

    fn take(&mut self, s: &Snapshot, now: Instant) {
        let same_stages = self.shown.len() == s.steps.len()
            && self
                .shown
                .iter()
                .zip(&s.steps)
                .all(|(a, b)| a.stage == b.stage);
        self.from.clear();
        self.from
            .extend_from_slice(if same_stages { &self.shown } else { &s.steps });
        self.ribbon_axis.fit(s.total_us);
        self.stair_axis
            .fit(s.total_us + (FRAMES - 1) as f32 * s.period_us);
        self.axes_from = if self.axes[0] > 0.0 {
            self.axes
        } else {
            [self.ribbon_axis.len_us(), self.stair_axis.len_us()]
        };
        self.target = Some(s.clone());
        self.since = Some(now);
        self.waiting = false;
    }

    /// The snapshot shown, if any.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.target.as_ref()
    }

    /// What the strip's heading says about the figures: held under the pointer, or still
    /// collecting after a change.
    pub fn status(&self) -> Option<String> {
        if self.hold {
            return Some(if self.waiting {
                "Paused while the pointer is on the diagram; newer figures are waiting".to_owned()
            } else {
                "Paused while the pointer is on the diagram".to_owned()
            });
        }
        let s = self.target.as_ref()?;
        (s.collected < WINDOW).then(|| {
            format!(
                "Collecting since the change: {:.1} of {} s",
                s.collected.as_secs_f32(),
                WINDOW.as_secs()
            )
        })
    }

    /// Height of the staircase as last drawn (before the first, a typical one).
    pub fn staircase_height(&self) -> f32 {
        if self.stair_height > 0.0 {
            self.stair_height
        } else {
            420.0
        }
    }

    /// The steps shown of `lane`, in order (the steps are grouped by lane).
    fn lane(&self, lane: Lane) -> &[Step] {
        let Some(a) = self.shown.iter().position(|s| s.stage.lane() == lane) else {
            return &[];
        };
        let n = self.shown[a..]
            .iter()
            .take_while(|s| s.stage.lane() == lane)
            .count();
        &self.shown[a..a + n]
    }

    /// The end of the step shown of `stage`.
    fn end_of(&self, stage: Stage) -> Option<f32> {
        self.shown
            .iter()
            .find(|s| s.stage == stage)
            .map(Step::end_us)
    }

    /// The end of the readout: the capture, where the image and the markers start.
    fn capture(&self) -> f32 {
        self.end_of(Stage::Readout).unwrap_or(0.0)
    }

    /// A lane's latency: the figure, µs, and what it measures; `None` for a lane without
    /// steps.
    fn latency(&self, s: &Snapshot, lane: Lane) -> Option<(f32, String)> {
        match lane {
            Lane::Sensor => self
                .end_of(Stage::Readout)
                .map(|end| (end, "frame captured (end of readout)".to_owned())),
            Lane::Image => self.lane(Lane::Image).last().map(|last| {
                (
                    last.end_us(),
                    if s.image_complete {
                        "picture handed to the GPU".to_owned()
                    } else {
                        "picture's last stage measured so far".to_owned()
                    },
                )
            }),
            Lane::Markers => self.end_of(Stage::MarkerSend).map(|here| {
                (
                    here,
                    match self.end_of(Stage::MarkerDraw) {
                        Some(d) => format!("markers at this PC; drawn at {} ms", ms(d)),
                        None => "markers at this PC".to_owned(),
                    },
                )
            }),
        }
    }

    // ── Ribbon ──────────────────────────────────────────────────────────────────────────────

    /// The ribbon, as wide as the space left, [`RIBBON_HEIGHT`] tall.
    pub fn ribbon(&mut self, ui: &mut Ui) {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(vec2(width, RIBBON_HEIGHT), Sense::hover());
        self.hold = self.target.is_some() && ui.rect_contains_pointer(rect);
        let this = &*self;
        let Some(s) = &this.target else {
            ui.painter().text(
                rect.left_center(),
                Align2::LEFT_CENTER,
                "Timing appears once the board answers.",
                theme::sans(BODY),
                MUTED,
            );
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, "Pipeline timing"));
            return;
        };
        response.widget_info(|| WidgetInfo {
            label: Some("Pipeline timing".to_owned()),
            current_text_value: Some(this.summary(s)),
            ..WidgetInfo::new(Role::Figure)
        });
        let painter = ui.painter_at(rect);
        let left = rect.left() + NAMES;
        let right = rect.right() - SIDE - 24.0;
        let len = this.axes[0].max(1.0);
        let scale = (right - left) / len;
        let x = |us: f32| left + us.max(0.0) * scale;
        // Bars and whiskers stay in the plot; labels may reach a little past it.
        let plot = painter.with_clip_rect(Rect::from_min_max(
            pos2(left - 1.0, rect.top()),
            pos2(right + 1.0, rect.bottom()),
        ));

        let mut parts: Vec<(Rect, Part)> = Vec::new();
        for (i, lane) in LANES.into_iter().enumerate() {
            let top = rect.top() + i as f32 * ROW;
            let bar_y = top + LABEL + LEAD;
            painter.text(
                pos2(rect.left(), bar_y + BAR / 2.0),
                Align2::LEFT_CENTER,
                lane.name(),
                theme::sans(BODY),
                MUTED,
            );
            let steps = this.lane(lane);
            if steps.is_empty() {
                let (text, color) = idle(s, lane);
                painter.text(
                    pos2(left + 4.0, bar_y + BAR / 2.0),
                    Align2::LEFT_CENTER,
                    text,
                    theme::sans(SMALL),
                    color,
                );
            } else {
                let from = (lane != Lane::Sensor).then(|| this.capture());
                ribbon_row(
                    &painter,
                    &plot,
                    steps,
                    from,
                    bar_y,
                    (left, right),
                    &x,
                    &mut parts,
                );
            }
            if let Some((us, caption)) = this.latency(s, lane) {
                latency(
                    &painter,
                    pos2(rect.right() - SIDE, bar_y + BAR / 2.0),
                    &ms(us),
                    &caption,
                    lane != Lane::Sensor,
                );
            }
        }

        // The axis in ms, then the frames that start meanwhile and the period.
        let ay = rect.top() + 3.0 * ROW;
        painter.hline(left..=right, ay, Stroke::new(1.0, LINE));
        painter.text(
            pos2(rect.left(), ay + 3.0),
            Align2::LEFT_TOP,
            "Time, ms",
            theme::sans(SMALL),
            MUTED,
        );
        ticks(&painter, len, x, ay, right);
        let sy = ay + AXIS;
        painter.text(
            pos2(rect.left(), sy + 1.0),
            Align2::LEFT_TOP,
            "Frame starts",
            theme::sans(SMALL),
            MUTED,
        );
        if s.period_us > 0.0 {
            let every = (18.0 / (s.period_us * scale)).ceil().max(1.0) as usize;
            let mut k = 0usize;
            while k < 1000 && k as f32 * s.period_us <= len {
                let fx = x(k as f32 * s.period_us);
                painter.vline(fx, (sy + 2.0)..=(sy + 12.0), Stroke::new(1.0, FAINT));
                if k.is_multiple_of(every) {
                    painter.text(
                        pos2(fx + 3.0, sy + 1.0),
                        Align2::LEFT_TOP,
                        (k + 1).to_string(),
                        theme::mono(SMALL),
                        FAINT,
                    );
                }
                k += 1;
            }
        }
        painter.text(
            pos2(left, sy + STARTS + 1.0),
            Align2::LEFT_TOP,
            this.period_sentence(s),
            theme::sans(SMALL),
            MUTED,
        );

        for (i, (r, part)) in parts.into_iter().enumerate() {
            let resp = ui.interact(r, response.id.with(("part", i)), Sense::hover());
            resp.widget_info(|| part_info(&part));
            let steps = &this.shown;
            resp.on_hover_ui(|ui| match part {
                Part::Step(st) => step_tip(ui, &st, steps, s.core1.as_ref(), None),
                Part::Wait(next, us) => wait_tip(ui, &next, us),
            });
        }
    }

    /// The footer's sentence: the period, and how many of them the way takes.
    fn period_sentence(&self, s: &Snapshot) -> String {
        if s.period_us <= 0.0 {
            return String::new();
        }
        let mut out = format!(
            "A frame starts every {} ms ({:.1} Hz).",
            ms(s.period_us),
            1e6 / s.period_us
        );
        let periods = |us: f32| us / s.period_us;
        if let Some(end) = self.lane(Lane::Image).last().map(Step::end_us)
            && s.image_complete
        {
            out.push_str(&format!(
                " The picture reaches the GPU {:.1} frame periods after its exposure starts, so \
                 that many frames are on their way at once.",
                periods(end)
            ));
        } else if let Some(here) = self.end_of(Stage::MarkerSend) {
            out.push_str(&format!(
                " The markers reach this PC {:.1} frame periods after the exposure starts.",
                periods(here)
            ));
        }
        out
    }

    /// The ribbon in a few sentences, for the tools and the screen reader (each bar and wait
    /// is a node of its own).
    fn summary(&self, s: &Snapshot) -> String {
        let mut out = Vec::new();
        for lane in LANES {
            match self.latency(s, lane) {
                Some((us, what)) => out.push(format!("{what}: {} ms", ms(us))),
                None => out.push(format!("{}: {}", lane.name(), idle(s, lane).0)),
            }
        }
        out.push(format!("a frame starts every {} ms", ms(s.period_us)));
        out.join("; ")
    }

    // ── Staircase ───────────────────────────────────────────────────────────────────────────

    /// The staircase, as wide as the space left; as tall as its rows need
    /// ([`Diagram::staircase_height`]).
    pub fn staircase(&mut self, ui: &mut Ui) {
        let width = ui.available_width();
        // Taken out while drawing, so the diagram's own fields can be set meanwhile.
        let Some(snap) = self.target.take() else {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
            ui.painter().text(
                rect.left_center(),
                Align2::LEFT_CENTER,
                "Timing appears once the board answers.",
                theme::sans(BODY),
                MUTED,
            );
            self.hold = false;
            return;
        };
        let s = &snap;
        let painter = ui.painter().clone();
        let headline = painter.layout_job(self.headline(s, width));
        let left = STAIR_NAMES + STAIR_VALUE + 16.0;
        let notes: Vec<Arc<Galley>> = [s.core1.as_ref().map(core1_job), self.presented(s)]
            .into_iter()
            .flatten()
            .map(|mut job| {
                job.wrap.max_width = width - left;
                painter.layout_job(job)
            })
            .collect();
        let rows_h: f32 = LANES
            .iter()
            .map(|&l| HEADING + self.lane(l).len().max(1) as f32 * PITCH)
            .sum();
        let head = headline.size().y + 10.0 + 20.0;
        let height = head + rows_h + AXIS + notes.iter().map(|g| g.size().y + 4.0).sum::<f32>();
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        self.stair_height = height;
        self.hold = ui.rect_contains_pointer(rect);
        let this = &*self;
        response.widget_info(|| WidgetInfo {
            label: Some("Pipeline timing by stage and frame".to_owned()),
            current_text_value: Some(format!(
                "{} Three frames, {} ms apart: the typical frame and the same one one and two \
                 frame periods later.",
                headline.text(),
                ms(s.period_us)
            )),
            ..WidgetInfo::new(Role::Figure)
        });
        let painter = painter.with_clip_rect(rect);
        painter.galley(rect.left_top(), headline.clone(), TEXT);

        let left = rect.left() + left;
        let right = rect.right() - 8.0;
        let len = this.axes[1].max(1.0);
        let scale = (right - left) / len;
        let x = |us: f32| left + us.max(0.0) * scale;
        let top = rect.top() + headline.size().y + 10.0;
        let rows_top = top + 20.0;
        let bottom = rows_top + rows_h;

        // The grid, the frames' headings and the medians' heading.
        let tick = Axis::tick_us(len);
        let mut i = 0;
        while i as f32 * tick <= len * 1.0001 {
            let gx = x(i as f32 * tick);
            painter.vline(gx, rows_top..=bottom, Stroke::new(1.0, LINE));
            // The last label ends at the axis' end, so the strip's edge does not cut it.
            let last = (i + 1) as f32 * tick > len * 1.0001;
            painter.text(
                pos2(gx, bottom + 4.0),
                if last {
                    Align2::RIGHT_TOP
                } else {
                    Align2::CENTER_TOP
                },
                trim(i as f32 * tick / 1e3),
                theme::mono(SMALL),
                MUTED,
            );
            i += 1;
        }
        painter.text(
            pos2(rect.left(), bottom + 4.0),
            Align2::LEFT_TOP,
            "Time, ms",
            theme::sans(SMALL),
            MUTED,
        );
        painter.text(
            pos2(rect.left() + STAIR_NAMES + STAIR_VALUE, top + 2.0),
            Align2::RIGHT_TOP,
            "Median",
            theme::sans(SMALL),
            MUTED,
        );
        let room = s.period_us * scale;
        for (k, alpha) in FRAME_ALPHA.into_iter().enumerate() {
            let fx = x(k as f32 * s.period_us);
            if fx > right - 8.0 {
                break;
            }
            let text = if room >= 60.0 {
                format!("Frame {}", k + 1)
            } else {
                format!("{}", k + 1)
            };
            painter.vline(fx, (top + 2.0)..=(rows_top - 2.0), Stroke::new(1.0, MUTED));
            painter.text(
                pos2(fx + 3.0, top + 2.0),
                Align2::LEFT_TOP,
                text,
                theme::sans(SMALL),
                theme::fade(MUTED, alpha.max(0.75)),
            );
        }

        // The rows: each lane under its heading.
        let mut rows: Vec<(f32, Step)> = Vec::with_capacity(this.shown.len());
        let mut y = rows_top;
        for lane in LANES {
            painter.text(
                pos2(rect.left(), y + HEADING - 4.0),
                Align2::LEFT_BOTTOM,
                lane.name(),
                theme::strong(SMALL),
                MUTED,
            );
            painter.hline(
                (rect.left() + 64.0)..=right,
                y + HEADING / 2.0 + 1.0,
                Stroke::new(1.0, LINE),
            );
            y += HEADING;
            let steps = this.lane(lane);
            if steps.is_empty() {
                let (text, color) = idle(s, lane);
                painter.text(
                    pos2(left + 4.0, y + PITCH / 2.0),
                    Align2::LEFT_CENTER,
                    text,
                    theme::sans(SMALL),
                    color,
                );
                y += PITCH;
                continue;
            }
            for st in steps {
                rows.push((y, *st));
                y += PITCH;
            }
        }
        let plot = painter.with_clip_rect(Rect::from_min_max(
            pos2(left - 1.0, rect.top()),
            pos2(right + 1.0, rect.bottom()),
        ));
        for &(ry, st) in &rows {
            let cy = ry + PITCH / 2.0;
            painter.text(
                pos2(rect.left(), cy),
                Align2::LEFT_CENTER,
                st.stage.name(),
                theme::sans(BODY),
                TEXT,
            );
            painter.text(
                pos2(rect.left() + STAIR_NAMES + STAIR_VALUE, cy),
                Align2::RIGHT_CENTER,
                format!("{} ms", ms(st.len_us)),
                theme::mono(SMALL),
                MUTED,
            );
            let h = hue(st.stage);
            for (k, alpha) in FRAME_ALPHA.into_iter().enumerate() {
                let offset = k as f32 * s.period_us;
                let x0 = x(st.start_us + offset);
                if x0 > right {
                    continue;
                }
                let by = sub_row(ry, k);
                let r = Rect::from_min_max(
                    pos2(x0, by),
                    pos2((x(st.end_us() + offset)).max(x0 + 3.0), by + SUB),
                );
                if st.measured && st.slowest_us > st.len_us * 1.02 {
                    whisker(
                        &plot,
                        r.right(),
                        x(st.start_us + offset + st.slowest_us),
                        r.center().y,
                        theme::fade(h.line, alpha),
                        right,
                        2.5,
                    );
                }
                bar(&plot, r, h, st.measured, alpha);
                tail_mark(&plot, r, &st, x, h, alpha);
                // The strips' ends on the readout, as detection takes them.
                if st.stage == Stage::Readout
                    && let Some(c) = &s.core1
                    && c.strips > 1
                    && c.strip_us * scale >= 3.0
                {
                    let line = Stroke::new(1.0, theme::fade(hue(Stage::Detect).line, alpha));
                    for i in 1..c.strips {
                        let sx = x(st.start_us + offset + f32::from(i) * c.strip_us);
                        if sx >= r.right() - 1.0 {
                            break;
                        }
                        plot.vline(sx, (r.top() - 1.0)..=(r.bottom() + 1.0), line);
                    }
                }
            }
        }
        let mut ny = bottom + AXIS;
        for g in notes {
            let h = g.size().y;
            painter.galley(pos2(left, ny), g, TEXT);
            ny += h + 4.0;
        }

        // Each row is a node, and its tooltip names the frame under the pointer.
        for (i, &(ry, st)) in rows.iter().enumerate() {
            let r = Rect::from_min_max(pos2(rect.left(), ry), pos2(right, ry + PITCH));
            let resp = ui.interact(r, response.id.with(("row", i)), Sense::hover());
            resp.widget_info(|| part_info(&Part::Step(st)));
            let frame = resp.hover_pos().map(|p| {
                let k = if p.x < left {
                    0
                } else {
                    (0..FRAMES)
                        .find(|&k| p.y < sub_row(ry, k) + SUB + 1.0)
                        .unwrap_or(FRAMES - 1)
                };
                (k, s.period_us)
            });
            let steps = &this.shown;
            resp.on_hover_ui(|ui| step_tip(ui, &st, steps, s.core1.as_ref(), frame));
        }
        self.target = Some(snap);
    }

    /// The staircase's heading: each way's latency in one line.
    fn headline(&self, s: &Snapshot, width: f32) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.wrap.max_width = width;
        let words = TextFormat::simple(theme::sans(SMALL), MUTED);
        let figure = TextFormat::simple(theme::mono_strong(FIGURE), TEXT);
        for (i, lane) in [Lane::Image, Lane::Markers, Lane::Sensor]
            .into_iter()
            .enumerate()
        {
            if i > 0 {
                job.append("   ·   ", 0.0, words.clone());
            }
            match self.latency(s, lane) {
                Some((us, what)) => {
                    let (what, after) = match what.split_once("; ") {
                        Some((a, b)) => (a.to_owned(), format!(", {b}")),
                        None => (what, String::new()),
                    };
                    job.append(&format!("{} ", capitalise(&what)), 0.0, words.clone());
                    job.append(&ms(us), 0.0, figure.clone());
                    job.append(&format!(" ms{after}"), 0.0, words.clone());
                }
                None => job.append(
                    &format!("{}: {}", lane.name(), idle(s, lane).0),
                    0.0,
                    words.clone(),
                ),
            }
        }
        job
    }

    /// A note when this PC presents fewer frames than the sensor makes: frames 2 and 3 are
    /// where the next frames would be, not frames that were all sent.
    fn presented(&self, s: &Snapshot) -> Option<LayoutJob> {
        let secs = s.collected.as_secs_f32();
        if !s.lanes.frames || s.period_us <= 0.0 || secs < 0.5 || self.lane(Lane::Image).is_empty()
        {
            return None;
        }
        let rate = 1e6 / s.period_us;
        let shown = s.host_frames as f32 / secs;
        (shown < rate * 0.9).then(|| {
            let mut job = LayoutJob::default();
            job.append(
                &format!(
                    "This PC presents {shown:.1} of the {rate:.1} frames per second; frames 2 and \
                     3 show when the next frames would go, not frames that all reach this PC."
                ),
                0.0,
                TextFormat::simple(theme::sans(SMALL), MUTED),
            );
            job
        })
    }

    // ── Key ─────────────────────────────────────────────────────────────────────────────────

    /// What the marks mean, in one line of samples; the staircase's own marks when
    /// `expanded`.
    pub fn key(&self, ui: &mut Ui, expanded: bool) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            let sample = |ui: &mut Ui, draw: &dyn Fn(&Painter, Rect), text: &str| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 14.0), Sense::hover());
                draw(ui.painter(), r.shrink2(vec2(0.0, 2.0)));
                ui.label(theme_text(text, theme::sans(SMALL), MUTED));
                ui.add_space(10.0);
            };
            sample(ui, &|p, r| bar(p, r, NEUTRAL, true, 1.0), "measured");
            sample(
                ui,
                &|p, r| bar(p, r, NEUTRAL, false, 1.0),
                "computed from the mode",
            );
            if !expanded {
                sample(ui, &|p, r| wait_band(p, r), "wait");
            }
            sample(
                ui,
                &|p, r| {
                    let y = r.center().y;
                    p.rect_filled(
                        Rect::from_min_max(r.min, pos2(r.left() + 8.0, r.bottom())),
                        RADIUS_SMALL,
                        NEUTRAL.fill,
                    );
                    whisker(
                        p,
                        r.left() + 8.0,
                        r.right() - 1.0,
                        y,
                        NEUTRAL.line,
                        r.right(),
                        3.5,
                    );
                },
                "slowest frame",
            );
            if expanded {
                sample(
                    ui,
                    &|p, r| {
                        bar(p, r, hue(Stage::Readout), false, 1.0);
                        for i in 1..3 {
                            p.vline(
                                r.left() + i as f32 * r.width() / 3.0,
                                r.y_range(),
                                Stroke::new(1.0, hue(Stage::Detect).line),
                            );
                        }
                    },
                    "end of a strip of core 1",
                );
                ui.label(theme_text(
                    "Each stage's three rows: the typical frame, then one and two frame periods \
                     later.",
                    theme::sans(SMALL),
                    MUTED,
                ));
            } else {
                sample(
                    ui,
                    &|p, r| not_measured(p, r.left(), r.right(), r.center().y),
                    "after the draw: not measured",
                );
            }
        });
    }
}

/// Why a lane has no steps, and the colour to say it in.
fn idle(s: &Snapshot, lane: Lane) -> (&'static str, Color32) {
    match lane {
        Lane::Sensor => ("Waiting for the board's mode", MUTED),
        Lane::Image if !s.lanes.frames => ("Frames off: the board sends no picture", MUTED),
        Lane::Image => ("Waiting for a frame the board timed", MUTED),
        Lane::Markers => match s.lanes.detection {
            Detection::Off => ("Detection off", MUTED),
            Detection::Unavailable => ("No detection in this mode (whole frames only)", MUTED),
            Detection::Fault => ("Detection stopped by a fault", FAULT),
            Detection::On => ("Waiting for blob frames", MUTED),
        },
    }
}

/// One ribbon row: the waits (from `from`, the capture, for the image and the markers), the
/// whiskers, the bars, the tail after a draw, and the labels with their leaders. Adds what
/// the pointer can point at to `parts`.
#[expect(clippy::too_many_arguments)]
fn ribbon_row(
    painter: &Painter,
    plot: &Painter,
    steps: &[Step],
    from: Option<f32>,
    bar_y: f32,
    (left, right): (f32, f32),
    x: &impl Fn(f32) -> f32,
    parts: &mut Vec<(Rect, Part)>,
) {
    let band =
        |x0: f32, x1: f32| Rect::from_min_max(pos2(x0, bar_y), pos2(x1.max(x0 + 4.0), bar_y + BAR));
    // Waits: grey bands, named where they are wide enough beside the whisker of the step
    // before (which reaches into them).
    let mut prev: Option<(f32, f32)> = from.map(|f| (f, f));
    for st in steps {
        if let Some((end, slowest_end)) = prev
            && st.start_us - end > 1.0
        {
            let r = Rect::from_min_max(
                pos2(x(end) + 1.0, bar_y + 1.0),
                pos2(x(st.start_us) - 1.0, bar_y + BAR - 1.0),
            );
            if r.width() >= 1.0 {
                wait_band(plot, r);
                let g = name_value(painter, "wait", &ms(st.start_us - end));
                let lo = r.left().max(x(slowest_end) + 4.0);
                if g.size().x + 10.0 <= r.right() - lo {
                    let cx = (lo + r.right()) / 2.0;
                    plot.galley(pos2(cx, r.center().y) - g.size() / 2.0, g, TEXT);
                }
                parts.push((
                    r.expand2(vec2(0.0, 1.0)),
                    Part::Wait(*st, st.start_us - end),
                ));
            }
        }
        let slowest_end = if st.measured && st.slowest_us > st.len_us * 1.02 {
            st.start_us + st.slowest_us
        } else {
            st.end_us()
        };
        prev = Some((st.end_us(), slowest_end));
    }
    let bars: Vec<Rect> = steps
        .iter()
        .map(|st| band(x(st.start_us) + 0.5, x(st.end_us()) - 0.5))
        .collect();
    for (st, r) in steps.iter().zip(&bars) {
        if st.measured && st.slowest_us > st.len_us * 1.02 {
            whisker(
                plot,
                r.right(),
                x(st.start_us + st.slowest_us),
                r.center().y,
                hue(st.stage).line,
                right,
                3.5,
            );
        }
    }
    for (st, r) in steps.iter().zip(&bars) {
        let h = hue(st.stage);
        bar(plot, *r, h, st.measured, 1.0);
        tail_mark(plot, *r, st, x, h, 1.0);
        parts.push((r.expand2(vec2(2.0, 0.0)), Part::Step(*st)));
    }
    if let (Some(last), Some(r)) = (steps.last(), bars.last())
        && matches!(last.stage, Stage::Draw | Stage::MarkerDraw)
    {
        // After the whisker, if there is one.
        let after = if last.measured {
            x(last.start_us + last.slowest_us).max(r.right())
        } else {
            r.right()
        };
        not_measured(plot, after + 4.0, after + 23.0, r.center().y);
    }

    // Labels: centred over their bar, else under it, else where they fit; a leader from each
    // to its bar.
    let lo = left - 8.0;
    let hi = right + 20.0;
    let mut tier_right = [f32::NEG_INFINITY; 2];
    for (st, r) in steps.iter().zip(&bars) {
        let g = name_value(painter, st.stage.name(), &ms(st.len_us));
        let w = g.size().x;
        let cx = r.center().x;
        let ideal = (cx - w / 2.0).clamp(lo, (hi - w).max(lo));
        let shift = |t: usize| ideal.max(tier_right[t] + 10.0) - ideal;
        let t = if shift(0) <= (w / 2.0).min(12.0) || shift(0) <= shift(1) {
            0
        } else {
            1
        };
        let at = ideal.max(tier_right[t] + 10.0);
        tier_right[t] = at + w;
        let y = if t == 0 {
            bar_y - LEAD - LABEL
        } else {
            bar_y + BAR + LEAD
        };
        let lx = cx.clamp(at + 3.0, at + w - 3.0);
        let (ly, by) = if t == 0 {
            (y + LABEL - 1.0, bar_y - 1.0)
        } else {
            (y + 1.0, bar_y + BAR + 1.0)
        };
        painter.line_segment(
            [pos2(lx, ly), pos2(cx, by)],
            Stroke::new(1.0, theme::fade(hue(st.stage).line, 0.8)),
        );
        painter.galley(pos2(at, y), g, TEXT);
    }
}

/// A lane's latency in the right column: the figure (large for a way's end), "ms", and what
/// it measures under it. `at`: the column's left at the bar's middle.
fn latency(painter: &Painter, at: eframe::egui::Pos2, figure: &str, caption: &str, large: bool) {
    let font = theme::mono_strong(if large { HERO } else { FIGURE });
    let g = painter.layout_no_wrap(figure.to_owned(), font, TEXT);
    // Right-aligned on one edge, so the figures line up.
    let num_right = at.x + 92.0;
    let num = pos2(num_right - g.size().x, at.y - g.size().y / 2.0 - 4.0);
    let gh = g.size().y;
    painter.galley(num, g, TEXT);
    painter.text(
        pos2(num_right + 4.0, num.y + gh - 6.0),
        Align2::LEFT_BOTTOM,
        "ms",
        theme::sans(SMALL),
        MUTED,
    );
    painter.text(
        pos2(at.x, num.y + gh - 2.0),
        Align2::LEFT_TOP,
        caption,
        theme::sans(SMALL),
        MUTED,
    );
}

/// The axis' ticks in ms under `ay`, a round step apart.
fn ticks(painter: &Painter, len: f32, x: impl Fn(f32) -> f32, ay: f32, right: f32) {
    let tick = Axis::tick_us(len);
    let mut i = 0;
    while i as f32 * tick <= len * 1.0001 {
        let tx = x(i as f32 * tick).min(right);
        painter.vline(tx, ay..=(ay + 4.0), Stroke::new(1.0, MUTED));
        painter.text(
            pos2(tx, ay + 5.0),
            Align2::CENTER_TOP,
            trim(i as f32 * tick / 1e3),
            theme::mono(SMALL),
            MUTED,
        );
        i += 1;
    }
}

/// The top of frame `k`'s bar in a staircase row starting at `ry`.
fn sub_row(ry: f32, k: usize) -> f32 {
    ry + (PITCH - (FRAMES as f32 * SUB + (FRAMES - 1) as f32 * 1.5)) / 2.0 + k as f32 * (SUB + 1.5)
}

/// A part's name and figures for the screen reader.
fn part_info(part: &Part) -> WidgetInfo {
    let (label, value) = match part {
        Part::Step(st) => (
            st.stage.label().to_owned(),
            if st.measured {
                format!(
                    "median {} ms, slowest {} ms, starts at {} ms",
                    ms(st.len_us),
                    ms(st.slowest_us),
                    ms(st.start_us)
                )
            } else {
                format!(
                    "{} ms, computed from the mode, starts at {} ms",
                    ms(st.len_us),
                    ms(st.start_us)
                )
            },
        ),
        Part::Wait(next, us) => (
            format!("Wait before {}", next.stage.label()),
            format!("median {} ms", ms(*us)),
        ),
    };
    // A list item keeps both: a label's name would be overwritten by its value.
    WidgetInfo {
        label: Some(label),
        current_text_value: Some(value),
        ..WidgetInfo::new(Role::ListItem)
    }
}

/// Core 1's figures of the window, problems in red.
fn core1_job(c: &Core1) -> LayoutJob {
    let mut job = LayoutJob::default();
    let words = TextFormat::simple(theme::sans(SMALL), MUTED);
    let value = |bad: bool| TextFormat::simple(theme::mono(SMALL), if bad { FAULT } else { TEXT });
    let mut put = |text: &str, v: Option<(String, bool)>| {
        job.append(text, 0.0, words.clone());
        if let Some((v, bad)) = v {
            job.append(&v, 0.0, value(bad));
        }
    };
    put(
        "Core 1, the longest per strip in the last 2 s: start ",
        Some((ms(c.start_max_us), false)),
    );
    put(
        " ms after the strip is read out · detect ",
        Some((ms(c.detect_max_us), false)),
    );
    put(
        " ms · wait for its copy ",
        Some((ms(c.copy_wait_max_us), false)),
    );
    put(
        " ms · held ",
        Some((ms(c.hold_max_us), c.hold_max_us > c.hold_budget_us)),
    );
    put(" ms of ", Some((ms(c.hold_budget_us), false)));
    put(" ms allowed.\n", None);
    put(
        &format!("{} ", plural(u32::from(c.strips), "strip", "strips")),
        None,
    );
    put("of ", Some((c.strip_lines.to_string(), false)));
    put(" lines, ", Some((ms(c.strip_us), false)));
    put(" ms each · measure ", Some((ms(c.measure_us), false)));
    put(
        " ms per frame · strips held too long ",
        Some((c.late_strips.to_string(), c.late_strips > 0)),
    );
    put(
        " · frames cut short ",
        Some((c.frames_cut.to_string(), c.frames_cut > 0)),
    );
    put(".", None);
    job
}

/// `n` and the noun that goes with it.
fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// A tooltip's lines: names on the left, figures in a column on the right.
fn tip_grid(ui: &mut Ui, id: &str, rows: &[(String, String, Color32)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([18.0, 3.0])
        .show(ui, |ui| {
            for (k, v, c) in rows {
                ui.label(theme_text(k, theme::sans(SMALL), MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(theme_text(v, theme::mono(SMALL), *c));
                });
                ui.end_row();
            }
        });
}

/// A tooltip's set-up: a readable width and tight lines.
fn tip_style(ui: &mut Ui) {
    ui.set_max_width(400.0);
    let s = ui.spacing_mut();
    s.interact_size.y = 0.0;
    s.item_spacing.y = 4.0;
}

/// The figures of one stage, for a tooltip; `frame`: which of the staircase's frames (and the
/// period), `None` for the ribbon's typical frame.
fn step_tip(
    ui: &mut Ui,
    st: &Step,
    steps: &[Step],
    core1: Option<&Core1>,
    frame: Option<(usize, f32)>,
) {
    tip_style(ui);
    let lane = st.stage.lane();
    // The step before in its lane, or the sensor's last for a lane's first.
    let before = steps
        .iter()
        .take_while(|s| s.stage != st.stage)
        .filter(|s| s.stage.lane() == lane || s.stage.lane() == Lane::Sensor)
        .last()
        .map_or(0.0, Step::end_us);
    let capture = steps
        .iter()
        .find(|s| s.stage == Stage::Readout)
        .map_or(0.0, Step::end_us);
    ui.label(theme_text(st.stage.label(), theme::strong(BODY), TEXT));
    let (offset, which) = match frame {
        Some((0, _)) | None => (
            0.0,
            "The typical frame: medians of the last 2 s. Times from the start of its exposure."
                .to_owned(),
        ),
        Some((k, period)) => (
            k as f32 * period,
            format!(
                "Frame {}: the typical frame, {} later. Times from the start of frame 1's \
                 exposure.",
                k + 1,
                plural(k as u32, "frame period", "frame periods")
            ),
        ),
    };
    ui.label(theme_text(&which, theme::sans(SMALL), MUTED));
    let what = match st.stage {
        Stage::Exposure => {
            "Computed from the mode: exposure lines × line time, right before \
                            the readout."
        }
        Stage::Readout => {
            "Computed from the mode: lines read × line time. It ends when the \
                           board has the whole frame (the capture). In the expanded view, ticks \
                           on it mark where core 1's strips end."
        }
        Stage::Encode => {
            "Measured on the board: the hardware JPEG encode, from its first rows to the end of \
             the file. Fed strip by strip it starts during the readout; the line across the bar \
             marks when the frame's last rows were handed to it."
        }
        Stage::Send => {
            "Measured: from the board taking the frame to its last chunk arriving at this PC. \
             The two clocks are matched on the fastest datagram of the last 2 s, so this reads \
             short by that datagram's own transit."
        }
        Stage::Decode => "Measured on this PC: the JPEG decode.",
        Stage::Draw => {
            "Measured on this PC: the window's pass that took the picture, to the end of its \
             upload to the GPU."
        }
        Stage::Detect => {
            "Measured on the board: core 1's detector on the frame's strips, from its start on \
             the first strip (during the readout) to the release of the last."
        }
        Stage::Measure => {
            "Measured on the board: closing the frame and measuring its blobs on the assembled \
             frame, to handing the result to core 0. The next frame's strips go first."
        }
        Stage::MarkerSend => {
            "Measured: from the board queuing the blobs to their arrival at this PC (clocks \
             matched as for the picture's send)."
        }
        Stage::MarkerDraw => {
            "Measured on this PC: the window's first pass that drew the blobs, to the \
             renderer's hand-off. With frames on they wait for their picture, so this ends with \
             the picture's draw."
        }
    };
    ui.label(theme_text(what, theme::sans(SMALL), MUTED));
    let row = |k: &str, v: String| (k.to_owned(), v, TEXT);
    let mut rows = Vec::new();
    if st.measured {
        rows.push(row("Median length", format!("{} ms", ms(st.len_us))));
        rows.push(row("Slowest", format!("{} ms", ms(st.slowest_us))));
    } else {
        rows.push(row("Length", format!("{} ms", ms(st.len_us))));
    }
    rows.push(row("Starts at", format!("{} ms", ms(st.start_us + offset))));
    if lane != Lane::Sensor && capture - st.start_us > 1.0 {
        rows.push(row(
            "Starts before the capture by",
            format!("{} ms", ms(capture - st.start_us)),
        ));
    }
    if st.start_us - before > 1.0 {
        rows.push(row(
            "Waited before",
            format!("{} ms", ms(st.start_us - before)),
        ));
    }
    if st.tail_us > 1.0 {
        rows.push(row(
            "Encoding after the last rows",
            format!("{} ms", ms(st.tail_us)),
        ));
    }
    match (st.stage, core1) {
        (Stage::Detect, Some(c)) => {
            rows.push(row(
                "Strips",
                format!(
                    "{} × {} lines, {} ms each",
                    c.strips,
                    c.strip_lines,
                    ms(c.strip_us)
                ),
            ));
            rows.push(row(
                "Slowest start after a strip is read out",
                format!("{} ms", ms(c.start_max_us)),
            ));
            rows.push(row(
                "Slowest detect of a strip",
                format!("{} ms", ms(c.detect_max_us)),
            ));
            rows.push(row(
                "Slowest wait for a strip's copy",
                format!("{} ms", ms(c.copy_wait_max_us)),
            ));
            rows.push((
                "Longest hold of a strip".to_owned(),
                format!(
                    "{} of {} ms allowed",
                    ms(c.hold_max_us),
                    ms(c.hold_budget_us)
                ),
                if c.hold_max_us > c.hold_budget_us {
                    FAULT
                } else {
                    TEXT
                },
            ));
            rows.push((
                "Strips held too long".to_owned(),
                c.late_strips.to_string(),
                if c.late_strips > 0 { FAULT } else { TEXT },
            ));
            rows.push((
                "Frames cut short".to_owned(),
                c.frames_cut.to_string(),
                if c.frames_cut > 0 { FAULT } else { TEXT },
            ));
        }
        (Stage::Measure, Some(c)) => {
            rows.push(row(
                "Processor time, median",
                format!("{} ms", ms(c.measure_us)),
            ));
        }
        _ => {}
    }
    tip_grid(ui, "step tip", &rows);
    if matches!(st.stage, Stage::Draw | Stage::MarkerDraw) {
        ui.label(theme_text(
            "From here to the display (vsync, compositor, scan-out) is not measured.",
            theme::sans(SMALL),
            MUTED,
        ));
    }
}

/// The figures of the wait before `next`, `us` long, for a tooltip.
fn wait_tip(ui: &mut Ui, next: &Step, us: f32) {
    tip_style(ui);
    ui.label(theme_text(
        &format!("Wait before {}", next.stage.label()),
        theme::strong(BODY),
        TEXT,
    ));
    let why = match next.stage {
        Stage::Encode => "The frame waiting for the encoder.",
        Stage::Send => "The frame waiting to be sent: the link still carries the frame before.",
        Stage::Decode => {
            "The frame waiting for the decoding stage, which decodes the newest frame once the \
             window took the picture before."
        }
        Stage::Draw => "The picture waiting for the window's next pass (one per display refresh).",
        Stage::MarkerSend => {
            "The blobs waiting for core 0, which takes core 1's results every millisecond, and \
             for its network side."
        }
        Stage::MarkerDraw => {
            "The blobs waiting for the window's next pass; with frames on, for their picture."
        }
        _ => "Time between the stage before and this one.",
    };
    ui.label(theme_text(why, theme::sans(SMALL), MUTED));
    tip_grid(
        ui,
        "wait tip",
        &[("Median wait".to_owned(), format!("{} ms", ms(us)), TEXT)],
    );
}

fn theme_text(s: &str, font: eframe::egui::FontId, c: Color32) -> eframe::egui::RichText {
    eframe::egui::RichText::new(s).font(font).color(c)
}

/// A name in words and a figure in mono, as one label.
fn name_value(painter: &Painter, name: &str, value: &str) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(name, 0.0, TextFormat::simple(theme::sans(SMALL), MUTED));
    job.append(value, 5.0, TextFormat::simple(theme::mono(SMALL), TEXT));
    painter.layout_job(job)
}

/// A tick label: whole numbers without decimals.
fn trim(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// A stage's bar: solid when measured, hatched when computed.
fn bar(painter: &Painter, r: Rect, h: Hue, measured: bool, alpha: f32) {
    if measured {
        painter.rect(
            r,
            RADIUS_SMALL,
            theme::fade(h.fill, alpha),
            Stroke::new(1.0, theme::fade(h.line, alpha)),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(r, RADIUS_SMALL, theme::fade(h.fill, 0.45 * alpha));
        let clip = painter.with_clip_rect(r.shrink(1.0).intersect(painter.clip_rect()));
        let stroke = Stroke::new(1.0, theme::fade(h.line, 0.6 * alpha));
        let mut hx = r.left() - r.height();
        while hx < r.right() {
            clip.line_segment(
                [pos2(hx, r.bottom()), pos2(hx + r.height(), r.top())],
                stroke,
            );
            hx += 5.0;
        }
        painter.rect_stroke(
            r,
            RADIUS_SMALL,
            Stroke::new(1.0, theme::fade(h.line, alpha)),
            StrokeKind::Inside,
        );
    }
}

/// A wait: a flat grey band.
fn wait_band(painter: &Painter, r: Rect) {
    painter.rect_filled(r, RADIUS_SMALL, WAIT.fill);
}

/// What follows a draw: dotted, unmeasured.
fn not_measured(painter: &Painter, x0: f32, x1: f32, y: f32) {
    let mut x = x0;
    while x < x1 {
        painter.circle_filled(pos2(x, y), 1.0, FAINT);
        x += 4.0;
    }
}

/// The start of a stage's tail ([`Step::tail_us`]: the encode after the frame's last rows) in
/// its bar `r`, a line across it; nothing when it has none or the tail is the whole bar.
fn tail_mark(painter: &Painter, r: Rect, st: &Step, x: impl Fn(f32) -> f32, h: Hue, alpha: f32) {
    if st.tail_us < 1.0 || st.tail_us >= st.len_us - 1.0 {
        return;
    }
    let tx = x(st.start_us + st.len_us - st.tail_us) + (r.left() - x(st.start_us));
    if tx <= r.left() + 2.0 || tx >= r.right() - 2.0 {
        return;
    }
    painter.vline(
        tx,
        (r.top() + 1.0)..=(r.bottom() - 1.0),
        Stroke::new(1.5, theme::fade(h.text, alpha)),
    );
}

/// From the median's end `x0` to the slowest sample's end `x1`, with a cap; an arrow at
/// `right` where it leaves the axis. `cap`: half the cap's height.
fn whisker(painter: &Painter, x0: f32, x1: f32, y: f32, c: Color32, right: f32, cap: f32) {
    if x1 - x0 < 2.0 || x0 >= right {
        return;
    }
    let stroke = Stroke::new(1.2, c);
    let end = x1.min(right);
    painter.hline(x0..=end, y, stroke);
    if x1 > right {
        painter.line_segment([pos2(end - cap, y - cap), pos2(end, y)], stroke);
        painter.line_segment([pos2(end - cap, y + cap), pos2(end, y)], stroke);
    } else {
        painter.vline(end, (y - cap)..=(y + cap), stroke);
    }
}
