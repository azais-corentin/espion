//! `--selftest`: run a while, measure over the last two thirds, print the statistics, save a
//! screenshot and exit.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use eframe::egui;
use poc_viewer::{blobs::MarkerStats, decode::DecodeStats, net::Stats, timing::Snapshot};

use crate::{
    display::{Shown, UploadTiming},
    ui::{hz, on_off},
};

/// `--selftest`.
pub struct SelfTestConfig {
    pub secs: f64,
    pub screenshot: PathBuf,
    pub vsync: bool,
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

pub struct SelfTest {
    cfg: SelfTestConfig,
    start: Instant,
    warmup: Duration,
    base: Option<Base>,
    requested: Option<Instant>,
    saved: Arc<Mutex<Option<String>>>,
}

/// What a self-test step reads of the window.
pub struct Probe<'a> {
    pub stats: &'a Stats,
    pub decode: DecodeStats,
    pub presented: u64,
    pub passes: u64,
    pub upload: UploadTiming,
    /// UI frame times since the measurement started, seconds.
    pub cpu: &'a mut Vec<f32>,
    pub shown: Option<&'a Shown>,
    pub pipeline: Option<&'a Snapshot>,
    /// The single-marker statistics of the window's statistics window.
    pub markers: MarkerStats,
    /// That window, seconds.
    pub stats_s: f64,
}

impl SelfTest {
    pub fn new(cfg: SelfTestConfig, now: Instant) -> Self {
        Self {
            warmup: Duration::from_secs_f64((cfg.secs / 3.0).min(2.0)),
            cfg,
            start: now,
            base: None,
            requested: None,
            saved: Arc::default(),
        }
    }

    /// One pass: takes the base after the warm-up, then at the end prints the report, asks for
    /// the screenshot and closes the window once it is saved.
    pub fn step(&mut self, ctx: &egui::Context, now: Instant, p: Probe<'_>) {
        ctx.request_repaint_after(Duration::from_millis(50));
        let elapsed = now - self.start;
        if self.base.is_none() && elapsed >= self.warmup {
            self.base = Some(Base {
                at: now,
                stats: p.stats.clone(),
                decode: p.decode.clone(),
                presented: p.presented,
                passes: p.passes,
                timing: p.upload,
            });
            p.cpu.clear();
        }
        if let Some(asked) = self.requested {
            if let Some(msg) = self.saved.lock().unwrap_or_else(|e| e.into_inner()).take() {
                println!("{msg}");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if now - asked > Duration::from_secs(5) {
                println!("screenshot: none after 5 s");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if elapsed.as_secs_f64() < self.cfg.secs {
            return;
        }
        let Some(b) = &self.base else {
            return;
        };
        println!("{}", pipeline_report(p.pipeline));
        println!(
            "{}",
            detector_report(b, now, p.stats, &p.markers, p.stats_s)
        );
        println!(
            "{}",
            report(
                b,
                now,
                p.stats,
                &p.decode,
                p.presented,
                p.passes,
                &p.upload,
                p.cpu,
                self.cfg.vsync,
                p.shown
            )
        );
        let path = self.cfg.screenshot.clone();
        let saved = self.saved.clone();
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
        self.requested = Some(now);
    }
}

fn percentile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// The board's detector over the measurement and the marker statistics, as the self-test
/// prints them.
fn detector_report(b: &Base, now: Instant, s: &Stats, m: &MarkerStats, stats_s: f64) -> String {
    let dt = (now - b.at).as_secs_f64();
    let received = (s.blob_frames - b.stats.blob_frames) as f64 / dt;
    let board = match (b.stats.state, s.state) {
        (Some(a), Some(z)) => {
            let (a, z) = (a.detect, z.detect);
            format!(
                "detector: {} (threshold {}, area {}), {} frames detected, {} skipped (busy), \
                 detect mean {:.3} ms max {:.3} ms (last state), threshold in use {}, background \
                 median {} sigma {}, {} blobs dropped, {} filtered, fault {}",
                on_off(z.settings.enabled),
                if z.settings.auto_threshold {
                    "automatic"
                } else {
                    "manual"
                },
                if z.settings.auto_area {
                    "automatic"
                } else {
                    "manual"
                },
                z.frames_detected.wrapping_sub(a.frames_detected),
                z.frames_skipped.wrapping_sub(a.frames_skipped),
                f64::from(z.detect_us_mean) / 1e3,
                f64::from(z.detect_us_max) / 1e3,
                z.threshold,
                z.background_median,
                z.background_sigma,
                z.blobs_dropped.wrapping_sub(a.blobs_dropped),
                z.blobs_filtered.wrapping_sub(a.blobs_filtered),
                on_off(z.fault),
            )
        }
        _ => "detector: no state".to_owned(),
    };
    let marker = if m.single > 0 {
        format!(
            "x {:.3} y {:.3} px, sigma x {:.4} y {:.4} px, peak {:.0}, area {:.1} px, width {:.2} \
             px, flux {:.0}",
            m.mean_x,
            m.mean_y,
            m.sigma_x,
            m.sigma_y,
            m.mean_peak,
            m.mean_area,
            m.mean_width,
            m.mean_flux
        )
    } else {
        "no frame with one blob".to_owned()
    };
    format!(
        "{board}\nblob frames: {received:.1} per second received, {} blobs\nmarkers over the last \
         {stats_s:.1} s: {} frames, detection rate {:.1} %, {} with one blob: {marker}",
        s.blobs - b.stats.blobs,
        m.frames,
        m.detection_rate() * 100.0,
        m.single,
    )
}

/// The pipeline timing's latest snapshot as the self-test prints it.
fn pipeline_report(s: Option<&Snapshot>) -> String {
    let Some(s) = s else {
        return "pipeline: no snapshot".to_owned();
    };
    let ms = |us: f32| us / 1e3;
    let steps = s
        .steps
        .iter()
        .map(|p| {
            format!(
                "{} {:.2} ms at {:.2}{}",
                p.stage.label(),
                ms(p.len_us),
                ms(p.start_us),
                if p.measured {
                    format!(" (slowest {:.2})", ms(p.slowest_us))
                } else {
                    " (computed)".to_owned()
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let end = |us: Option<f32>, to: &str| {
        us.map_or("-".to_owned(), |us| {
            format!("{:.2} ms {to}", ms(us)).trim_end().to_owned()
        })
    };
    format!(
        "pipeline: from the start of the exposure, image {}, markers {} (drawn {}), period \
         {:.2} ms, {} board, {} presented, {} blob and {} drawn blob frames in the window\n\
         pipeline steps: {steps}",
        end(
            s.image_us,
            if s.image_complete {
                "to the GPU"
            } else {
                "(partial)"
            }
        ),
        end(s.markers_us, "to here"),
        end(s.markers_drawn_us, ""),
        ms(s.period_us),
        s.board_frames,
        s.host_frames,
        s.blob_frames,
        s.overlay_frames,
    )
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
