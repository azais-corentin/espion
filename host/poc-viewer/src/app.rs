//! The window: controls and status in a side panel, the image in the rest.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use eframe::egui::{self, Color32, RichText};
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
};

use crate::display::{self, ImageCallback, Shown, UploadTiming};

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

/// `--selftest`.
pub struct SelfTestConfig {
    pub secs: f64,
    pub screenshot: PathBuf,
    pub vsync: bool,
}

/// Width of the side panel, points: one column of settings and status; the picture takes the
/// rest of the window.
const PANEL_WIDTH: f32 = 390.0;

const RED: Color32 = Color32::from_rgb(235, 70, 60);
const GREEN: Color32 = Color32::from_rgb(90, 200, 90);
const ORANGE: Color32 = Color32::from_rgb(240, 160, 40);

/// Rates over the last second.
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
        self.decoded_fps = (d.decoded + d.failures - self.decoded) as f64 / dt;
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
        (self.at, self.whole, self.bytes, self.presented) =
            (now, s.frames_whole, s.bytes, presented);
        (self.jpeg_whole, self.jpeg_bytes) = (s.jpeg_whole, s.jpeg_bytes);
        (self.decoded, self.decode_total) = (d.decoded + d.failures, d.decode_total);
        (self.cpu_sum, self.cpu_n) = (0.0, 0);
    }
}

/// Measurement base of the self-test.
struct Base {
    at: Instant,
    stats: Stats,
    decode: DecodeStats,
    presented: u64,
    passes: u64,
    timing: UploadTiming,
}

struct SelfTest {
    cfg: SelfTestConfig,
    start: Instant,
    warmup: Duration,
    base: Option<Base>,
    requested: Option<Instant>,
    saved: Arc<Mutex<Option<String>>>,
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
    fit: bool,
    display_gain: f32,
    shown: Option<Arc<Shown>>,
    serial: u64,
    timing: Arc<Mutex<UploadTiming>>,
    presented: u64,
    passes: u64,
    cpu: Vec<f32>,
    meter: Meter,
    selftest: Option<SelfTest>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, setup: Setup) -> Result<Self> {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .ok_or_else(|| anyhow!("the wgpu renderer is not available"))?;
        display::init(rs);
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
            fit: !setup.one_to_one,
            display_gain: setup.display_gain,
            shown: None,
            serial: 0,
            timing: Arc::default(),
            presented: 0,
            passes: 0,
            cpu: Vec::new(),
            meter: Meter::new(),
            selftest: setup.selftest.map(|cfg| SelfTest {
                warmup: Duration::from_secs_f64((cfg.secs / 3.0).min(2.0)),
                cfg,
                start: now,
                base: None,
                requested: None,
                saved: Arc::default(),
            }),
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

    fn show(&mut self, p: Picture) {
        self.serial += 1;
        self.presented += 1;
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
            });
        }
    }

    /// One column: what is happening (link, status, the board's last refusal), the settings
    /// (mode, controls, stream, display), then the board's detailed state, collapsed.
    fn panel(&mut self, ui: &mut egui::Ui, s: &Stats, d: &DecodeStats, now: Instant) {
        self.link(ui, s, now);
        self.status(ui, s, d);
        answer_line(ui, s);
        ui.separator();
        self.mode_editor(ui, s);
        ui.separator();
        self.controls(ui, s);
        ui.separator();
        self.stream_editor(ui, s);
        ui.separator();
        self.display_controls(ui);
        ui.separator();
        egui::CollapsingHeader::new("Board: as applied")
            .default_open(false)
            .show(ui, |ui| self.board(ui, s));
    }

    fn link(&self, ui: &mut egui::Ui, s: &Stats, now: Instant) {
        if s.connected(now) {
            ui.colored_label(GREEN, RichText::new("Connected").strong().size(18.0));
        } else {
            ui.colored_label(RED, RichText::new("DISCONNECTED").strong().size(18.0));
            let since = s.last_answer.map_or("never answered".to_owned(), |t| {
                format!("no answer for {:.1} s", (now - t).as_secs_f64())
            });
            ui.colored_label(
                RED,
                format!("{since}; retrying every {} ms", RETRY_INTERVAL.as_millis()),
            );
        }
        ui.label(format!("camera {}", self.client.camera()));
        ui.label(format!(
            "local {}, receive buffer {} KiB",
            self.client.local_addr(),
            self.client.rcvbuf() / 1024
        ));
    }

    fn status(&self, ui: &mut egui::Ui, s: &Stats, d: &DecodeStats) {
        let m = &self.meter;
        let board = s.state.map(|st| st.counters);
        let jpeg = s.state.filter(|st| st.stream.encoding == Encoding::Jpeg);
        let ms = |t: Duration| t.as_secs_f64() * 1e3;
        egui::Grid::new("status").num_columns(2).show(ui, |ui| {
            let mut row = |k: &str, v: String| row(ui, k, v);
            row("Received whole", format!("{:.1} fps", m.rx_fps));
            row("Decoded", format!("{:.1} fps", m.decoded_fps));
            row("Presented", format!("{:.1} fps", m.shown_fps));
            row("Throughput", format!("{:.1} Mbit/s", m.mbps));
            row(
                "Compressed frame, last / mean",
                jpeg.filter(|_| m.mean_len > 0.0).map_or("-".into(), |_| {
                    format!("{} B / {:.0} B", s.last_frame_len, m.mean_len)
                }),
            );
            row(
                "Compression ratio",
                jpeg.filter(|_| m.mean_len > 0.0).map_or("-".into(), |st| {
                    let raw = st.camera.width as f64 * st.camera.height as f64;
                    format!("{:.1} : 1", raw / m.mean_len)
                }),
            );
            row(
                "Decode time, last / mean",
                format!("{:.2} ms / {:.2} ms", ms(d.decode_last), m.decode_ms),
            );
            row("Decode failures", d.failures.to_string());
            row(
                "Board JPEG quality in use",
                s.state.map_or("-".into(), |st| {
                    format!(
                        "{} (set {}, automatic {})",
                        st.encoder.quality,
                        st.stream.jpeg_quality,
                        on_off(st.stream.auto_quality)
                    )
                }),
            );
            row(
                "Board encode time",
                s.state.map_or("-".into(), |st| {
                    format!("{:.2} ms", st.encoder.encode_us as f64 / 1e3)
                }),
            );
            row(
                "Board encoder errors",
                s.state
                    .map_or("-".into(), |st| st.encoder.errors.to_string()),
            );
            row(
                "Skipped by board encoder (busy)",
                s.state
                    .map_or("-".into(), |st| st.encoder.frames_skipped.to_string()),
            );
            row("Dropped, chunk missing", s.frames_incomplete.to_string());
            row("Cut by a mode change", s.frames_cut.to_string());
            row("Replaced before shown", s.frames_replaced.to_string());
            row(
                "Skipped by board (link busy)",
                board.map_or("-".into(), |c| c.frames_skipped.to_string()),
            );
            row(
                "Capture errors",
                board.map_or("-".into(), |c| c.capture_errors.to_string()),
            );
            row(
                "Request round trip",
                s.rtt
                    .map_or("-".into(), |d| format!("{:.2} ms", d.as_secs_f64() * 1e3)),
            );
            row(
                "Frame",
                self.shown.as_ref().map_or("-".into(), |sh| {
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
            row(
                "Number, generation",
                self.shown.as_ref().map_or("-".into(), |sh| {
                    format!("#{}, {}", sh.frame.number, sh.frame.generation)
                }),
            );
            row("UI frame time", format!("{:.2} ms", m.cpu_ms));
        });
        if let Some(e) = &d.last_failure {
            ui.colored_label(RED, format!("Last decode failure: {e}"));
        }
    }

    fn mode_editor(&mut self, ui: &mut egui::Ui, s: &Stats) {
        ui.heading("Sensor mode");
        let e = &mut self.edit;
        ui.horizontal(|ui| {
            ui.label("Format");
            ui.selectable_value(&mut e.format, Format::Raw8, "sensor RAW8");
            ui.selectable_value(&mut e.format, Format::Raw10, "sensor RAW10");
        });
        ui.horizontal(|ui| {
            ui.label("Readout");
            ui.selectable_value(&mut e.readout, Readout::Crop, "crop");
            ui.selectable_value(&mut e.readout, Readout::Binned, "2x2 binned");
        });
        ui.horizontal(|ui| {
            labelled(
                ui,
                "Width",
                egui::DragValue::new(&mut e.width).range(0..=2048),
            );
            labelled(
                ui,
                "Height",
                egui::DragValue::new(&mut e.height).range(0..=2048),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.centred, "centred origin");
            ui.add_enabled_ui(!self.centred, |ui| {
                labelled(ui, "Origin x", egui::DragValue::new(&mut self.origin.x));
                labelled(ui, "Origin y", egui::DragValue::new(&mut self.origin.y));
            });
        });
        let mut rate = e.rate_mhz as f64 / 1000.0;
        let mut tol = e.tolerance_mhz as f64 / 1000.0;
        ui.horizontal_wrapped(|ui| {
            labelled(
                ui,
                "Rate, Hz",
                egui::DragValue::new(&mut rate)
                    .range(0.0..=2000.0)
                    .speed(0.1)
                    .fixed_decimals(3),
            );
            labelled(
                ui,
                "Tolerance, Hz",
                egui::DragValue::new(&mut tol)
                    .range(0.0..=1000.0)
                    .speed(0.01)
                    .fixed_decimals(3),
            );
        });
        e.rate_mhz = (rate * 1000.0).round() as u32;
        e.tolerance_mhz = (tol * 1000.0).round() as u32;

        // The driver's verdict on the edited mode, before anything is sent, with the encoding
        // the stream will have in it.
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
            ui.colored_label(
                ORANGE,
                format!(
                    "Applying moves the stream from {:?} to RAW8: {e}",
                    self.stream.encoding
                ),
            );
        }
        match &local {
            Ok(m) => {
                let info = camera::mode_info(m);
                clock_line(ui, info.sensor_clock_mhz, info.exceeds_datasheet_clock);
                ui.label(format!(
                    "HTS {}, VTS {}, PSV {}: {} ({:+} mHz)",
                    info.hts,
                    info.vts,
                    on_off(info.psv),
                    hz(info.frame_rate_mhz),
                    info.rate_error_mhz
                ));
                ui.label(format!(
                    "Longest exposure {} lines ({:.2} ms)",
                    info.max_exposure_lines,
                    info.max_exposure_lines as f64 * info.line_ns as f64 / 1e6
                ));
            }
            Err(err) => {
                ui.colored_label(RED, format!("Refused locally: {err}"));
            }
        }
        let (max_w, max_h) = ov9281::limits::max_size(camera::readout(req.readout));
        if (128..=max_w).contains(&req.width) && (2..=max_h).contains(&req.height) {
            let (f80, f96) = camera::fastest_rates(req.format, req.width, req.height, req.readout);
            ui.label(format!(
                "Fastest {} at 80 MHz, {} at 96 MHz; slowest {}",
                hz(f80),
                hz(f96),
                hz(camera::slowest_rate())
            ));
        }
        ui.label(format!(
            "Sizes 128..={max_w} x 2..={max_h}; width multiple of {}, height of {}",
            ov9281::limits::width_multiple(camera::format(req.format)),
            ov9281::limits::height_multiple(camera::format(req.format))
        ));
        let applied = s.state.map(|st| st.camera);
        if applied.is_some_and(|a| camera::build_mode(&a).ok() != local.ok()) {
            ui.label("Edited: differs from the board's mode");
        }
        if ui
            .add_enabled(local.is_ok(), egui::Button::new("Apply mode"))
            .clicked()
            && let Ok(m) = local
        {
            self.edit.exposure_lines = self.edit.exposure_lines.clamp(1, m.max_exposure_lines());
            if stream != self.stream {
                self.stream = stream;
                self.client.send(Command::SetStream(self.stream.settings()));
            }
            self.client.send(Command::SetCamera(self.request()));
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, s: &Stats) {
        ui.heading("Controls (applied at once)");
        let max = s
            .state
            .map(|st| st.mode.max_exposure_lines)
            .or_else(|| {
                camera::build_mode(&self.request())
                    .ok()
                    .map(|m| m.max_exposure_lines())
            })
            .unwrap_or(1);
        let e = &mut self.edit;
        let mut changed = false;
        changed |= ui
            .add(egui::Slider::new(&mut e.exposure_lines, 1..=max.max(1)).text("exposure, lines"))
            .changed();
        if let Some(st) = s.state {
            ui.label(format!(
                "Exposure time {:.3} ms",
                e.exposure_lines as f64 * st.mode.line_ns as f64 / 1e6
            ));
        }
        changed |= ui
            .add(
                egui::Slider::new(
                    &mut e.gain_sixteenths,
                    ov9281::limits::GAIN_MIN..=ov9281::limits::GAIN_MAX,
                )
                .text("gain, 1/16")
                .custom_formatter(|v, _| format!("{v} ({:.2}x)", v / 16.0)),
            )
            .changed();
        egui::ComboBox::from_label("test pattern")
            .selected_text(format!("{:?}", e.test_pattern))
            .show_ui(ui, |ui| {
                for p in [
                    TestPattern::Off,
                    TestPattern::Bars,
                    TestPattern::RollingBars,
                    TestPattern::GreySquares,
                    TestPattern::BlackWhiteSquares,
                    TestPattern::Random,
                ] {
                    changed |= ui
                        .selectable_value(&mut e.test_pattern, p, format!("{p:?}"))
                        .changed();
                }
            });
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
    }

    fn stream_editor(&mut self, ui: &mut egui::Ui, s: &Stats) {
        ui.heading("Stream (applied at once)");
        // The running mode decides which encodings can be sent.
        let mode = s.state.map_or_else(|| self.request(), |st| st.camera);
        let st = &mut self.stream;
        let mut changed = ui.checkbox(&mut st.enabled, "send frames").changed();
        ui.horizontal_wrapped(|ui| {
            ui.label("Encoding");
            for (e, label) in [
                (Encoding::Jpeg, "send JPEG"),
                (Encoding::Raw8, "send RAW8"),
                (Encoding::Raw10, "send RAW10 packed"),
            ] {
                let allowed = e.check(mode.format, mode.width, mode.height);
                let r = ui.add_enabled(
                    allowed.is_ok(),
                    egui::Button::selectable(st.encoding == e, label),
                );
                let r = match allowed {
                    Ok(()) => r,
                    Err(err) => r.on_disabled_hover_text(err.to_string()),
                };
                if r.clicked() && st.encoding != e {
                    st.encoding = e;
                    changed = true;
                }
            }
        });
        if let Err(e) = Encoding::Jpeg.check(mode.format, mode.width, mode.height) {
            ui.colored_label(ORANGE, format!("JPEG not available: {e}"));
        }
        if st.auto_quality {
            // The board chooses: the slider shows the quality it has in use.
            let mut in_use = s.state.map_or(AUTO_QUALITY_CEILING, |b| b.encoder.quality);
            ui.add_enabled(
                false,
                egui::Slider::new(&mut in_use, JPEG_QUALITY_MIN..=JPEG_QUALITY_MAX)
                    .text("JPEG quality"),
            );
        } else {
            changed |= ui
                .add(
                    egui::Slider::new(&mut st.manual_quality, JPEG_QUALITY_MIN..=JPEG_QUALITY_MAX)
                        .text("JPEG quality"),
                )
                .changed();
        }
        changed |= ui
            .checkbox(&mut st.auto_quality, "automatic JPEG quality")
            .on_hover_text(format!(
                "The board chooses the quality, up to {AUTO_QUALITY_CEILING}: it lowers it while \
                 frames are skipped because the link is busy and raises it back when there is \
                 room. The quality set on the slider applies with this off."
            ))
            .changed();
        if st.auto_quality {
            ui.label(format!(
                "The board chooses the JPEG quality, up to {AUTO_QUALITY_CEILING}"
            ));
        }
        changed |= labelled(
            ui,
            "Frames per second at most (0: no limit)",
            egui::DragValue::new(&mut st.max_fps).range(0..=2000),
        )
        .changed();
        if changed {
            self.client.send(Command::SetStream(st.settings()));
        }
    }

    fn display_controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Display");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.fit, true, "fit to window");
            ui.selectable_value(&mut self.fit, false, "1:1 pixels");
        });
        ui.add(
            egui::Slider::new(&mut self.display_gain, 0.25..=32.0)
                .logarithmic(true)
                .text("display gain"),
        );
    }

    fn board(&self, ui: &mut egui::Ui, s: &Stats) {
        let Some(st) = s.state else {
            ui.label("no state yet");
            return;
        };
        let (c, m, n) = (st.camera, st.mode, st.counters);
        ui.label(format!(
            "{:?} {}x{} {:?} at ({}, {}), {} ± {}",
            c.format,
            c.width,
            c.height,
            c.readout,
            c.origin.map_or(0, |o| o.x),
            c.origin.map_or(0, |o| o.y),
            hz(c.rate_mhz),
            hz(c.tolerance_mhz)
        ));
        clock_line(ui, m.sensor_clock_mhz, m.exceeds_datasheet_clock);
        egui::Grid::new("board").num_columns(2).show(ui, |ui| {
            let mut row = |k: &str, v: String| row(ui, k, v);
            row(
                "HTS / VTS / PSV",
                format!("{} / {} / {}", m.hts, m.vts, on_off(m.psv)),
            );
            row("Frame rate", hz(m.frame_rate_mhz));
            row("Rate error", format!("{:+} mHz", m.rate_error_mhz));
            row("Line time", format!("{} ns", m.line_ns));
            row(
                "Longest exposure",
                format!("{} lines", m.max_exposure_lines),
            );
            row("Bytes per frame", m.frame_bytes.to_string());
            row(
                "Exposure, gain",
                format!(
                    "{} lines, {:.2}x, {:?}",
                    c.exposure_lines,
                    c.gain_sixteenths as f32 / 16.0,
                    c.test_pattern
                ),
            );
            row(
                "Stream",
                format!(
                    "{}, {:?}, {}",
                    if st.stream.enabled { "on" } else { "off" },
                    st.stream.encoding,
                    match st.stream.max_fps {
                        0 => "no limit".to_owned(),
                        n => format!("at most {n} fps"),
                    }
                ),
            );
            row(
                "Quality set, automatic, in use",
                format!(
                    "set {}, automatic {}, in use {}",
                    st.stream.jpeg_quality,
                    on_off(st.stream.auto_quality),
                    st.encoder.quality
                ),
            );
            row(
                "Last JPEG, encode time",
                format!(
                    "{} B, {:.3} ms",
                    st.encoder.frame_len,
                    st.encoder.encode_us as f64 / 1e3
                ),
            );
            row(
                "Encoder errors / skipped",
                format!("{} / {}", st.encoder.errors, st.encoder.frames_skipped),
            );
            row("Generation", st.generation.to_string());
            row(
                "Captured / sent",
                format!("{} / {}", n.frames_captured, n.frames_sent),
            );
            row(
                "Skipped / errors",
                format!("{} / {}", n.frames_skipped, n.capture_errors),
            );
            row("Datagrams sent", n.datagrams_sent.to_string());
            row("Uptime", format!("{:.1} s", st.uptime_us as f64 / 1e6));
        });
    }

    fn image(&mut self, ui: &mut egui::Ui, s: &Stats, now: Instant) {
        let Some(shown) = self.shown.clone() else {
            ui.centered_and_justified(|ui| ui.label("waiting for frames…"));
            return;
        };
        let (w, h) = (shown.frame.width as f32, shown.frame.height as f32);
        let gain = self.display_gain;
        let timing = self.timing.clone();
        let paint = |ui: &mut egui::Ui, rect: egui::Rect| {
            let vis = rect.intersect(ui.clip_rect());
            if vis.width() <= 0.0 || vis.height() <= 0.0 {
                return;
            }
            let uv0 = (vis.min - rect.min) / rect.size();
            let uv1 = (vis.max - rect.min) / rect.size();
            ui.painter().add(
                ImageCallback {
                    shown: shown.clone(),
                    uv: [uv0.x, uv0.y, uv1.x, uv1.y],
                    gain,
                    timing: timing.clone(),
                }
                .paint(vis),
            );
        };
        let area = ui.available_rect_before_wrap();
        if self.fit {
            let scale = (area.width() / w).min(area.height() / h);
            let rect = egui::Rect::from_center_size(area.center(), egui::vec2(w, h) * scale);
            paint(ui, rect);
        } else {
            let ppp = ui.ctx().pixels_per_point();
            egui::ScrollArea::both().show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(w, h) / ppp, egui::Sense::hover());
                paint(ui, rect);
            });
        }
        if !s.connected(now) {
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "DISCONNECTED",
                egui::FontId::proportional(48.0),
                RED,
            );
        }
    }

    fn selftest_step(&mut self, ctx: &egui::Context, now: Instant, s: &Stats) {
        let Some(t) = &mut self.selftest else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(50));
        let elapsed = now - t.start;
        if t.base.is_none() && elapsed >= t.warmup {
            t.base = Some(Base {
                at: now,
                stats: s.clone(),
                decode: self.decoder.stats(),
                presented: self.presented,
                passes: self.passes,
                timing: *self.timing.lock().unwrap_or_else(|e| e.into_inner()),
            });
            self.cpu.clear();
        }
        if let Some(asked) = t.requested {
            if let Some(msg) = t.saved.lock().unwrap_or_else(|e| e.into_inner()).take() {
                println!("{msg}");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if now - asked > Duration::from_secs(5) {
                println!("screenshot: none after 5 s");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if elapsed.as_secs_f64() < t.cfg.secs {
            return;
        }
        let Some(b) = &t.base else {
            return;
        };
        let timing = *self.timing.lock().unwrap_or_else(|e| e.into_inner());
        println!(
            "{}",
            report(
                b,
                now,
                s,
                &self.decoder.stats(),
                self.presented,
                self.passes,
                &timing,
                &mut self.cpu,
                t.cfg.vsync,
                self.shown.as_deref()
            )
        );
        let path = t.cfg.screenshot.clone();
        let saved = t.saved.clone();
        ctx.request_screenshot(move |image| {
            let msg = match save_png(&path, &image) {
                Ok(()) => format!(
                    "screenshot: {} ({}x{})",
                    path.display(),
                    image.size[0],
                    image.size[1]
                ),
                Err(e) => format!("screenshot: {e:#}"),
            };
            *saved.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
        });
        t.requested = Some(now);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let now = Instant::now();
        self.passes += 1;
        if let Some(cpu) = frame.info().cpu_usage {
            self.meter.cpu_sum += cpu as f64;
            self.meter.cpu_n += 1;
            if self.cpu.len() < 1 << 20 {
                self.cpu.push(cpu);
            }
        }
        if let Some(p) = self.decoder.take() {
            self.show(p);
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

        egui::Panel::left("panel")
            .resizable(false)
            .exact_size(PANEL_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.panel(ui, &stats, &dstats, now));
            });
        egui::CentralPanel::default().show(ui, |ui| self.image(ui, &stats, now));
        self.selftest_step(ui.ctx(), now, &stats);
        // Keeps the status fresh when no frames come.
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

/// The board's latest refusal (until a request of the same kind succeeds), and whether a
/// request waits for its answer.
fn answer_line(ui: &mut egui::Ui, s: &Stats) {
    if let Some(r) = &s.refusal {
        let what = match r.op {
            Op::SetCamera(_) => "camera settings",
            Op::SetStream(_) => "stream settings",
            Op::GetState | Op::Poll => "request",
        };
        ui.colored_label(RED, format!("Refused the {what}: {}", r.error));
    }
    if s.pending {
        ui.label("waiting for an answer…");
    }
}

/// A grid row: the name, then the value as text labelled by the name (for AccessKit).
fn row(ui: &mut egui::Ui, name: &str, value: String) {
    let id = ui.label(name).id;
    ui.label(value).labelled_by(id);
    ui.end_row();
}

/// `widget` after a label naming it, linked for AccessKit.
fn labelled(ui: &mut egui::Ui, name: &str, widget: impl egui::Widget) -> egui::Response {
    let id = ui.label(name).id;
    ui.add(widget).labelled_by(id)
}

fn clock_line(ui: &mut egui::Ui, mhz: u8, above: bool) {
    if above {
        ui.colored_label(
            ORANGE,
            format!("Sensor clock {mhz} MHz: above the datasheet's 80 MHz"),
        );
    } else {
        ui.label(format!("Sensor clock {mhz} MHz"));
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn hz(mhz: u32) -> String {
    format!("{}.{:03} Hz", mhz / 1000, mhz % 1000)
}

fn percentile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[allow(clippy::too_many_arguments)]
fn report(
    b: &Base,
    now: Instant,
    s: &Stats,
    dz: &DecodeStats,
    presented: u64,
    passes: u64,
    timing: &UploadTiming,
    cpu: &mut [f32],
    vsync: bool,
    shown: Option<&Shown>,
) -> String {
    let dt = (now - b.at).as_secs_f64();
    let d = |a: u64, z: u64| a - z;
    cpu.sort_by(f32::total_cmp);
    let ms = |v: f32| v as f64 * 1e3;
    let mean = cpu.iter().map(|&v| v as f64).sum::<f64>() / cpu.len().max(1) as f64 * 1e3;
    let uploads = timing.count - b.timing.count;
    let upload_mean = (timing.total - b.timing.total).as_secs_f64() * 1e3 / uploads.max(1) as f64;
    let board = match (b.stats.state, s.state) {
        (Some(a), Some(z)) => {
            let (e, ez) = (a.encoder, z.encoder);
            let (a, z) = (a.counters, z.counters);
            format!(
                "board: captured {}, sent {}, skipped (link busy) {}, capture errors {}, datagrams {}\n\
                 board encoder: quality in use {} (set {}, automatic {}), last JPEG {} B, encode {:.3} ms, \
                 errors {}, skipped (busy) {}",
                z.frames_captured.wrapping_sub(a.frames_captured),
                z.frames_sent.wrapping_sub(a.frames_sent),
                z.frames_skipped.wrapping_sub(a.frames_skipped),
                z.capture_errors.wrapping_sub(a.capture_errors),
                z.datagrams_sent.wrapping_sub(a.datagrams_sent),
                ez.quality,
                s.state.map_or(0, |st| st.stream.jpeg_quality),
                on_off(s.state.is_some_and(|st| st.stream.auto_quality)),
                ez.frame_len,
                ez.encode_us as f64 / 1e3,
                ez.errors.wrapping_sub(e.errors),
                ez.frames_skipped.wrapping_sub(e.frames_skipped),
            )
        }
        _ => "board: no state".to_owned(),
    };
    let jpeg = d(s.jpeg_whole, b.stats.jpeg_whole);
    let mean_len = d(s.jpeg_bytes, b.stats.jpeg_bytes) as f64 / jpeg.max(1) as f64;
    let decoded = (dz.decoded + dz.failures) - (b.decode.decoded + b.decode.failures);
    let decode_mean =
        (dz.decode_total - b.decode.decode_total).as_secs_f64() * 1e3 / decoded.max(1) as f64;
    let raw_len = s
        .state
        .map_or(0.0, |st| st.camera.width as f64 * st.camera.height as f64);
    let decoding = format!(
        "JPEG frames: {jpeg} whole, mean {mean_len:.0} B (compression {:.1} : 1 against RAW8)\n\
         decoded: {:.1} fps, decode mean {decode_mean:.3} ms, max {:.3} ms, {} failures, {} raw passed",
        raw_len / mean_len.max(1.0),
        decoded as f64 / dt,
        dz.decode_max.as_secs_f64() * 1e3,
        dz.failures - b.decode.failures,
        dz.passed - b.decode.passed,
    );
    let mode = s.state.map_or("mode: none".to_owned(), |st| {
        format!(
            "mode: {:?} {}x{} {:?}, {} MHz HTS {} VTS {} PSV {}, {}; stream {:?} max_fps {}",
            st.camera.format,
            st.camera.width,
            st.camera.height,
            st.camera.readout,
            st.mode.sensor_clock_mhz,
            st.mode.hts,
            st.mode.vts,
            on_off(st.mode.psv),
            hz(st.mode.frame_rate_mhz),
            st.stream.encoding,
            st.stream.max_fps
        )
    });
    let frame = shown.map_or("frame: none".to_owned(), |sh| {
        let f = &sh.frame;
        match sh.jpeg_len {
            Some(n) => format!("frame: {}x{} JPEG {n} B, shown decoded", f.width, f.height),
            None => format!(
                "frame: {}x{} {:?} {} B",
                f.width,
                f.height,
                f.encoding,
                f.data.len()
            ),
        }
    });
    let refusal = s.refusal.map_or("refusal: none".to_owned(), |r| {
        format!("refusal: {}", r.error)
    });
    format!(
        "selftest: vsync {}, measured over {dt:.2} s\n{mode}\n{frame}\n\
         received: {:.1} fps whole, {:.1} Mbit/s UDP payload, {} dropped (chunk missing), \
         {} cut by a mode change, {} replaced before shown, {} stale chunks\n\
         presented: {:.1} fps (new frames drawn), {:.1} UI passes/s\n\
         ui frame time (eframe cpu_usage): mean {mean:.3} ms, p50 {:.3}, p99 {:.3}, max {:.3} \
         over {} passes\n\
         upload (write_texture): mean {upload_mean:.3} ms, max {:.3} ms over {uploads} frames\n\
         {decoding}\n{board}\nrtt: {}\n{refusal}",
        on_off(vsync),
        d(s.frames_whole, b.stats.frames_whole) as f64 / dt,
        d(s.bytes, b.stats.bytes) as f64 * 8.0 / dt / 1e6,
        d(s.frames_incomplete, b.stats.frames_incomplete),
        d(s.frames_cut, b.stats.frames_cut),
        d(s.frames_replaced, b.stats.frames_replaced),
        d(s.stale_chunks, b.stats.stale_chunks),
        d(presented, b.presented) as f64 / dt,
        d(passes, b.passes) as f64 / dt,
        ms(percentile(cpu, 0.5)),
        ms(percentile(cpu, 0.99)),
        ms(percentile(cpu, 1.0)),
        cpu.len(),
        timing.max.as_secs_f64() * 1e3,
        s.rtt.map_or("-".to_owned(), |r| format!(
            "{:.3} ms",
            r.as_secs_f64() * 1e3
        )),
    )
}

fn save_png(path: &std::path::Path, image: &egui::ColorImage) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        image.size[0] as u32,
        image.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    w.write_image_data(image.as_raw())?;
    w.finish()?;
    Ok(())
}
