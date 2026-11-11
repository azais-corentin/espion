//! poc-viewer: live view and control of the OV9281 on the ESP32-P4-ETH proof of concept.
//!
//! One window: a status bar, the settings rail, the picture and the pipeline timing under it,
//! the counters in a drawer ([`app`], drawn with [`ui`]). Networking runs on its own thread
//! ([`poc_viewer::net`]); the image goes to the GPU as it came off the wire ([`display`]).

mod app;
mod display;
mod selftest;
mod ui;

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use eframe::{egui, egui_wgpu::wgpu};
use espion_proto::cam::{CameraSettings, Encoding, Format, Origin, Readout, TestPattern};
use poc_viewer::{
    addr,
    fake::{self, FakeCamera, FakeConfig},
    stream::StreamEdit,
};

/// Live view and control of the OV9281 camera stream (espion-proto, camera stream kinds).
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Camera address, link-local with its interface scope.
    #[arg(default_value = "[fe80::4553:9281%eth0]:5009")]
    camera: String,

    /// Run against the built-in fake camera on loopback instead of the board.
    #[arg(long)]
    fake: bool,
    /// Fake camera: line-rate limit in Mbit/s (Ethernet bytes on the wire).
    #[arg(long, requires = "fake", value_name = "MBPS")]
    fake_mbps: Option<f64>,
    /// Fake camera: fraction of datagrams lost (0..1).
    #[arg(
        long,
        requires = "fake",
        value_name = "FRACTION",
        default_value_t = 0.0
    )]
    fake_loss: f64,

    /// Present without vsync (as fast as frames come).
    #[arg(long)]
    no_vsync: bool,
    /// Start at 1:1 (one image pixel per screen pixel) instead of fit-to-window.
    #[arg(long)]
    one_to_one: bool,
    /// Display gain at start (brightness multiplier on screen; the data is unchanged).
    #[arg(long, default_value_t = 1.0)]
    display_gain: f32,

    /// Self-test: run this many seconds, save a screenshot of the window, print the
    /// statistics and exit.
    #[arg(long, value_name = "SECONDS")]
    selftest: Option<f64>,
    /// Where the self-test saves its screenshot (PNG).
    #[arg(long, default_value = "/tmp/poc-p4-eth/viewer/selftest.png")]
    screenshot: PathBuf,

    #[command(flatten)]
    init: InitArgs,
}

/// Settings sent once at start (a camera option sends the camera settings, a stream option the
/// stream settings); unset values are the fake camera's defaults. Without any, the viewer
/// adopts the board's settings.
#[derive(Debug, clap::Args)]
struct InitArgs {
    /// Sensor format.
    #[arg(long)]
    format: Option<FormatArg>,
    /// Output size, WIDTHxHEIGHT.
    #[arg(long, value_parser = parse_size)]
    size: Option<(u16, u16)>,
    /// 2x2 binned readout.
    #[arg(long)]
    binned: bool,
    /// Window origin in sensor pixels, X,Y (default: centred).
    #[arg(long, value_parser = parse_origin)]
    origin: Option<Origin>,
    /// Frame rate, Hz.
    #[arg(long)]
    rate: Option<f64>,
    /// Frame-rate tolerance, Hz.
    #[arg(long)]
    tolerance: Option<f64>,
    /// Exposure, lines.
    #[arg(long)]
    exposure: Option<u16>,
    /// Analog gain, sixteenths (16 = 1x).
    #[arg(long)]
    gain: Option<u8>,
    /// Test pattern.
    #[arg(long)]
    pattern: Option<PatternArg>,
    /// Pixel encoding on the wire.
    #[arg(long)]
    encoding: Option<EncodingArg>,
    /// Most frames sent per second (0: no limit).
    #[arg(long)]
    max_fps: Option<u16>,
    /// JPEG quality, 1..=100, with automatic quality off. With it on, the board chooses the
    /// quality, up to 90, and this one applies when automatic quality is turned off.
    #[arg(long)]
    quality: Option<u8>,
    /// Automatic JPEG quality: the board chooses the quality, up to 90 (sent as the ceiling,
    /// whatever `--quality`).
    #[arg(long)]
    auto_quality: Option<bool>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FormatArg {
    Raw8,
    Raw10,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EncodingArg {
    Raw8,
    Raw10,
    Jpeg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PatternArg {
    Off,
    Bars,
    RollingBars,
    GreySquares,
    BwSquares,
    Random,
}

fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    Ok((
        w.parse().map_err(|_| "bad width")?,
        h.parse().map_err(|_| "bad height")?,
    ))
}

fn parse_origin(s: &str) -> Result<Origin, String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
    Ok(Origin {
        x: x.parse().map_err(|_| "bad x")?,
        y: y.parse().map_err(|_| "bad y")?,
    })
}

fn millihertz(hz: f64) -> u32 {
    (hz * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32
}

impl InitArgs {
    fn camera(&self) -> Option<CameraSettings> {
        let given = self.format.is_some()
            || self.size.is_some()
            || self.binned
            || self.origin.is_some()
            || self.rate.is_some()
            || self.tolerance.is_some()
            || self.exposure.is_some()
            || self.gain.is_some()
            || self.pattern.is_some();
        if !given {
            return None;
        }
        let d = fake::default_camera();
        let (width, height) = self.size.unwrap_or((d.width, d.height));
        Some(CameraSettings {
            format: match self.format {
                Some(FormatArg::Raw8) => Format::Raw8,
                Some(FormatArg::Raw10) => Format::Raw10,
                None => d.format,
            },
            width,
            height,
            readout: if self.binned {
                Readout::Binned
            } else {
                Readout::Crop
            },
            origin: self.origin,
            rate_mhz: self.rate.map_or(d.rate_mhz, millihertz),
            tolerance_mhz: self.tolerance.map_or(d.tolerance_mhz, millihertz),
            exposure_lines: self.exposure.unwrap_or(d.exposure_lines),
            gain_sixteenths: self.gain.unwrap_or(d.gain_sixteenths),
            test_pattern: match self.pattern {
                None => d.test_pattern,
                Some(PatternArg::Off) => TestPattern::Off,
                Some(PatternArg::Bars) => TestPattern::Bars,
                Some(PatternArg::RollingBars) => TestPattern::RollingBars,
                Some(PatternArg::GreySquares) => TestPattern::GreySquares,
                Some(PatternArg::BwSquares) => TestPattern::BlackWhiteSquares,
                Some(PatternArg::Random) => TestPattern::Random,
            },
        })
    }

    /// The stream controls at start; `--quality` is the manual quality.
    fn stream(&self) -> Option<StreamEdit> {
        if self.encoding.is_none()
            && self.max_fps.is_none()
            && self.quality.is_none()
            && self.auto_quality.is_none()
        {
            return None;
        }
        let d = StreamEdit::new(fake::default_stream());
        Some(StreamEdit {
            enabled: true,
            encoding: match self.encoding {
                Some(EncodingArg::Raw8) => Encoding::Raw8,
                Some(EncodingArg::Raw10) => Encoding::Raw10,
                Some(EncodingArg::Jpeg) => Encoding::Jpeg,
                None => d.encoding,
            },
            max_fps: self.max_fps.unwrap_or(d.max_fps),
            manual_quality: self.quality.unwrap_or(d.manual_quality),
            auto_quality: self.auto_quality.unwrap_or(d.auto_quality),
        })
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (camera, fake): (SocketAddr, Option<FakeCamera>) = if args.fake {
        let mut cfg = FakeConfig {
            mbps: args.fake_mbps,
            loss: args.fake_loss,
            ..FakeConfig::default()
        };
        // Start the fake in the requested settings when they go together (the viewer sends
        // them right after anyway), so a RAW10 stream request finds a RAW10 sensor. A mode JPEG
        // cannot be made of, without a stream given, starts the fake in RAW8.
        let stream = args.init.stream().map(|e| e.settings());
        if let Some(s) = stream {
            cfg.stream = s;
        }
        if let Some(c) = args.init.camera() {
            if stream.is_none()
                && cfg
                    .stream
                    .encoding
                    .check(c.format, c.width, c.height)
                    .is_err()
            {
                cfg.stream.encoding = Encoding::Raw8;
            }
            if poc_viewer::camera::validate(&c, cfg.stream.encoding).is_ok()
                && cfg.stream.check(c.format, c.width, c.height).is_ok()
            {
                cfg.camera = c;
            }
        }
        if cfg
            .stream
            .check(cfg.camera.format, cfg.camera.width, cfg.camera.height)
            .is_err()
        {
            cfg.stream = fake::default_stream();
        }
        let f = FakeCamera::spawn(cfg).context("starting the fake camera")?;
        println!("fake camera on {}", f.addr());
        (f.addr(), Some(f))
    } else {
        (
            addr::parse(&args.camera).with_context(|| format!("camera address {}", args.camera))?,
            None,
        )
    };

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("poc-viewer")
            .with_inner_size([1900.0, 1150.0])
            .with_maximized(true),
        ..Default::default()
    };
    options.wgpu_options.surface.present_mode = if args.no_vsync {
        wgpu::PresentMode::AutoNoVsync
    } else {
        wgpu::PresentMode::AutoVsync
    };

    let setup = app::Setup {
        camera,
        fake,
        init_camera: args.init.camera(),
        init_stream: args.init.stream(),
        one_to_one: args.one_to_one,
        display_gain: args.display_gain,
        selftest: args.selftest.map(|secs| selftest::SelfTestConfig {
            secs,
            screenshot: args.screenshot.clone(),
            vsync: !args.no_vsync,
        }),
    };
    eframe::run_native(
        "poc-viewer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, setup)?))),
    )
    .map_err(|e| anyhow!("{e}"))
}
