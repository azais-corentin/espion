//! OV9281 capture over MIPI CSI-2 with `esp32p4-csi`: SCCB mode set-up, receiver start, frame
//! rate and error counters per mode, cache-invalidation timing and coherence check, and an
//! optional still dump over the log UART (decoded on the host by `tools/still.py`).
//!
//! The async functions keep their state small (the crate denies `clippy::large_stack_frames`):
//! every log line is formatted in a separate `#[inline(never)]` function.

use embassy_time::{Duration, Instant as EInstant, Timer, with_timeout};
use esp_hal::i2c::master::{Error as I2cError, I2c};
use esp_hal::peripherals::VDMA_CH0;
use esp_println::println;
use esp32p4_csi::{Config, Csi, Frame, LdoChannel, LdoSetting, PhyStatus, enable_ldo};
use log::{error, info, warn};

use crate::ov9281::{self, Mode};
use crate::soak;

type Bus = I2c<'static, esp_hal::Async>;

/// Frame buffers the driver rotates.
const BUFFERS: usize = 3;
/// Every buffer is sized for the largest mode.
const BUFFER_LEN: usize = Csi::buffer_len(&Config::new(1280, 800, ov9281::LANE_MBPS));
/// How long to wait for a frame before printing the debugging checklist.
const FRAME_TIMEOUT: Duration = Duration::from_millis(500);
/// Fill value of the frame buffers before the first capture.
const SENTINEL: u8 = 0x5A;
/// ROM cache maps L1 data cache and L2 cache (`cache_map_t`).
const CACHE_MAP_L1_L2: u32 = (1 << 4) | (1 << 5);

unsafe extern "C" {
    fn Cache_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_WriteBack_Addr(map: u32, addr: u32, size: u32) -> i32;
}

/// What one run of the camera section does.
pub struct Plan {
    /// Indexes into [`ov9281::MODES`], in order.
    pub modes: &'static [usize],
    /// Sensor test pattern (0x5E00 = 0x80) instead of the scene.
    pub test_pattern: bool,
    /// Clock lane gated between packets (0x4800 = 0x60) instead of continuous.
    pub clock_gated: bool,
    /// Exposure in lines.
    pub exposure_lines: u16,
    /// Gain, 0x10 = 1x.
    pub gain: u8,
    /// Frame-rate measurement window per mode.
    pub measure: Duration,
    /// Time the cache invalidation and run the cache coherence check per mode.
    pub cache_check: bool,
    /// Dump one frame per mode over the log UART.
    pub still_dump: bool,
    /// Run the processing soak per mode after the checks above (`soak`). `None`: capture only.
    pub soak: Option<soak::Plan>,
}

/// Runs `plan`. `psram` must hold at least `BUFFERS * BUFFER_LEN` bytes (plus one 1280x800
/// frame for the soak's synthetic frames); its start is cache-line aligned (PSRAM maps at
/// 0x4800_0000).
pub async fn run(i2c: &mut Bus, vdma: VDMA_CH0<'static>, psram: &'static mut [u8], plan: &Plan) {
    if !power_up() {
        return;
    }
    // Settling time for VO3 (esp-hal's DSI driver waits 500 us).
    Timer::after_millis(1).await;
    let split = (BUFFERS * BUFFER_LEN).min(psram.len());
    let (buffers, rest) = psram.split_at_mut(split);
    let Some(mut csi) = make_receiver(vdma, buffers, &ov9281::MODES[plan.modes[0]]) else {
        return;
    };
    let mut soak = plan.soak.and_then(|p| make_soak(p, rest));
    for &index in plan.modes {
        capture_mode(i2c, &mut csi, &ov9281::MODES[index], plan, soak.as_mut()).await;
    }
    if let Some(s) = &soak {
        s.log_table();
    }
    note("done");
}

/// The soak's synthetic frames go right after the capture buffers (a multiple of 4096 B, so
/// the area starts on a cache line).
#[inline(never)]
fn make_soak(plan: soak::Plan, rest: &'static mut [u8]) -> Option<soak::Soak> {
    const _: () = assert!(BUFFER_LEN.is_multiple_of(128));
    let need = Csi::buffer_len(&Config::new(1280, 800, ov9281::LANE_MBPS));
    if rest.len() < need {
        fail("PSRAM too small for the soak's synthetic frame", &need);
        return None;
    }
    Some(soak::Soak::new(plan, &mut rest[..need]))
}

/// Turns on LDO VO3 at 2.5 V: `VDD_MIPI_DPHY` on this board.
#[inline(never)]
fn power_up() -> bool {
    match enable_ldo(LdoChannel::Vo3, 2500) {
        Ok(s) => {
            log_ldo(&s);
            true
        }
        Err(e) => {
            fail("LDO VO3", &e);
            false
        }
    }
}

/// Fills the frame buffers with [`SENTINEL`] and creates the receiver.
#[inline(never)]
fn make_receiver(
    vdma: VDMA_CH0<'static>,
    psram: &'static mut [u8],
    first: &Mode,
) -> Option<Csi<'static>> {
    if psram.len() < BUFFERS * BUFFER_LEN {
        fail(
            "PSRAM too small for the frame buffers",
            &(BUFFERS * BUFFER_LEN),
        );
        return None;
    }
    // Sentinel so a tail the DMA did not write shows up in the first frames.
    psram[..BUFFERS * BUFFER_LEN].fill(SENTINEL);
    // SAFETY: ROM cache call on memory this function owns.
    unsafe {
        Cache_WriteBack_Addr(
            CACHE_MAP_L1_L2,
            psram.as_ptr() as u32,
            (BUFFERS * BUFFER_LEN) as u32,
        )
    };
    let (b0, rest) = psram.split_at_mut(BUFFER_LEN);
    let (b1, rest) = rest.split_at_mut(BUFFER_LEN);
    let (b2, _) = rest.split_at_mut(BUFFER_LEN);
    match Csi::new(vdma, [b0, b1, b2], csi_config(first)) {
        Ok(c) => {
            log_ready(&c);
            Some(c)
        }
        Err(e) => {
            fail("Csi::new", &e);
            None
        }
    }
}

fn csi_config(mode: &Mode) -> Config {
    Config::new(mode.width, mode.height, mode.lane_mbps)
}

async fn capture_mode(
    i2c: &mut Bus,
    csi: &mut Csi<'static>,
    mode: &Mode,
    plan: &Plan,
    soak: Option<&mut soak::Soak>,
) {
    if start_mode(i2c, csi, mode, plan).await {
        measure(csi, mode, plan.measure).await;
        if plan.cache_check {
            cache_check(csi).await;
        }
        if plan.still_dump {
            still(csi, mode, plan).await;
        }
        if let Some(s) = soak {
            s.run_mode(csi, mode).await;
        }
    }
    stop(i2c, csi).await;
}

/// Sensor set-up, receiver start, stream-on and the first frame. `false` if no frame arrived.
async fn start_mode(i2c: &mut Bus, csi: &mut Csi<'static>, mode: &Mode, plan: &Plan) -> bool {
    log_mode(mode, plan);
    if let Err(e) = sensor_setup(i2c, mode, plan).await {
        fail("SCCB write", &e);
        return false;
    }
    log_sensor_regs(i2c).await;
    if let Err(e) = csi.set_config(csi_config(mode)).and_then(|()| csi.start()) {
        fail("receiver start", &e);
        return false;
    }
    Timer::after_millis(1).await;
    log_phy_before_stream(&csi.phy_status());
    if let Err(e) = sccb_write(i2c, ov9281::STREAM_ON).await {
        fail("stream on", &e);
    }
    first_frame(csi).await
}

async fn first_frame(csi: &mut Csi<'static>) -> bool {
    match with_timeout(FRAME_TIMEOUT, csi.wait_frame(None)).await {
        Ok(Ok(f)) => {
            log_first_frame(&f);
            true
        }
        Ok(Err(e)) => {
            fail("wait_frame", &e);
            false
        }
        Err(_) => {
            fail("no frame within (ms)", &FRAME_TIMEOUT.as_millis());
            checklist(csi).await;
            false
        }
    }
}

async fn still(csi: &mut Csi<'static>, mode: &Mode, plan: &Plan) {
    match with_timeout(FRAME_TIMEOUT, csi.wait_frame(None)).await {
        Ok(Ok(f)) => dump_still(mode, plan, &f),
        _ => note("no frame for the still"),
    }
}

async fn stop(i2c: &mut Bus, csi: &mut Csi<'static>) {
    if let Err(e) = sccb_write(i2c, ov9281::STREAM_OFF).await {
        fail("stream off", &e);
    }
    // Let the current frame finish so the lanes go back to LP-11.
    Timer::after_millis(20).await;
    csi.stop();
}

/// Frame counts and sizes over one measurement window.
struct Measurement {
    expected: usize,
    window_us: u64,
    delivered: u32,
    wrong_size: u32,
    first_wrong: usize,
    /// First and last delivered frame: arrival time (us since `t0`) and sequence number.
    first: (u64, u32),
    last: (u64, u32),
}

/// Counts frames for `window` and checks every frame's size.
async fn measure(csi: &mut Csi<'static>, mode: &Mode, window: Duration) {
    // Drop what completed while the previous log lines went out (esp-println holds a critical
    // section per line, so the swap interrupt can be late and a frame cut short).
    let _ = with_timeout(FRAME_TIMEOUT, csi.wait_frame(None)).await;
    csi.reset_stats();
    let t0 = EInstant::now();
    let mut m = Measurement {
        expected: mode.width as usize * mode.height as usize,
        window_us: 0,
        delivered: 0,
        wrong_size: 0,
        first_wrong: 0,
        first: (0, 0),
        last: (0, 0),
    };
    let mut frame: Option<Frame> = None;
    while t0.elapsed() < window {
        match with_timeout(FRAME_TIMEOUT, csi.wait_frame(frame.take())).await {
            Ok(Ok(f)) => {
                let at = (t0.elapsed().as_micros(), f.seq());
                if m.delivered == 0 {
                    m.first = at;
                }
                m.last = at;
                m.delivered += 1;
                if f.received_len() != m.expected {
                    if m.wrong_size == 0 {
                        m.first_wrong = f.received_len();
                    }
                    m.wrong_size += 1;
                }
                frame = Some(f);
            }
            Ok(Err(e)) => {
                fail("wait_frame", &e);
                break;
            }
            Err(_) => {
                note("frames stopped during the measurement");
                checklist(csi).await;
                break;
            }
        }
    }
    drop(frame);
    m.window_us = t0.elapsed().as_micros().max(1);
    log_measurement(csi, mode, &m);
    log_errors(csi);
}

/// The driver's "no frames arrive" checklist (`poc-csi.md`), in order, plus a second register
/// snapshot 5 ms later to see what moves.
async fn checklist(csi: &Csi<'static>) {
    log_checklist(csi);
    Timer::after_millis(5).await;
    log_snapshot(csi);
}

/// Times the ROM call `wait_frame` makes (`Cache_Invalidate_Addr` over the frame, L1 + L2) on
/// a delivered frame, once with the frame's lines not cached and once after reading every
/// line. Then checks it is needed and works: the first `PROBE` bytes of a buffer are read (so
/// they sit in the caches); after 40 ms of DMA without `wait_frame`, reading them through the
/// cache must show the old frame and reading them after an invalidation the new one. When the
/// buffer is next delivered, the bytes as `wait_frame` hands them out must equal what an
/// explicit invalidation shows.
async fn cache_check(csi: &mut Csi<'static>) {
    if let Some((addr, cached)) = cache_control(csi).await {
        coherence(csi, addr, cached).await;
    }
}

/// Size of the probe the coherence checks read.
const PROBE: usize = 16 * 1024;

/// Timing, then the control: returns the buffer address and the probe sum now in the caches.
async fn cache_control(csi: &mut Csi<'static>) -> Option<(u32, u32)> {
    let Ok(Ok(f)) = with_timeout(FRAME_TIMEOUT, csi.wait_frame(None)).await else {
        note("no frame for the cache check");
        return None;
    };
    let addr = f.as_ptr() as u32;
    time_invalidation(&f);
    let old = byte_sum(&f[..PROBE]);
    drop(f);
    // No `wait_frame` here (it would invalidate the buffer): let the DMA cycle through the
    // free buffers for a few frames.
    Timer::after_millis(40).await;
    Some((addr, probe_control(addr, old)))
}

/// Reads the probe through the caches and after an invalidation; returns the cached sum.
#[inline(never)]
fn probe_control(addr: u32, old: u32) -> u32 {
    // SAFETY: read-only peek at a driver-owned buffer for this test.
    let peek = unsafe { core::slice::from_raw_parts(addr as *const u8, PROBE) };
    let cached = byte_sum(peek);
    // SAFETY: ROM cache call on a driver-owned buffer the CPU only reads.
    unsafe { Cache_Invalidate_Addr(CACHE_MAP_L1_L2, addr, PROBE as u32) };
    let memory = byte_sum(peek);
    log_control(addr, old, cached, memory);
    cached
}

/// Waits until buffer `addr` is delivered again and compares what `wait_frame` handed out with
/// a fresh invalidation.
async fn coherence(csi: &mut Csi<'static>, addr: u32, cached: u32) {
    let mut frame = None;
    for _ in 0..8 {
        let Ok(Ok(g)) = with_timeout(FRAME_TIMEOUT, csi.wait_frame(frame.take())).await else {
            note("no frame for the coherence check");
            return;
        };
        if g.as_ptr() as u32 == addr {
            probe_delivered(&g, cached);
            return;
        }
        frame = Some(g);
    }
    fail("buffer did not come back within 8 frames", &addr);
}

#[inline(never)]
fn probe_delivered(g: &Frame, cached: u32) {
    let addr = g.as_ptr() as u32;
    let delivered = byte_sum(&g[..PROBE]);
    // SAFETY: ROM cache call on a delivered frame the CPU only reads.
    unsafe { Cache_Invalidate_Addr(CACHE_MAP_L1_L2, addr, PROBE as u32) };
    let fresh = byte_sum(&g[..PROBE]);
    log_coherence(addr, cached, delivered, fresh);
}

#[inline(never)]
fn time_invalidation(f: &Frame) {
    let (addr, len) = (f.as_ptr() as u32, f.len().div_ceil(64) as u32 * 64);
    let t = esp_hal::time::Instant::now();
    // SAFETY: ROM cache call on a delivered frame the CPU only reads.
    let rc_cold = unsafe { Cache_Invalidate_Addr(CACHE_MAP_L1_L2, addr, len) };
    let cold = t.elapsed().as_micros();
    let sum = line_sum(f);
    core::hint::black_box(sum);
    let t = esp_hal::time::Instant::now();
    // SAFETY: as above.
    let rc_warm = unsafe { Cache_Invalidate_Addr(CACHE_MAP_L1_L2, addr, len) };
    let warm = t.elapsed().as_micros();
    info!(
        "camera: cache invalidation of {len} B at {addr:#010x}: {cold} us uncached (rc {rc_cold}), {warm} us after reading the frame (rc {rc_warm})"
    );
}

fn line_sum(buf: &[u8]) -> u32 {
    let (lines, _) = buf.as_chunks::<64>();
    lines.iter().fold(0u32, |a, c| a.wrapping_add(c[0] as u32))
}

fn byte_sum(buf: &[u8]) -> u32 {
    buf.iter().fold(0u32, |a, &b| a.wrapping_add(b as u32))
}

// ── SCCB ────────────────────────────────────────────────────────────────────────────────

async fn sensor_setup(i2c: &mut Bus, mode: &Mode, plan: &Plan) -> Result<(), I2cError> {
    for &(reg, val) in mode.regs {
        if reg == ov9281::DELAY {
            Timer::after_millis(val as u64).await;
        } else {
            sccb_write(i2c, (reg, val)).await?;
        }
    }
    for r in ov9281::exposure_regs(plan.exposure_lines) {
        sccb_write(i2c, r).await?;
    }
    for r in ov9281::gain_regs(plan.gain) {
        sccb_write(i2c, r).await?;
    }
    if plan.test_pattern {
        sccb_write(i2c, ov9281::TEST_PATTERN_ON).await?;
    }
    if plan.clock_gated {
        sccb_write(i2c, ov9281::MIPI_CLOCK_GATED).await?;
    }
    Ok(())
}

async fn sccb_write(i2c: &mut Bus, (reg, val): (u16, u8)) -> Result<(), I2cError> {
    let [hi, lo] = reg.to_be_bytes();
    i2c.write_async(ov9281::SCCB_ADDR, &[hi, lo, val]).await
}

async fn sccb_read(i2c: &mut Bus, reg: u16) -> Option<u8> {
    let mut v = [0u8];
    i2c.write_read_async(ov9281::SCCB_ADDR, &reg.to_be_bytes(), &mut v)
        .await
        .ok()
        .map(|_| v[0])
}

/// Registers that decide the link and the frame timing, read back after the mode set-up.
const READBACK: [u16; 14] = [
    0x0100, 0x030D, 0x3662, 0x4800, 0x5E00, 0x3808, 0x3809, 0x380A, 0x380B, 0x380C, 0x380D, 0x380E,
    0x380F, 0x3509,
];

async fn log_sensor_regs(i2c: &mut Bus) {
    let mut vals = [0u8; READBACK.len()];
    for (v, &r) in vals.iter_mut().zip(READBACK.iter()) {
        *v = sccb_read(i2c, r).await.unwrap_or(0xEE);
    }
    log_readback(&vals);
}

// ── Log lines ───────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn fail(what: &str, detail: &dyn core::fmt::Debug) {
    error!("camera: {what}: {detail:?}");
}

#[inline(never)]
fn note(what: &str) {
    info!("camera: {what}");
}

#[inline(never)]
fn log_ldo(s: &LdoSetting) {
    info!(
        "camera: LDO VO3 2.5 V: dref {} mul {} calibrated {} expected {} mV, ctrl {:#010x} ana {:#010x}",
        s.dref, s.mul, s.calibrated, s.expected_mv, s.ctrl, s.ana
    );
}

#[inline(never)]
fn log_ready(csi: &Csi<'static>) {
    info!(
        "camera: CSI receiver ready, chip rev {}, {BUFFERS} buffers of {BUFFER_LEN} B in PSRAM",
        csi.chip_revision()
    );
}

#[inline(never)]
fn log_mode(mode: &Mode, plan: &Plan) {
    info!(
        "camera: mode {} ({}x{}), {} clock, {}, exposure {} lines, gain {:#04x}",
        mode.name,
        mode.width,
        mode.height,
        if plan.clock_gated {
            "gated"
        } else {
            "continuous"
        },
        if plan.test_pattern {
            "test pattern"
        } else {
            "scene"
        },
        plan.exposure_lines,
        plan.gain
    );
}

#[inline(never)]
fn log_readback(vals: &[u8; READBACK.len()]) {
    info!("camera: sensor regs {READBACK:04x?} = {vals:02x?}");
}

#[inline(never)]
fn log_phy_before_stream(phy: &PhyStatus) {
    info!("camera: before stream-on: {phy:?}");
    if !(phy.clock_stop && phy.data_stop[0] && phy.data_stop[1]) {
        warn!("camera: lanes not all in stop state before stream-on");
    }
}

#[inline(never)]
fn log_first_frame(f: &Frame) {
    let n = f.len();
    info!(
        "camera: first frame seq {}, received {} of {} B, head {:02x?} tail {:02x?} (sentinel {SENTINEL:#04x})",
        f.seq(),
        f.received_len(),
        n,
        &f[..16],
        &f[n - 16..]
    );
}

/// Millihertz as `x.yyy`.
struct Fps(u64);

impl core::fmt::Display for Fps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:03}", self.0 / 1000, self.0 % 1000)
    }
}

#[inline(never)]
fn log_measurement(csi: &Csi<'static>, mode: &Mode, m: &Measurement) {
    let s = csi.stats();
    let counted = Fps(s.frames_done as u64 * 1_000_000_000 / m.window_us);
    // Frame period from first to last arrival: no ±1 frame counting error.
    let span_us = (m.last.0 - m.first.0).max(1);
    let span = Fps((m.last.1 - m.first.1) as u64 * 1_000_000_000 / span_us);
    info!(
        "camera: {}: {} frames in {} ms = {counted} fps, first-to-last {span} fps (sensor {}), delivered {}, bytes/frame {} (expected {}, block items reported {}), wrong size {} (first {}), dropped {}, overwritten {}",
        mode.name,
        s.frames_done,
        m.window_us / 1000,
        Fps(mode.frame_rate_mhz() as u64),
        m.delivered,
        s.last_frame_bytes,
        m.expected,
        s.last_block_items,
        m.wrong_size,
        m.first_wrong,
        s.frames_dropped,
        s.frames_overwritten
    );
}

#[inline(never)]
fn log_errors(csi: &Csi<'static>) {
    let s = csi.stats();
    info!(
        "camera: errors: dma {} (bits {:#010x}), host {} (main {:#010x}, detail {:x?}), bridge {} (bits {:#x})",
        s.dma_errors,
        s.dma_error_bits,
        s.host_errors,
        s.host_error_main,
        s.host_error_detail,
        s.bridge_errors,
        s.bridge_error_bits
    );
}

#[inline(never)]
fn log_checklist(csi: &Csi<'static>) {
    let d = csi.debug_status();
    info!("check 1: chip rev {}", csi.chip_revision());
    info!(
        "check 2: hsfreqrange readback {:?} (want {:?})",
        d.hsfreqrange_readback,
        esp32p4_csi::hsfreqrange(csi.config().lane_bit_rate_mbps)
    );
    info!("check 3/4: {:?}", d.phy);
    log_errors(csi);
    info!(
        "check 6: isp cntl {:#010x} int_raw {:#010x}",
        d.isp_cntl, d.isp_int_raw
    );
    info!(
        "check 7: bridge enabled {} int_raw {:#010x} buf_depth {}",
        d.bridge_enabled, d.bridge_int_raw, d.bridge_buf_depth
    );
    log_snapshot(csi);
}

#[inline(never)]
fn log_snapshot(csi: &Csi<'static>) {
    let d = csi.debug_status();
    info!(
        "check 8: dma enabled {} int {:#010x} completed items {} fifo left {} dar {:#010x}, bridge buf_depth {}, phy {:?}",
        d.dma_enabled,
        d.dma_int_status,
        d.dma_completed_items,
        d.dma_fifo_left,
        d.dma_dar,
        d.bridge_buf_depth,
        d.phy
    );
}

#[inline(never)]
fn log_control(addr: u32, old: u32, cached: u32, memory: u32) {
    info!(
        "camera: cache control: buffer {addr:#010x} after 40 ms of DMA, no invalidation: sum {cached} (old frame {old}), after invalidation {memory} -> {}",
        if cached == old && memory != old {
            "stale without invalidation, as expected"
        } else {
            "inconclusive"
        }
    );
}

#[inline(never)]
fn log_coherence(addr: u32, old: u32, delivered: u32, fresh: u32) {
    info!(
        "camera: coherence: first 16 KiB of buffer {addr:#010x}: cached old sum {old}, delivered {delivered}, after re-invalidation {fresh} -> {}",
        if delivered == fresh { "OK" } else { "STALE" }
    );
}

// ── Still dump ──────────────────────────────────────────────────────────────────────────

/// Raw bytes per `@@D` line (base64: 128 characters).
const DUMP_CHUNK: usize = 96;

/// Prints `frame` as `@@IMG`, numbered base64 `@@D` lines and `@@END` with a CRC-32 (IEEE,
/// as zlib) over the pixels. `tools/still.py` turns it into a PGM and PNG.
#[inline(never)]
fn dump_still(mode: &Mode, plan: &Plan, frame: &Frame) {
    let kind = if plan.test_pattern {
        "pattern"
    } else {
        "scene"
    };
    let t = esp_hal::time::Instant::now();
    println!(
        "@@IMG {}x{}-{} {} {} {}",
        mode.width,
        mode.height,
        kind,
        mode.width,
        mode.height,
        frame.len()
    );
    let mut crc = !0u32;
    let mut line = [0u8; DUMP_CHUNK / 3 * 4];
    for (i, chunk) in frame.chunks(DUMP_CHUNK).enumerate() {
        crc = crc32_update(crc, chunk);
        let n = base64(chunk, &mut line);
        // SAFETY: base64 output is ASCII.
        let text = unsafe { core::str::from_utf8_unchecked(&line[..n]) };
        println!("@@D {i:x} {text}");
    }
    println!("@@END {:08x} {}", !crc, frame.len().div_ceil(DUMP_CHUNK));
    info!(
        "camera: still dumped in {} ms",
        t.elapsed().as_micros() / 1000
    );
}

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    crc
}

fn base64(data: &[u8], out: &mut [u8]) -> usize {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut n = 0;
    for c in data.chunks(3) {
        let v = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out[n] = T[(v >> 18) as usize & 63];
        out[n + 1] = T[(v >> 12) as usize & 63];
        out[n + 2] = if c.len() > 1 {
            T[(v >> 6) as usize & 63]
        } else {
            b'='
        };
        out[n + 3] = if c.len() > 2 {
            T[v as usize & 63]
        } else {
            b'='
        };
        n += 4;
    }
    n
}
