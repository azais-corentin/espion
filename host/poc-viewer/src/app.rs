//! The window: the status bar on top, the settings rail on the left, the picture, and the
//! pipeline timing under it; the counters in a drawer on the right.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use eframe::egui::{self, Margin, Sense, Stroke, Ui, vec2};
use espion_proto::cam::{
    CameraSettings, Encoding, Format, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, Op, Origin, Readout,
    TestPattern,
};
use poc_viewer::{
    camera,
    decode::{DecodeStats, Decoder, Picture},
    fake::{self, FakeCamera},
    net::{Client, Command, RETRY_INTERVAL, Stats},
    stream::{AUTO_QUALITY_CEILING, StreamEdit},
    timing::{HostFrame, Sensor, Stage, Timing},
};

use crate::{
    display::{self, Shown, UploadTiming},
    selftest::{Probe, SelfTest, SelfTestConfig},
    ui::{
        diagram::Diagram,
        hz, on_off,
        picture::{self, View},
        theme::{self, BODY, FAULT, GAP, MUTED, NEUTRAL, PAD, TEXT, hue},
        widgets::{
            self, Choice, History, Scale, Tone, button, caption, chip, figure, notice, number, row,
            section, segmented, slider, stat, stat_toned, switch,
        },
        window_picker,
    },
};

/// What `main` hands the app.
pub struct Setup {
    pub camera: SocketAddr,
    pub fake: Option<FakeCamera>,
    pub init_camera: Option<CameraSettings>,
    pub init_stream: Option<StreamEdit>,
    pub selftest: Option<SelfTestConfig>,
    /// Start at 1:1 instead of fit-to-window.
    pub one_to_one: bool,
    /// Display gain at start.
    pub display_gain: f32,
}

/// Width of the settings rail, points.
const RAIL_WIDTH: f32 = 340.0;
/// Width of the counters drawer, points.
const DRAWER_WIDTH: f32 = 330.0;
/// Height of the staircase when the timing strip is expanded, points.
const STAIRCASE_HEIGHT: f32 = 300.0;

/// Rates over the last second, and their last minute.
struct Meter {
    at: Instant,
    whole: u64,
    bytes: u64,
    jpeg_whole: u64,
    jpeg_bytes: u64,
    presented: u64,
    decoded: u64,
    decode_total: Duration,
    rx_fps: f64,
    mbps: f64,
    shown_fps: f64,
    decoded_fps: f64,
    /// Mean length of the JPEG frames received whole, bytes.
    mean_len: f64,
    /// Mean decode time, ms.
    decode_ms: f64,
    cpu_sum: f64,
    cpu_n: u32,
    cpu_ms: f64,
    rx_history: History,
    shown_history: History,
    mbps_history: History,
}

impl Meter {
    fn new() -> Self {
        Self {
            at: Instant::now(),
            whole: 0,
            bytes: 0,
            jpeg_whole: 0,
            jpeg_bytes: 0,
            presented: 0,
            decoded: 0,
            decode_total: Duration::ZERO,
            rx_fps: 0.0,
            mbps: 0.0,
            shown_fps: 0.0,
            decoded_fps: 0.0,
            mean_len: 0.0,
            decode_ms: 0.0,
            cpu_sum: 0.0,
            cpu_n: 0,
            cpu_ms: 0.0,
            rx_history: History::default(),
            shown_history: History::default(),
            mbps_history: History::default(),
        }
    }

    fn update(&mut self, now: Instant, s: &Stats, d: &DecodeStats, presented: u64) {
        let dt = (now - self.at).as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let whole = s.frames_whole - self.whole;
        self.rx_fps = whole as f64 / dt;
        self.mbps = (s.bytes - self.bytes) as f64 * 8.0 / dt / 1e6;
        let jpeg = s.jpeg_whole - self.jpeg_whole;
        self.mean_len = if jpeg > 0 {
            (s.jpeg_bytes - self.jpeg_bytes) as f64 / jpeg as f64
        } else {
            0.0
        };
        self.shown_fps = (presented - self.presented) as f64 / dt;
        let decoded = d.decoded + d.failures - self.decoded;
        self.decoded_fps = decoded as f64 / dt;
        self.decode_ms = if decoded > 0 {
            (d.decode_total - self.decode_total).as_secs_f64() * 1e3 / decoded as f64
        } else {
            0.0
        };
        self.cpu_ms = if self.cpu_n > 0 {
            self.cpu_sum / self.cpu_n as f64 * 1e3
        } else {
            0.0
        };
        self.rx_history.push(self.rx_fps as f32);
        self.shown_history.push(self.shown_fps as f32);
        self.mbps_history.push(self.mbps as f32);
        (self.at, self.whole, self.bytes, self.presented) =
            (now, s.frames_whole, s.bytes, presented);
        (self.jpeg_whole, self.jpeg_bytes) = (s.jpeg_whole, s.jpeg_bytes);
        (self.decoded, self.decode_total) = (d.decoded + d.failures, d.decode_total);
        (self.cpu_sum, self.cpu_n) = (0.0, 0);
    }
}

/// The view choices of the display section.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewChoice {
    Fit,
    Pixels,
    Zoomed,
}

pub struct App {
    client: Arc<Client>,
    decoder: Decoder,
    _fake: Option<FakeCamera>,
    /// The mode and control settings being edited.
    edit: CameraSettings,
    /// The editor's origin is "centred" (sent as `None`).
    centred: bool,
    origin: Origin,
    stream: StreamEdit,
    /// Take the board's settings into the editors at its next state: its first, and the first
    /// after the link was lost or the board restarted.
    adopt: bool,
    /// The link was lost since the board's last state.
    lost: bool,
    /// The board's uptime in its last state, µs (it goes back when the board restarts).
    uptime_us: u64,
    view: View,
    display_gain: f32,
    shown: Option<Arc<Shown>>,
    serial: u64,
    timing: Arc<Mutex<UploadTiming>>,
    presented: u64,
    passes: u64,
    cpu: Vec<f32>,
    meter: Meter,
    selftest: Option<SelfTest>,
    /// Where a frame's time goes.
    pipeline: Timing,
    /// The frame shown last, until its upload is known: its serial and its times so far
    /// (`drawn` not yet).
    drawing: Option<(u64, HostFrame)>,
    /// The board's generation the pipeline's samples are of.
    generation: u32,
    diagram: Diagram,
    latency_history: History,
    /// The timing strip shows the staircase.
    expanded: bool,
    /// The counters drawer is open.
    drawer: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, setup: Setup) -> Result<Self> {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .ok_or_else(|| anyhow!("the wgpu renderer is not available"))?;
        display::init(rs);
        theme::install(&cc.egui_ctx);
        let info = rs.adapter.get_info();
        println!(
            "wgpu adapter {} ({:?}, {:?}), surface {:?}",
            info.name, info.device_type, info.backend, rs.target_format
        );
        let ctx = cc.egui_ctx.clone();
        let client = Arc::new(
            Client::connect(setup.camera, move || ctx.request_repaint())
                .with_context(|| format!("opening a socket for {}", setup.camera))?,
        );
        let ctx = cc.egui_ctx.clone();
        let decoder = Decoder::spawn(client.clone(), move || ctx.request_repaint())
            .context("starting the decoding thread")?;
        println!(
            "camera {}, bound {}, receive buffer granted {} bytes (requested {})",
            client.camera(),
            client.local_addr(),
            client.rcvbuf(),
            poc_viewer::net::RCVBUF_REQUEST
        );
        let adopt = setup.init_camera.is_none();
        if let Some(c) = setup.init_camera {
            client.send(Command::SetCamera(c));
        }
        if let Some(s) = setup.init_stream {
            client.send(Command::SetStream(s.settings()));
        }
        client.send(Command::GetState);
        let edit = setup.init_camera.unwrap_or_else(fake::default_camera);
        let now = Instant::now();
        Ok(Self {
            client,
            decoder,
            _fake: setup.fake,
            centred: edit.origin.is_none(),
            origin: edit.origin.unwrap_or(Origin { x: 0, y: 0 }),
            edit,
            stream: setup
                .init_stream
                .unwrap_or(StreamEdit::new(fake::default_stream())),
            adopt,
            lost: false,
            uptime_us: 0,
            view: if setup.one_to_one {
                View::Pixels
            } else {
                View::Fit
            },
            display_gain: setup.display_gain,
            shown: None,
            serial: 0,
            timing: Arc::default(),
            presented: 0,
            passes: 0,
            cpu: Vec::new(),
            meter: Meter::new(),
            selftest: setup.selftest.map(|cfg| SelfTest::new(cfg, now)),
            pipeline: Timing::new(now),
            drawing: None,
            generation: 0,
            diagram: Diagram::default(),
            latency_history: History::default(),
            expanded: false,
            drawer: false,
        })
    }

    /// The editor's request (origin per the "centred" switch).
    fn request(&self) -> CameraSettings {
        CameraSettings {
            origin: (!self.centred).then_some(self.origin),
            ..self.edit
        }
    }

    /// The stream controls to use with the mode `req`: the stream's, or RAW8 when its
    /// encoding cannot be made of that mode (RAW8 can be sent from every mode).
    fn stream_for(&self, req: &CameraSettings) -> StreamEdit {
        match self
            .stream
            .encoding
            .check(req.format, req.width, req.height)
        {
            Ok(()) => self.stream,
            Err(_) => StreamEdit {
                encoding: Encoding::Raw8,
                ..self.stream
            },
        }
    }

    fn show(&mut self, p: Picture, now: Instant) {
        self.serial += 1;
        self.presented += 1;
        self.drawing = Some((
            self.serial,
            HostFrame {
                completed: p.frame.completed,
                decode: p.decode,
                taken: now,
                drawn: now,
            },
        ));
        let new = Arc::new(Shown {
            serial: self.serial,
            frame: p.frame,
            jpeg_len: p.jpeg_len,
        });
        if let Some(old) = self.shown.replace(new)
            && let Ok(old) = Arc::try_unwrap(old)
        {
            self.decoder.recycle(Picture {
                frame: old.frame,
                jpeg_len: old.jpeg_len,
                decode: None,
            });
        }
    }

    /// Hands the frame shown in an earlier pass to the pipeline timing, now that its upload
    /// ended. Before the next frame is taken, which starts a new record.
    fn drawn(&mut self, now: Instant) {
        let uploaded = self.timing.lock().unwrap_or_else(|e| e.into_inner()).last;
        if let Some((serial, f)) = self.drawing
            && let Some((done, drawn)) = uploaded
            && done == serial
        {
            self.pipeline.host(now, HostFrame { drawn, ..f });
            self.drawing = None;
        }
    }

    /// Feeds the pipeline timing the newest frame the board timed and makes a new snapshot
    /// when one is due; moves the diagram towards it.
    fn time_pipeline(&mut self, ctx: &egui::Context, now: Instant, s: &Stats) {
        if let Some(st) = s.state
            && st.generation != self.generation
        {
            self.generation = st.generation;
            self.pipeline.reset(now);
        }
        if let Some(b) = s.board_frame {
            self.pipeline.board(now, b);
        }
        if self.pipeline.update(now, s.state.as_ref().map(Sensor::of))
            && let Some(snap) = self.pipeline.snapshot()
        {
            self.latency_history.push(snap.total_us / 1e3);
        }
        if self.diagram.update(self.pipeline.snapshot(), now) {
            ctx.request_repaint();
        }
    }

    // ── Status bar ──────────────────────────────────────────────────────────────────────────

    fn status_bar(&mut self, ui: &mut Ui, s: &Stats, now: Instant) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            link_pill(ui, s, now, &self.client);
            let m = &self.meter;
            let stage = |st| hue(st);
            figure(
                ui,
                "Received",
                &format!("{:.1}", m.rx_fps),
                "fps",
                &m.rx_history,
                stage(Stage::Send),
            );
            figure(
                ui,
                "Presented",
                &format!("{:.1}", m.shown_fps),
                "fps",
                &m.shown_history,
                stage(Stage::Draw),
            );
            figure(
                ui,
                "Link",
                &format!("{:.1}", m.mbps),
                "Mbit/s",
                &m.mbps_history,
                stage(Stage::Send),
            );
            let latency = self
                .diagram
                .snapshot()
                .map_or("-".to_owned(), |snap| format!("{:.1}", snap.total_us / 1e3));
            figure(
                ui,
                "Latency",
                &latency,
                "ms",
                &self.latency_history,
                NEUTRAL,
            );
            widgets::from_right(ui, |ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                if chip(ui, "Counters", self.drawer).clicked() {
                    self.drawer = !self.drawer;
                }
                let faults = faults(s, &self.decoder.stats());
                if faults > 0 {
                    ui.label(
                        egui::RichText::new(format!("{faults} faults"))
                            .font(theme::mono(BODY))
                            .color(FAULT),
                    )
                    .on_hover_text(
                        "Frames dropped for a missing chunk, capture and encoder errors, \
                         decode failures since start. The counters drawer has each.",
                    );
                }
                if s.pending {
                    ui.label(
                        egui::RichText::new("Waiting for an answer")
                            .font(theme::sans(BODY))
                            .color(MUTED),
                    );
                }
            });
        });
    }

    // ── Rail ────────────────────────────────────────────────────────────────────────────────

    fn rail(&mut self, ui: &mut Ui, s: &Stats, d: &DecodeStats) {
        if let Some(r) = &s.refusal {
            let what = match r.op {
                Op::SetCamera(_) => "camera settings",
                Op::SetStream(_) => "stream settings",
                Op::GetState | Op::Poll => "request",
            };
            notice(
                ui,
                Tone::Fault,
                &format!("The board refused the {what}: {}", r.error),
            );
            ui.add_space(GAP);
        }
        if let Some(e) = &d.last_failure {
            notice(ui, Tone::Fault, &format!("Last decode failure: {e}"));
            ui.add_space(GAP);
        }
        self.mode_section(ui, s);
        self.controls_section(ui, s);
        self.stream_section(ui, s);
        self.display_section(ui);
    }

    fn mode_section(&mut self, ui: &mut Ui, s: &Stats) {
        let h = hue(Stage::Readout);
        section(ui, "Sensor mode", h, |ui| {
            let e = &mut self.edit;
            segmented(
                ui,
                "Sensor format",
                h,
                &mut e.format,
                &[
                    Choice::new(Format::Raw8, "RAW8"),
                    Choice::new(Format::Raw10, "RAW10"),
                ],
                2,
            );
            if segmented(
                ui,
                "Readout",
                h,
                &mut e.readout,
                &[
                    Choice::new(Readout::Crop, "Crop"),
                    Choice::new(Readout::Binned, "2x2 binned"),
                ],
                2,
            ) {
                // The window keeps its place on the sensor: binning halves the output size,
                // cropping doubles it (on the format's grid, within the readout's largest).
                let f = camera::format(e.format);
                let (wm, hm) = (
                    u16::from(ov9281::limits::width_multiple(f)),
                    u16::from(ov9281::limits::height_multiple(f)),
                );
                let (max_w, max_h) = ov9281::limits::max_size(camera::readout(e.readout));
                let fit = |v: u32, m: u16, max: u16| (v.min(u32::from(max)) as u16 / m * m).max(m);
                let (w, h2) = match e.readout {
                    Readout::Binned => (u32::from(e.width) / 2, u32::from(e.height) / 2),
                    Readout::Crop => (u32::from(e.width) * 2, u32::from(e.height) * 2),
                };
                (e.width, e.height) = (fit(w, wm, max_w), fit(h2, hm, max_h));
            }
            window_picker::show(ui, h, &mut self.edit, &mut self.centred, &mut self.origin);
            let e = &mut self.edit;
            row(ui, "Size", |ui| {
                let w = (ui.available_width() - 2.0 * GAP - 10.0) / 2.0;
                number(ui, "Width", &mut e.width, 0..=2048, 1.0, 0, w);
                ui.label(egui::RichText::new("x").color(MUTED));
                number(ui, "Height", &mut e.height, 0..=2048, 1.0, 0, w);
            });
            switch(ui, "Centred origin", h, &mut self.centred);
            if !self.centred {
                let o = &mut self.origin;
                row(ui, "Origin", |ui| {
                    let w = (ui.available_width() - 2.0 * GAP - 10.0) / 2.0;
                    number(ui, "Origin x", &mut o.x, 0..=1280, 1.0, 0, w);
                    ui.label(egui::RichText::new(",").color(MUTED));
                    number(ui, "Origin y", &mut o.y, 0..=800, 1.0, 0, w);
                });
            }
            let e = &mut self.edit;
            let mut rate = f64::from(e.rate_mhz) / 1000.0;
            let mut tol = f64::from(e.tolerance_mhz) / 1000.0;
            row(ui, "Rate, Hz", |ui| {
                let w = ui.available_width();
                number(ui, "Rate, Hz", &mut rate, 0.0..=2000.0, 0.1, 3, w);
            });
            row(ui, "Tolerance, Hz", |ui| {
                let w = ui.available_width();
                number(ui, "Tolerance, Hz", &mut tol, 0.0..=1000.0, 0.01, 3, w);
            });
            e.rate_mhz = (rate * 1000.0).round() as u32;
            e.tolerance_mhz = (tol * 1000.0).round() as u32;
            self.mode_verdict(ui, s);
        });
    }

    /// The driver's verdict on the edited mode, before anything is sent, with the encoding the
    /// stream will have in it; and the button that sends it.
    fn mode_verdict(&mut self, ui: &mut Ui, s: &Stats) {
        let h = hue(Stage::Readout);
        let req = self.request();
        let stream = self.stream_for(&req);
        let local = camera::build_mode(&req).and_then(|m| {
            stream
                .encoding
                .check(req.format, req.width, req.height)
                .map(|()| m)
        });
        if let Err(e) = self
            .stream
            .encoding
            .check(req.format, req.width, req.height)
        {
            notice(
                ui,
                Tone::Caution,
                &format!(
                    "Applying moves the stream from {:?} to RAW8: {e}",
                    self.stream.encoding
                ),
            );
        }
        match &local {
            Ok(m) => {
                let info = camera::mode_info(m);
                if info.exceeds_datasheet_clock {
                    notice(
                        ui,
                        Tone::Caution,
                        &format!(
                            "Sensor clock {} MHz: above the datasheet's 80 MHz",
                            info.sensor_clock_mhz
                        ),
                    );
                }
                caption(
                    ui,
                    &format!(
                        "{} ({:+} mHz) at {} MHz: HTS {}, VTS {}, PSV {}. Longest exposure {} \
                         lines ({:.2} ms).",
                        hz(info.frame_rate_mhz),
                        info.rate_error_mhz,
                        info.sensor_clock_mhz,
                        info.hts,
                        info.vts,
                        on_off(info.psv),
                        info.max_exposure_lines,
                        f64::from(info.max_exposure_lines) * f64::from(info.line_ns) / 1e6
                    ),
                );
            }
            Err(err) => notice(ui, Tone::Fault, &format!("Refused here: {err}")),
        }
        let (max_w, max_h) = ov9281::limits::max_size(camera::readout(req.readout));
        let mut limits = format!(
            "Sizes 128..={max_w} x 2..={max_h}, width a multiple of {}, height of {}.",
            ov9281::limits::width_multiple(camera::format(req.format)),
            ov9281::limits::height_multiple(camera::format(req.format))
        );
        if (128..=max_w).contains(&req.width) && (2..=max_h).contains(&req.height) {
            let (f80, f96) = camera::fastest_rates(req.format, req.width, req.height, req.readout);
            limits.push_str(&format!(
                " Fastest {} at 80 MHz, {} at 96 MHz; slowest {}.",
                hz(f80),
                hz(f96),
                hz(camera::slowest_rate())
            ));
        }
        caption(ui, &limits);
        let applied = s.state.map(|st| st.camera);
        let differs = applied.is_some_and(|a| camera::build_mode(&a).ok() != local.ok());
        if differs {
            caption(ui, "Edited: differs from the board's mode.");
        }
        let clicked = ui
            .add_enabled_ui(local.is_ok(), |ui| {
                button(ui, "Apply mode", differs.then_some(h)).clicked()
            })
            .inner;
        if clicked && let Ok(m) = local {
            self.edit.exposure_lines = self.edit.exposure_lines.clamp(1, m.max_exposure_lines());
            if stream != self.stream {
                self.stream = stream;
                self.client.send(Command::SetStream(self.stream.settings()));
            }
            self.client.send(Command::SetCamera(self.request()));
        }
    }

    fn controls_section(&mut self, ui: &mut Ui, s: &Stats) {
        let h = hue(Stage::Exposure);
        let max = s
            .state
            .map(|st| st.mode.max_exposure_lines)
            .or_else(|| {
                camera::build_mode(&self.request())
                    .ok()
                    .map(|m| m.max_exposure_lines())
            })
            .unwrap_or(1)
            .max(1);
        let line_ns = s.state.map(|st| st.mode.line_ns);
        section(ui, "Exposure and gain", h, |ui| {
            caption(ui, "Applied at once, without restarting the stream.");
            let e = &mut self.edit;
            let mut changed = false;
            let mut lines = f64::from(e.exposure_lines);
            let shown = match line_ns {
                Some(ns) => format!(
                    "{:.3} ms  {} lines",
                    lines * f64::from(ns) / 1e6,
                    e.exposure_lines
                ),
                None => format!("{} lines", e.exposure_lines),
            };
            if slider(
                ui,
                "Exposure",
                h,
                &mut lines,
                &Scale {
                    range: 1.0..=f64::from(max),
                    step: 1.0,
                    logarithmic: max > 400,
                },
                &shown,
            ) {
                e.exposure_lines = lines as u16;
                changed = true;
            }
            let mut gain = f64::from(e.gain_sixteenths);
            let shown = format!("{:.2}x", gain / 16.0);
            if slider(
                ui,
                "Gain",
                h,
                &mut gain,
                &Scale {
                    range: f64::from(ov9281::limits::GAIN_MIN)
                        ..=f64::from(ov9281::limits::GAIN_MAX),
                    step: 1.0,
                    logarithmic: false,
                },
                &shown,
            ) {
                e.gain_sixteenths = gain as u8;
                changed = true;
            }
            ui.label(egui::RichText::new("Test pattern").color(MUTED));
            changed |= segmented(
                ui,
                "Test pattern",
                h,
                &mut e.test_pattern,
                &[
                    Choice::new(TestPattern::Off, "Off"),
                    Choice::new(TestPattern::Bars, "Bars"),
                    Choice::new(TestPattern::RollingBars, "Rolling"),
                    Choice::new(TestPattern::GreySquares, "Grey sq."),
                    Choice::new(TestPattern::BlackWhiteSquares, "B/W sq."),
                    Choice::new(TestPattern::Random, "Random"),
                ],
                3,
            );
            if changed {
                // The board's mode with the new controls: it changes only the controls.
                let base = s.state.map_or(self.request(), |st| st.camera);
                self.client.send(Command::SetCamera(CameraSettings {
                    exposure_lines: self.edit.exposure_lines,
                    gain_sixteenths: self.edit.gain_sixteenths,
                    test_pattern: self.edit.test_pattern,
                    ..base
                }));
            }
        });
    }

    fn stream_section(&mut self, ui: &mut Ui, s: &Stats) {
        let h = hue(Stage::Send);
        let enc = hue(Stage::Encode);
        // The running mode decides which encodings can be sent.
        let mode = s.state.map_or_else(|| self.request(), |st| st.camera);
        let in_use = s.state.map_or(AUTO_QUALITY_CEILING, |b| b.encoder.quality);
        section(ui, "Stream", h, |ui| {
            caption(ui, "Applied at once.");
            let st = &mut self.stream;
            let mut changed = switch(ui, "Send frames", h, &mut st.enabled).changed();
            let refusal = |e: Encoding| e.check(mode.format, mode.width, mode.height).err();
            changed |= segmented(
                ui,
                "Encoding",
                h,
                &mut st.encoding,
                &[
                    Choice {
                        refused: refusal(Encoding::Jpeg).map(|e| e.to_string()),
                        ..Choice::new(Encoding::Jpeg, "JPEG")
                    },
                    Choice {
                        refused: refusal(Encoding::Raw8).map(|e| e.to_string()),
                        ..Choice::new(Encoding::Raw8, "RAW8")
                    },
                    Choice {
                        refused: refusal(Encoding::Raw10).map(|e| e.to_string()),
                        ..Choice::new(Encoding::Raw10, "RAW10 packed")
                    },
                ],
                3,
            );
            if let Some(e) = refusal(Encoding::Jpeg) {
                caption(ui, &format!("JPEG not available: {e}"));
            }
            changed |= switch(ui, "Automatic JPEG quality", enc, &mut st.auto_quality)
                .on_hover_text(format!(
                    "The board chooses the quality, up to {AUTO_QUALITY_CEILING}: it lowers it \
                     while frames are skipped because the link is busy and raises it back when \
                     there is room. The quality set on the slider applies with this off."
                ))
                .changed();
            let scale = Scale {
                range: f64::from(JPEG_QUALITY_MIN)..=f64::from(JPEG_QUALITY_MAX),
                step: 1.0,
                logarithmic: false,
            };
            if st.auto_quality {
                // The board chooses: the slider shows the quality it has in use.
                let mut q = f64::from(in_use);
                ui.add_enabled_ui(false, |ui| {
                    slider(
                        ui,
                        "JPEG quality",
                        enc,
                        &mut q,
                        &scale,
                        &format!("{in_use} (board, up to {AUTO_QUALITY_CEILING})"),
                    );
                });
            } else {
                let mut q = f64::from(st.manual_quality);
                let shown = st.manual_quality.to_string();
                if slider(ui, "JPEG quality", enc, &mut q, &scale, &shown) {
                    st.manual_quality = q as u8;
                    changed = true;
                }
            }
            row(ui, "Frames/s at most", |ui| {
                let w = ui.available_width();
                changed |= number(
                    ui,
                    "Frames per second at most (0: no limit)",
                    &mut st.max_fps,
                    0..=2000,
                    1.0,
                    0,
                    w,
                );
            });
            caption(ui, "0: no limit.");
            if changed {
                self.client.send(Command::SetStream(st.settings()));
            }
        });
    }

    fn display_section(&mut self, ui: &mut Ui) {
        let h = hue(Stage::Draw);
        section(ui, "Display", h, |ui| {
            let ppp = ui.ctx().pixels_per_point();
            let mut choice = match self.view {
                View::Fit => ViewChoice::Fit,
                View::Pixels => ViewChoice::Pixels,
                View::Free { scale, .. } if (scale * ppp - 1.0).abs() < 1e-3 => ViewChoice::Pixels,
                View::Free { .. } => ViewChoice::Zoomed,
            };
            let zoom = match self.view {
                View::Free { scale, .. } => format!("{:.2}x", scale * ppp),
                _ => "Zoomed".to_owned(),
            };
            let before = choice;
            segmented(
                ui,
                "View",
                h,
                &mut choice,
                &[
                    Choice::new(ViewChoice::Fit, "Fit"),
                    Choice::new(ViewChoice::Pixels, "1:1"),
                    Choice {
                        refused: Some("Zoom with the wheel over the picture".to_owned()),
                        ..Choice::new(ViewChoice::Zoomed, &zoom)
                    },
                ],
                3,
            );
            if choice != before {
                self.view = match choice {
                    ViewChoice::Fit => View::Fit,
                    ViewChoice::Pixels | ViewChoice::Zoomed => View::Pixels,
                };
            }
            let mut gain = f64::from(self.display_gain);
            let shown = format!("{gain:.2}x");
            if slider(
                ui,
                "Display gain",
                h,
                &mut gain,
                &Scale {
                    range: 0.25..=32.0,
                    step: 0.0,
                    logarithmic: true,
                },
                &shown,
            ) {
                self.display_gain = gain as f32;
            }
            caption(
                ui,
                "Brightness on screen only. Over the picture: the wheel zooms, dragging moves, \
                 a double click fits.",
            );
        });
    }

    // ── Timing strip ────────────────────────────────────────────────────────────────────────

    fn timing_strip(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Pipeline timing")
                    .font(theme::strong(BODY))
                    .color(TEXT),
            );
            ui.label(
                egui::RichText::new(
                    "Median of the last 2 s, once a second. Solid: measured; dashed: computed \
                     from the mode; whisker: slowest.",
                )
                .font(theme::sans(theme::SMALL))
                .color(MUTED),
            );
            widgets::from_right(ui, |ui| {
                let text = if self.expanded {
                    "Collapse timing"
                } else {
                    "Expand timing"
                };
                if chip(ui, text, self.expanded).clicked() {
                    self.expanded = !self.expanded;
                }
            });
        });
        ui.add_space(4.0);
        self.diagram.ribbon(ui);
        if self.expanded {
            ui.add_space(GAP + 4.0);
            widgets::rule(ui);
            ui.add_space(GAP);
            self.diagram.staircase(ui, STAIRCASE_HEIGHT);
        }
    }

    // ── Picture ─────────────────────────────────────────────────────────────────────────────

    fn picture(&mut self, ui: &mut Ui, s: &Stats, now: Instant) {
        match self.shown.clone() {
            Some(shown) => {
                picture::show(ui, &mut self.view, &shown, self.display_gain, &self.timing)
            }
            None => picture::empty(
                ui,
                "Waiting for frames",
                &format!("From {}", self.client.camera()),
            ),
        }
        if !s.connected(now) {
            let area = ui.max_rect();
            let since = s
                .last_answer
                .map_or("The camera never answered".to_owned(), |t| {
                    format!("No answer for {:.1} s", (now - t).as_secs_f64())
                });
            let text = format!(
                "Disconnected. {since}; asking again every {} ms.",
                RETRY_INTERVAL.as_millis()
            );
            let painter = ui.painter();
            let g = painter.layout_no_wrap(text, theme::strong(BODY), FAULT);
            let r = egui::Rect::from_center_size(
                area.center_top() + vec2(0.0, 28.0),
                g.size() + vec2(28.0, 14.0),
            );
            painter.rect(
                r,
                theme::RADIUS,
                theme::FAULT_FILL,
                Stroke::new(1.0, FAULT),
                egui::StrokeKind::Inside,
            );
            painter.galley(r.min + vec2(14.0, 7.0), g, FAULT);
        }
    }

    // ── Counters drawer ─────────────────────────────────────────────────────────────────────

    fn drawer(&self, ui: &mut Ui, s: &Stats, d: &DecodeStats) {
        let m = &self.meter;
        let st = s.state;
        let dash = || "-".to_owned();
        let ms = |t: Duration| t.as_secs_f64() * 1e3;
        section(ui, "Sensor", hue(Stage::Readout), |ui| {
            let Some(st) = st else {
                caption(ui, "No state from the board yet.");
                return;
            };
            let (c, md, n) = (st.camera, st.mode, st.counters);
            caption(
                ui,
                &format!(
                    "{:?} {}x{} {:?} at ({}, {}), {} ± {}",
                    c.format,
                    c.width,
                    c.height,
                    c.readout,
                    c.origin.map_or(0, |o| o.x),
                    c.origin.map_or(0, |o| o.y),
                    hz(c.rate_mhz),
                    hz(c.tolerance_mhz)
                ),
            );
            stat(ui, "Sensor clock", &format!("{} MHz", md.sensor_clock_mhz));
            stat(
                ui,
                "HTS / VTS / PSV",
                &format!("{} / {} / {}", md.hts, md.vts, on_off(md.psv)),
            );
            stat(ui, "Frame rate", &hz(md.frame_rate_mhz));
            stat(ui, "Rate error", &format!("{:+} mHz", md.rate_error_mhz));
            stat(ui, "Line time", &format!("{} ns", md.line_ns));
            stat(
                ui,
                "Longest exposure",
                &format!("{} lines", md.max_exposure_lines),
            );
            stat(ui, "Bytes per frame", &md.frame_bytes.to_string());
            stat(
                ui,
                "Exposure, gain",
                &format!(
                    "{} lines, {:.2}x, {:?}",
                    c.exposure_lines,
                    f32::from(c.gain_sixteenths) / 16.0,
                    c.test_pattern
                ),
            );
            stat(ui, "Captured", &n.frames_captured.to_string());
            stat_toned(
                ui,
                "Capture errors",
                &n.capture_errors.to_string(),
                n.capture_errors > 0,
            );
        });
        section(ui, "Encoder", hue(Stage::Encode), |ui| {
            stat(
                ui,
                "Board JPEG quality in use",
                &st.map_or_else(dash, |st| {
                    format!(
                        "{} (set {}, automatic {})",
                        st.encoder.quality,
                        st.stream.jpeg_quality,
                        on_off(st.stream.auto_quality)
                    )
                }),
            );
            stat(
                ui,
                "Board encode time",
                &st.map_or_else(dash, |st| {
                    format!("{:.2} ms", f64::from(st.encoder.encode_us) / 1e3)
                }),
            );
            stat(
                ui,
                "Last JPEG",
                &st.map_or_else(dash, |st| format!("{} B", st.encoder.frame_len)),
            );
            let errors = st.map_or(0, |st| st.encoder.errors);
            stat_toned(
                ui,
                "Board encoder errors",
                &st.map_or_else(dash, |_| errors.to_string()),
                errors > 0,
            );
            stat(
                ui,
                "Skipped by board encoder (busy)",
                &st.map_or_else(dash, |st| st.encoder.frames_skipped.to_string()),
            );
        });
        section(ui, "Link", hue(Stage::Send), |ui| {
            let jpeg = st.filter(|st| st.stream.encoding == Encoding::Jpeg);
            stat(ui, "Received whole", &format!("{:.1} fps", m.rx_fps));
            stat(ui, "Throughput", &format!("{:.1} Mbit/s", m.mbps));
            stat(
                ui,
                "Compressed frame, last / mean",
                &jpeg.filter(|_| m.mean_len > 0.0).map_or_else(dash, |_| {
                    format!("{} B / {:.0} B", s.last_frame_len, m.mean_len)
                }),
            );
            stat(
                ui,
                "Compression ratio",
                &jpeg.filter(|_| m.mean_len > 0.0).map_or_else(dash, |st| {
                    let raw = f64::from(st.camera.width) * f64::from(st.camera.height);
                    format!("{:.1} : 1", raw / m.mean_len)
                }),
            );
            stat(
                ui,
                "Sent by the board",
                &st.map_or_else(dash, |st| st.counters.frames_sent.to_string()),
            );
            stat(
                ui,
                "Skipped by board (link busy)",
                &st.map_or_else(dash, |st| st.counters.frames_skipped.to_string()),
            );
            stat(
                ui,
                "Datagrams sent",
                &st.map_or_else(dash, |st| st.counters.datagrams_sent.to_string()),
            );
            stat_toned(
                ui,
                "Dropped, chunk missing",
                &s.frames_incomplete.to_string(),
                s.frames_incomplete > 0,
            );
            stat(ui, "Cut by a mode change", &s.frames_cut.to_string());
            stat(
                ui,
                "Request round trip",
                &s.rtt.map_or_else(dash, |d| format!("{:.2} ms", ms(d))),
            );
            caption(
                ui,
                &format!(
                    "Camera {}, local {}, receive buffer {} KiB",
                    self.client.camera(),
                    self.client.local_addr(),
                    self.client.rcvbuf() / 1024
                ),
            );
        });
        section(ui, "Decode", hue(Stage::Decode), |ui| {
            stat(ui, "Decoded", &format!("{:.1} fps", m.decoded_fps));
            stat(
                ui,
                "Decode time, last / mean",
                &format!("{:.2} ms / {:.2} ms", ms(d.decode_last), m.decode_ms),
            );
            stat_toned(
                ui,
                "Decode failures",
                &d.failures.to_string(),
                d.failures > 0,
            );
        });
        section(ui, "Display", hue(Stage::Draw), |ui| {
            stat(ui, "Presented", &format!("{:.1} fps", m.shown_fps));
            stat(ui, "Replaced before shown", &s.frames_replaced.to_string());
            stat(
                ui,
                "Frame",
                &self.shown.as_ref().map_or_else(dash, |sh| {
                    let f = &sh.frame;
                    match sh.jpeg_len {
                        Some(n) => format!("{}x{} JPEG, {n} B", f.width, f.height),
                        None => format!(
                            "{}x{} {:?}, {} B",
                            f.width,
                            f.height,
                            f.encoding,
                            f.data.len()
                        ),
                    }
                }),
            );
            stat(
                ui,
                "Number, generation",
                &self.shown.as_ref().map_or_else(dash, |sh| {
                    format!("#{}, {}", sh.frame.number, sh.frame.generation)
                }),
            );
            stat(ui, "UI frame time", &format!("{:.2} ms", m.cpu_ms));
        });
        section(ui, "Board", NEUTRAL, |ui| {
            stat(
                ui,
                "Generation",
                &st.map_or_else(dash, |st| st.generation.to_string()),
            );
            stat(
                ui,
                "Uptime",
                &st.map_or_else(dash, |st| format!("{:.1} s", st.uptime_us as f64 / 1e6)),
            );
            stat(
                ui,
                "Stream",
                &st.map_or_else(dash, |st| {
                    format!(
                        "{}, {:?}, {}",
                        on_off(st.stream.enabled),
                        st.stream.encoding,
                        match st.stream.max_fps {
                            0 => "no limit".to_owned(),
                            n => format!("at most {n} fps"),
                        }
                    )
                }),
            );
        });
    }
}

/// Faults since start: frames dropped for a missing chunk, capture and encoder errors, decode
/// failures.
fn faults(s: &Stats, d: &DecodeStats) -> u64 {
    let board = s.state.map_or(0, |st| {
        u64::from(st.counters.capture_errors) + u64::from(st.encoder.errors)
    });
    s.frames_incomplete + board + d.failures
}

/// The link's state: a dot and a word, the camera's address under it.
fn link_pill(ui: &mut Ui, s: &Stats, now: Instant, client: &Client) {
    let up = s.connected(now);
    let (word, color) = if up {
        ("Connected", hue(Stage::Readout).line)
    } else {
        ("Disconnected", FAULT)
    };
    let painter = ui.painter().clone();
    let word_g = painter.layout_no_wrap(word.to_owned(), theme::strong(BODY), TEXT);
    let addr = client.camera().to_string();
    let addr_g = painter.layout_no_wrap(addr, theme::mono(theme::SMALL), MUTED);
    let w = 16.0 + word_g.size().x.max(addr_g.size().x);
    let (rect, response) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo {
        label: Some("Link".to_owned()),
        current_text_value: Some(word.to_owned()),
        ..egui::WidgetInfo::new(egui::Role::Status)
    });
    painter.circle_filled(rect.left_top() + vec2(4.0, 9.0), 4.0, color);
    painter.galley(rect.left_top() + vec2(16.0, 0.0), word_g, TEXT);
    painter.galley(
        egui::pos2(rect.left() + 16.0, rect.bottom() - addr_g.size().y),
        addr_g,
        MUTED,
    );
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let now = Instant::now();
        self.passes += 1;
        if let Some(cpu) = frame.info().cpu_usage {
            self.meter.cpu_sum += f64::from(cpu);
            self.meter.cpu_n += 1;
            if self.cpu.len() < 1 << 20 {
                self.cpu.push(cpu);
            }
        }
        self.drawn(now);
        if let Some(p) = self.decoder.take() {
            self.show(p, now);
        }
        let stats = self.client.stats();
        let dstats = self.decoder.stats();
        if let Some(st) = stats.state
            && st.uptime_us != self.uptime_us
        {
            // A new state. After the link was lost or the board restarted, the board's
            // settings may not be the editors' any more.
            self.adopt |= self.lost || st.uptime_us < self.uptime_us;
            (self.lost, self.uptime_us) = (false, st.uptime_us);
        }
        self.lost |= stats.state.is_some() && !stats.connected(now);
        if self.adopt
            && let Some(st) = stats.state
        {
            self.edit = st.camera;
            self.centred = st.camera.origin.is_none();
            self.origin = st.camera.origin.unwrap_or(Origin { x: 0, y: 0 });
            self.stream.adopt(st.stream);
            self.adopt = false;
        }
        self.meter.update(now, &stats, &dstats, self.presented);
        self.time_pipeline(ui.ctx(), now, &stats);

        let bar = |fill| {
            egui::Frame::new()
                .fill(fill)
                .inner_margin(Margin::symmetric(PAD as i8, 8))
        };
        egui::Panel::top("status")
            .resizable(false)
            .exact_size(widgets::bar_height() + 8.0)
            .frame(bar(theme::PANEL).stroke(Stroke::new(1.0, theme::LINE)))
            .show(ui, |ui| self.status_bar(ui, &stats, now));
        egui::Panel::left("rail")
            .resizable(false)
            .exact_size(RAIL_WIDTH)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::symmetric(PAD as i8, PAD as i8)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(Margin {
                                right: 10,
                                ..Margin::ZERO
                            })
                            .show(ui, |ui| self.rail(ui, &stats, &dstats));
                    });
            });
        if self.drawer {
            egui::Panel::right("counters")
                .resizable(false)
                .exact_size(DRAWER_WIDTH)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::symmetric(PAD as i8, PAD as i8)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| self.drawer(ui, &stats, &dstats));
                });
        }
        egui::Panel::bottom("timing")
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin {
                left: PAD as i8 + 4,
                right: PAD as i8 + 4,
                top: 10,
                bottom: 12,
            }))
            .show(ui, |ui| self.timing_strip(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::WELL))
            .show(ui, |ui| self.picture(ui, &stats, now));
        if let Some(t) = &mut self.selftest {
            let upload = *self.timing.lock().unwrap_or_else(|e| e.into_inner());
            t.step(
                ui.ctx(),
                now,
                Probe {
                    stats: &stats,
                    decode: dstats,
                    presented: self.presented,
                    passes: self.passes,
                    upload,
                    cpu: &mut self.cpu,
                    shown: self.shown.as_deref(),
                    pipeline: self.pipeline.snapshot(),
                },
            );
        }
        // Keeps the status fresh when no frames come.
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}
