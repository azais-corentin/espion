//! The pipeline timing diagram (the ribbon), drawn from [`Snapshot`]s (one a second).
//!
//! One row per way of the frame on one time axis in ms: the sensor's (exposure, readout), the
//! image's and the markers', each starting where the readout feeds it. A way that is off keeps
//! its row and says why, so the layout never changes with the settings. Each stage is a bar
//! with its name and median above it, or below it where names would collide; the waits
//! between stages are grey bands named "wait"; each way has its latency on the right. Under
//! the axis, the frames that start meanwhile.
//!
//! Measured bars are solid, computed ones hatched; a whisker reaches to the slowest sample of
//! the window (an arrow where it leaves the axis); what follows a draw (the display) is not
//! measured, a dotted tail. Every bar and wait has a tooltip and is a node of its own for the
//! screen reader. While the pointer is over the diagram, a cursor line follows it with its
//! time on the axis. A new snapshot and a new axis length glide into place in [`GLIDE`]; while
//! the pointer is over the diagram it holds still, so its figures and tooltips can be read.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Galley, Layout, Painter, Rect, Role, Sense, Stroke,
    StrokeKind, TextFormat, Ui, WidgetInfo, pos2, text::LayoutJob, vec2,
};
use poc_viewer::timing::{Axis, Core1, Detection, Lane, Snapshot, Stage, Step, WINDOW};

use super::theme::{
    self, BODY, FAINT, FAULT, FIGURE, HERO, Hue, LINE, MUTED, NEUTRAL, RAISED, SMALL, TEXT, WAIT,
    hue,
};

/// How long a new snapshot takes to glide into place.
const GLIDE: Duration = Duration::from_millis(150);

/// The lane names' column, the latencies' column, a label's line, the space between a label
/// and its bar, a bar, and one row (labels above and below the bar).
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

/// What the diagram shows: the latest snapshot, eased in from the one before.
#[derive(Debug, Default)]
pub struct Diagram {
    target: Option<Snapshot>,
    /// The steps shown when the target arrived.
    from: Vec<Step>,
    since: Option<Instant>,
    /// The steps shown now.
    shown: Vec<Step>,
    /// The axis, and its length shown, gliding from `axis_from` when the target arrived.
    axis: Axis,
    axis_from: f32,
    axis_len: f32,
    /// The pointer is over the diagram: it holds still.
    hold: bool,
    /// A newer snapshot waits while the diagram holds.
    waiting: bool,
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
            *self = Self::default();
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
        self.axis_len = ease(self.axis_from, self.axis.len_us());
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
        self.axis.fit(s.total_us);
        self.axis_from = if self.axis_len > 0.0 {
            self.axis_len
        } else {
            self.axis.len_us()
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
                "Paused while hovering, new data waiting".to_owned()
            } else {
                "Paused while hovering".to_owned()
            });
        }
        let s = self.target.as_ref()?;
        (s.collected < WINDOW).then(|| {
            format!(
                "Collecting data: {:.1} / {} s",
                s.collected.as_secs_f32(),
                WINDOW.as_secs()
            )
        })
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
                .map(|end| (end, "frame captured".to_owned())),
            Lane::Image => self.lane(Lane::Image).last().map(|last| {
                (
                    last.end_us(),
                    if s.image_complete {
                        "sent to the GPU".to_owned()
                    } else {
                        "measured so far".to_owned()
                    },
                )
            }),
            Lane::Markers => self.end_of(Stage::MarkerSend).map(|here| {
                (
                    here,
                    match self.end_of(Stage::MarkerDraw) {
                        Some(d) => format!("received, drawn at {} ms", ms(d)),
                        None => "received".to_owned(),
                    },
                )
            }),
        }
    }

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
                "Waiting for the board",
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
        let len = this.axis_len.max(1.0);
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
            "Time (ms)",
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

        // The cursor: a line at the pointer down to the axis, its time in a box over the
        // axis' labels.
        if this.hold
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && (left..=right).contains(&p.x)
        {
            painter.vline(
                p.x,
                rect.top()..=ay,
                Stroke::new(1.0, theme::fade(TEXT, 0.7)),
            );
            let g = painter.layout_no_wrap(
                format!("{} ms", ms((p.x - left) / scale)),
                theme::mono(SMALL),
                TEXT,
            );
            let r = Rect::from_center_size(
                pos2(p.x, ay + 3.0 + (g.size().y + 2.0) / 2.0),
                g.size() + vec2(10.0, 2.0),
            );
            painter.rect(
                r,
                CornerRadius::ZERO,
                RAISED,
                Stroke::new(1.0, MUTED),
                StrokeKind::Inside,
            );
            painter.galley(r.center() - g.size() / 2.0, g, TEXT);
        }

        for (i, (r, part)) in parts.into_iter().enumerate() {
            let resp = ui.interact(r, response.id.with(("part", i)), Sense::hover());
            resp.widget_info(|| part_info(&part));
            let steps = &this.shown;
            resp.on_hover_ui(|ui| match part {
                Part::Step(st) => step_tip(ui, &st, steps, s.core1.as_ref()),
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
            "New frame every {} ms ({:.1} fps)",
            ms(s.period_us),
            1e6 / s.period_us
        );
        let periods = |us: f32| us / s.period_us;
        if let Some(end) = self.lane(Lane::Image).last().map(Step::end_us)
            && s.image_complete
        {
            out.push_str(&format!("  ·  picture latency: {:.1} frames", periods(end)));
        } else if let Some(here) = self.end_of(Stage::MarkerSend) {
            out.push_str(&format!("  ·  marker latency: {:.1} frames", periods(here)));
        }
        out
    }

    /// The ribbon in a few sentences, for the tools and the screen reader (each bar and wait
    /// is a node of its own).
    fn summary(&self, s: &Snapshot) -> String {
        let mut out = Vec::new();
        for lane in LANES {
            match self.latency(s, lane) {
                Some((us, what)) => out.push(format!("{}: {} ms ({what})", lane.name(), ms(us))),
                None => out.push(format!("{}: {}", lane.name(), idle(s, lane).0)),
            }
        }
        out.push(format!("new frame every {} ms", ms(s.period_us)));
        out.join("; ")
    }

    /// What the marks mean, in one line of samples.
    pub fn key(&self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            let sample = |ui: &mut Ui, draw: &dyn Fn(&Painter, Rect), text: &str| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 14.0), Sense::hover());
                draw(ui.painter(), r.shrink2(vec2(0.0, 2.0)));
                ui.label(theme_text(text, theme::sans(SMALL), MUTED));
                ui.add_space(10.0);
            };
            sample(ui, &|p, r| bar(p, r, NEUTRAL, true), "measured");
            sample(ui, &|p, r| bar(p, r, NEUTRAL, false), "computed");
            sample(ui, &|p, r| wait_band(p, r), "wait");
            sample(
                ui,
                &|p, r| {
                    p.rect_filled(
                        Rect::from_min_max(r.min, pos2(r.left() + 8.0, r.bottom())),
                        CornerRadius::ZERO,
                        NEUTRAL.fill,
                    );
                    whisker(
                        p,
                        r.left() + 8.0,
                        r.right() - 1.0,
                        r.center().y,
                        NEUTRAL.line,
                        r.right(),
                    );
                },
                "slowest",
            );
            sample(
                ui,
                &|p, r| not_measured(p, r.left(), r.right(), r.center().y),
                "not measured",
            );
        });
    }
}

/// Why a lane has no steps, and the colour to say it in.
fn idle(s: &Snapshot, lane: Lane) -> (&'static str, Color32) {
    match lane {
        Lane::Sensor => ("Waiting for the sensor mode", MUTED),
        Lane::Image if !s.lanes.frames => ("Frames off", MUTED),
        Lane::Image => ("Waiting for frames", MUTED),
        Lane::Markers => match s.lanes.detection {
            Detection::Off => ("Detection off", MUTED),
            Detection::Unavailable => ("No detection in this mode", MUTED),
            Detection::Fault => ("Detection stopped (fault)", FAULT),
            Detection::On => ("Waiting for markers", MUTED),
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
            );
        }
    }
    for (st, r) in steps.iter().zip(&bars) {
        let h = hue(st.stage);
        bar(plot, *r, h, st.measured);
        tail_mark(plot, *r, st, x, h);
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
                    "{} ms, computed, starts at {} ms",
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
    ui.set_max_width(320.0);
    let s = ui.spacing_mut();
    s.interact_size.y = 0.0;
    s.item_spacing.y = 4.0;
}

/// The figures of one stage, for a tooltip.
fn step_tip(ui: &mut Ui, st: &Step, steps: &[Step], core1: Option<&Core1>) {
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
    let what = match st.stage {
        Stage::Exposure => "The sensor collecting light. Computed from the sensor settings.",
        Stage::Readout => {
            "The sensor sending the frame to the board, line by line. Computed from the sensor \
             settings."
        }
        Stage::Encode => {
            "The board compressing the frame to JPEG. It starts during the readout; the line in \
             the bar marks when the last rows came in."
        }
        Stage::Send => {
            "The picture travelling from the board to this PC. May read slightly short: the \
             clocks are matched on the fastest packet."
        }
        Stage::Decode => "This PC decompressing the JPEG.",
        Stage::Draw => "This PC drawing the picture and uploading it to the GPU.",
        Stage::Detect => {
            "The board finding bright spots, strip by strip, while the frame is still being \
             read out."
        }
        Stage::Measure => "The board measuring each spot on the whole frame.",
        Stage::MarkerSend => "The markers travelling from the board to this PC.",
        Stage::MarkerDraw => {
            "This PC drawing the markers. With frames on, they are drawn with their picture."
        }
    };
    ui.label(theme_text(what, theme::sans(SMALL), MUTED));
    let row = |k: &str, v: String| (k.to_owned(), v, TEXT);
    let mut rows = Vec::new();
    if st.measured {
        rows.push(row("Median", format!("{} ms", ms(st.len_us))));
        rows.push(row("Slowest", format!("{} ms", ms(st.slowest_us))));
    } else {
        rows.push(row("Duration", format!("{} ms", ms(st.len_us))));
    }
    rows.push(row("Starts at", format!("{} ms", ms(st.start_us))));
    if lane != Lane::Sensor && capture - st.start_us > 1.0 {
        rows.push(row(
            "Overlaps the readout by",
            format!("{} ms", ms(capture - st.start_us)),
        ));
    }
    if st.start_us - before > 1.0 {
        rows.push(row(
            "Wait before it",
            format!("{} ms", ms(st.start_us - before)),
        ));
    }
    if st.tail_us > 1.0 {
        rows.push(row("After the last rows", format!("{} ms", ms(st.tail_us))));
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
                "Start delay (max)",
                format!("{} ms", ms(c.start_max_us)),
            ));
            rows.push(row(
                "Per strip (max)",
                format!("{} ms", ms(c.detect_max_us)),
            ));
            rows.push(row(
                "Copy wait (max)",
                format!("{} ms", ms(c.copy_wait_max_us)),
            ));
            rows.push((
                "Hold (max / allowed)".to_owned(),
                format!("{} / {} ms", ms(c.hold_max_us), ms(c.hold_budget_us)),
                if c.hold_max_us > c.hold_budget_us {
                    FAULT
                } else {
                    TEXT
                },
            ));
            rows.push((
                "Late strips".to_owned(),
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
            rows.push(row("CPU time", format!("{} ms", ms(c.measure_us))));
        }
        _ => {}
    }
    tip_grid(ui, "step tip", &rows);
    if matches!(st.stage, Stage::Draw | Stage::MarkerDraw) {
        ui.label(theme_text(
            "The time from here to the screen is not measured.",
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
        Stage::Encode => "Waiting for the encoder.",
        Stage::Send => "Waiting for the network: the previous frame is still being sent.",
        Stage::Decode => {
            "Waiting for the decoder. It takes the newest frame once the previous one is drawn."
        }
        Stage::Draw => "Waiting for the next screen refresh.",
        Stage::MarkerSend => {
            "Waiting for the board to pick up the result (every 1 ms) and send it."
        }
        Stage::MarkerDraw => {
            "Waiting for the next screen refresh, or for their picture when frames are on."
        }
        _ => "Time between two stages.",
    };
    ui.label(theme_text(why, theme::sans(SMALL), MUTED));
    tip_grid(
        ui,
        "wait tip",
        &[("Median".to_owned(), format!("{} ms", ms(us)), TEXT)],
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

/// A stage's bar, square-cornered: solid when measured, hatched when computed.
fn bar(painter: &Painter, r: Rect, h: Hue, measured: bool) {
    if measured {
        painter.rect(
            r,
            CornerRadius::ZERO,
            h.fill,
            Stroke::new(1.0, h.line),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(r, CornerRadius::ZERO, theme::fade(h.fill, 0.45));
        let clip = painter.with_clip_rect(r.shrink(1.0).intersect(painter.clip_rect()));
        let stroke = Stroke::new(1.0, theme::fade(h.line, 0.6));
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
            CornerRadius::ZERO,
            Stroke::new(1.0, h.line),
            StrokeKind::Inside,
        );
    }
}

/// A wait: a flat grey band, square-cornered.
fn wait_band(painter: &Painter, r: Rect) {
    painter.rect_filled(r, CornerRadius::ZERO, WAIT.fill);
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
fn tail_mark(painter: &Painter, r: Rect, st: &Step, x: impl Fn(f32) -> f32, h: Hue) {
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
        Stroke::new(1.5, h.text),
    );
}

/// From the median's end `x0` to the slowest sample's end `x1`, with a cap; an arrow at
/// `right` where it leaves the axis.
fn whisker(painter: &Painter, x0: f32, x1: f32, y: f32, c: Color32, right: f32) {
    /// Half the cap's height.
    const CAP: f32 = 3.5;
    if x1 - x0 < 2.0 || x0 >= right {
        return;
    }
    let stroke = Stroke::new(1.2, c);
    let end = x1.min(right);
    painter.hline(x0..=end, y, stroke);
    if x1 > right {
        painter.line_segment([pos2(end - CAP, y - CAP), pos2(end, y)], stroke);
        painter.line_segment([pos2(end - CAP, y + CAP), pos2(end, y)], stroke);
    } else {
        painter.vline(end, (y - CAP)..=(y + CAP), stroke);
    }
}
