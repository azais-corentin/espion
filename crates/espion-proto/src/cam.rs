//! Camera stream: live frames from one board's OV9281 and remote control of its settings.
//!
//! Used by the `poc-p4-stream` firmware and the `poc-viewer` host app over IPv6 link-local
//! (`docs/protocol.md`, "Camera stream"). Three kinds:
//!
//! - [`Request`] (`0x20`, host to board): read the state, set the camera or the stream
//!   settings, or poll. Each carries a sequence number that the [`Response`] echoes. Every
//!   request is idempotent, so the host repeats it until it is answered.
//! - [`Response`] (`0x21`, board to host): the complete [`State`] or a [`CamError`].
//! - [`FrameChunk`] (`0x22`, board to host): [`CHUNK_MAX`] bytes of one frame, self-describing.
//!
//! The board answers and streams only to the address and port the last valid request came
//! from, and stops sending frames when no valid request arrived for [`STREAM_TIMEOUT_MS`].
//!
//! The types mirror the `ov9281` driver's (format, readout, test pattern, its errors) without
//! depending on it: each side converts.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, HEADER_LEN, decode_body, encode, kind, read_header};

/// UDP port of the camera stream on the board.
pub const CAM_PORT: u16 = 5009;

/// Largest UDP payload: a 1500-byte IPv6 packet minus the IPv6 (40) and UDP (8) headers, so
/// IPv6 never fragments.
pub const MAX_DATAGRAM: usize = 1500 - 40 - 8;

/// The board stops sending frames when no valid request arrived for this long (ms). The host
/// polls well within it.
pub const STREAM_TIMEOUT_MS: u32 = 3000;

/// Bytes of a frame chunk datagram besides its data, at the largest value of every field:
/// header, then the postcard varints of frame (5), generation (5), encoding (1), width (3),
/// height (3), timestamp (10), frame length (5), offset (5) and the data length (2).
pub const CHUNK_OVERHEAD_MAX: usize = HEADER_LEN + 5 + 5 + 1 + 3 + 3 + 10 + 5 + 5 + 2;

/// Data bytes per frame chunk: every chunk but a frame's last carries exactly this many, so a
/// chunk datagram is at most [`MAX_DATAGRAM`] bytes.
pub const CHUNK_MAX: usize = MAX_DATAGRAM - CHUNK_OVERHEAD_MAX;

/// Largest [`Encoding::Jpeg`] frame a receiver accepts, bytes (2 MiB). It holds the board
/// encoder's worst case for 1280x800: a 384-byte header plus 2 bytes per pixel.
pub const JPEG_FRAME_MAX: u32 = 2 * 1024 * 1024;

/// [`Encoding::Jpeg`] needs a width and a height that are multiples of this (the 8x8 blocks
/// of a greyscale JPEG).
pub const JPEG_ALIGN: u16 = 8;

/// Lowest JPEG quality ([`StreamSettings::jpeg_quality`]).
pub const JPEG_QUALITY_MIN: u8 = 1;

/// Highest JPEG quality ([`StreamSettings::jpeg_quality`]).
pub const JPEG_QUALITY_MAX: u8 = 100;

/// Period of the automatic JPEG quality control, ms ([`auto_quality`]).
pub const AUTO_QUALITY_PERIOD_MS: u32 = 200;

/// Lowest quality the automatic control lowers to (or the set quality, if lower).
pub const AUTO_QUALITY_MIN: u8 = 10;

/// The automatic control raises the quality only while the link, busy sending frames for this
/// share of the period (percent) scaled by the step's growth, has room ([`auto_quality`]).
pub const AUTO_QUALITY_ROOM_PCT: u8 = 80;

/// The IJG quality scaling of the base quantisation tables, percent: `5000 / q` below 50,
/// `200 - 2q` from 50 (at least 1). A frame's size grows roughly as the scaling shrinks, so
/// one quality step near 100 grows the file far more than one near 50.
pub const fn ijg_scale(q: u8) -> u32 {
    if q == 0 {
        5000
    } else if q < 50 {
        5000 / q as u32
    } else if q >= 100 {
        1
    } else {
        200 - 2 * q as u32
    }
}

/// One step of the automatic JPEG quality control, taken at the end of every
/// [`AUTO_QUALITY_PERIOD_MS`] while the stream sends [`Encoding::Jpeg`] with
/// [`StreamSettings::auto_quality`] on: the quality in use for the next period.
///
/// - `skipped`: at least one frame was skipped in the period because the link was still busy
///   with the previous one ([`Counters::frames_skipped`] grew).
/// - `busy_pct`: share of the period the link was busy sending frames (from the first to the
///   last chunk of each frame), percent.
///
/// Above `set` (the ceiling) it returns `set`. When frames were skipped the quality drops by
/// an eighth (at least 2) down to [`AUTO_QUALITY_MIN`], never below it and never up.
/// Otherwise it rises by 1 towards `set` when the link has room for the step:
/// `busy_pct * ijg_scale(q) < AUTO_QUALITY_ROOM_PCT * ijg_scale(q + 1)` (below 80 % busy,
/// less the step's expected growth: 72 % at 90, 53 % at 97, 40 % at 98 and 99), and holds
/// otherwise, so it settles below the link's limit instead of a sawtooth against it.
pub const fn auto_quality(in_use: u8, set: u8, skipped: bool, busy_pct: u8) -> u8 {
    let floor = if set < AUTO_QUALITY_MIN {
        set
    } else {
        AUTO_QUALITY_MIN
    };
    if in_use > set {
        set
    } else if skipped {
        if in_use <= floor {
            return in_use;
        }
        let step = if in_use / 8 > 2 { in_use / 8 } else { 2 };
        let q = in_use.saturating_sub(step);
        if q < floor { floor } else { q }
    } else if in_use < set
        && (busy_pct as u32) * ijg_scale(in_use)
            < AUTO_QUALITY_ROOM_PCT as u32 * ijg_scale(in_use + 1)
    {
        in_use + 1
    } else {
        in_use
    }
}

/// Sensor pixel format (mirrors `ov9281::Format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    /// 8 bits per pixel.
    Raw8,
    /// 10 bits per pixel.
    Raw10,
}

/// Readout of the window (mirrors `ov9281::Readout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Readout {
    /// One output pixel per sensor pixel, up to 1280x800.
    Crop,
    /// 2x2 binning, up to 640x400.
    Binned,
}

/// Sensor test pattern (mirrors `ov9281::TestPattern`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TestPattern {
    /// The image.
    Off,
    /// 8 vertical bars.
    Bars,
    /// Rolling bars.
    RollingBars,
    /// Grey squares.
    GreySquares,
    /// Black-and-white squares.
    BlackWhiteSquares,
    /// Random data, the same every frame.
    Random,
}

/// Pixel encoding of the frames on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Encoding {
    /// One byte per pixel, row after row. From a RAW10 sensor: the 8 high bits.
    Raw8,
    /// CSI-2 packed RAW10: every 4 pixels P0..P3 take 5 bytes, `P0[9:2] P1[9:2] P2[9:2]
    /// P3[9:2]`, then `P3[1:0] << 6 | P2[1:0] << 4 | P1[1:0] << 2 | P0[1:0]`. Needs a RAW10
    /// sensor format.
    Raw10,
    /// One complete baseline greyscale JPEG file per frame (JFIF header included, as the
    /// board's encoder writes it) of the 8-bit picture, `width` x `height`. Its length varies
    /// from frame to frame, at most [`JPEG_FRAME_MAX`]. Needs a RAW8 sensor format and a
    /// width and a height that are multiples of [`JPEG_ALIGN`].
    Jpeg,
}

impl Encoding {
    /// Bytes of a `width`-pixel line of a raw encoding (RAW10 widths are multiples of 4);
    /// `None` for [`Encoding::Jpeg`], which has no fixed line length.
    pub const fn line_bytes(self, width: u16) -> Option<u32> {
        match self {
            Self::Raw8 => Some(width as u32),
            Self::Raw10 => Some(width as u32 * 5 / 4),
            Self::Jpeg => None,
        }
    }

    /// Bytes of a frame of a raw encoding (`u64`: a RAW10 frame of the largest `u16` sizes
    /// overflows `u32`); `None` for [`Encoding::Jpeg`], whose length varies.
    pub const fn frame_bytes(self, width: u16, height: u16) -> Option<u64> {
        match self.line_bytes(width) {
            Some(line) => Some(line as u64 * height as u64),
            None => None,
        }
    }

    /// `true` when `frame_len` is a possible length of a frame of this encoding and size:
    /// exactly [`Encoding::frame_bytes`] for a raw encoding; for [`Encoding::Jpeg`],
    /// 1..=[`JPEG_FRAME_MAX`] with both sides non-zero multiples of [`JPEG_ALIGN`].
    pub const fn frame_len_ok(self, width: u16, height: u16, frame_len: u32) -> bool {
        match self.frame_bytes(width, height) {
            Some(bytes) => frame_len as u64 == bytes,
            None => {
                width != 0
                    && height != 0
                    && width.is_multiple_of(JPEG_ALIGN)
                    && height.is_multiple_of(JPEG_ALIGN)
                    && frame_len >= 1
                    && frame_len <= JPEG_FRAME_MAX
            }
        }
    }

    /// Whether this encoding can be made of a sensor mode of `format`, `width` x `height`:
    /// RAW10 needs a RAW10 sensor ([`CamError::Encoding`]); JPEG needs a RAW8 sensor and both
    /// sides multiples of [`JPEG_ALIGN`] ([`CamError::Jpeg`], the format checked first).
    pub const fn check(self, format: Format, width: u16, height: u16) -> Result<(), CamError> {
        match (self, format) {
            (Self::Raw8, _) | (Self::Raw10, Format::Raw10) => Ok(()),
            (Self::Raw10, Format::Raw8) => Err(CamError::Encoding {
                format,
                encoding: self,
            }),
            (Self::Jpeg, Format::Raw10) => Err(CamError::Jpeg {
                reason: JpegRefusal::Format,
                format,
                width,
                height,
            }),
            (Self::Jpeg, Format::Raw8) => {
                if width.is_multiple_of(JPEG_ALIGN) && height.is_multiple_of(JPEG_ALIGN) {
                    Ok(())
                } else {
                    Err(CamError::Jpeg {
                        reason: JpegRefusal::Size,
                        format,
                        width,
                        height,
                    })
                }
            }
        }
    }
}

/// Why [`Encoding::Jpeg`] cannot be made of a sensor mode ([`CamError::Jpeg`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JpegRefusal {
    /// The sensor format is RAW10; the encoder takes 8-bit pixels.
    Format,
    /// The width or the height is not a multiple of [`JPEG_ALIGN`].
    Size,
}

/// Top-left corner of the window, in sensor pixels of the 1280x800 active area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Origin {
    /// Column, a multiple of 8.
    pub x: u16,
    /// Row (binned: even).
    pub y: u16,
}

/// The complete camera settings: the sensor mode request and the controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CameraSettings {
    /// Sensor pixel format.
    pub format: Format,
    /// Output width in pixels (binned: binned pixels).
    pub width: u16,
    /// Output height in lines.
    pub height: u16,
    /// Crop or 2x2 binning.
    pub readout: Readout,
    /// Window origin; `None`: centred. In a [`State`] always the origin applied.
    pub origin: Option<Origin>,
    /// Requested frame rate, millihertz.
    pub rate_mhz: u32,
    /// Largest acceptable difference from the requested rate, millihertz.
    pub tolerance_mhz: u32,
    /// Exposure in lines, 1..=[`ModeInfo::max_exposure_lines`].
    pub exposure_lines: u16,
    /// Analog gain in sixteenths: 16 = 1x, up to 248 = 15.5x.
    pub gain_sixteenths: u8,
    /// Test pattern instead of the image.
    pub test_pattern: TestPattern,
}

impl CameraSettings {
    /// `true` when `other` asks for the same sensor mode (everything but exposure, gain and
    /// test pattern), so applying it changes only the controls.
    pub fn same_mode(&self, other: &Self) -> bool {
        (
            self.format,
            self.width,
            self.height,
            self.readout,
            self.origin,
            self.rate_mhz,
            self.tolerance_mhz,
        ) == (
            other.format,
            other.width,
            other.height,
            other.readout,
            other.origin,
            other.rate_mhz,
            other.tolerance_mhz,
        )
    }
}

/// What the board streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamSettings {
    /// Send frames (while a host keeps polling).
    pub enabled: bool,
    /// Pixel encoding on the wire.
    pub encoding: Encoding,
    /// Most frames sent per second; 0: no limit (every frame the link can take).
    pub max_fps: u16,
    /// JPEG quality, [`JPEG_QUALITY_MIN`]..=[`JPEG_QUALITY_MAX`] (IJG scale); with
    /// `auto_quality` the ceiling. Checked and kept whatever the encoding.
    pub jpeg_quality: u8,
    /// Automatic JPEG quality ([`auto_quality`]): lower the quality while frames are skipped
    /// because the link is busy, raise it back towards `jpeg_quality` when there is room.
    pub auto_quality: bool,
}

impl StreamSettings {
    /// The checks of a `SetStream` against the running sensor mode, in the board's order: the
    /// encoding ([`Encoding::check`]), then the JPEG quality ([`CamError::Quality`]).
    pub const fn check(&self, format: Format, width: u16, height: u16) -> Result<(), CamError> {
        if let Err(e) = self.encoding.check(format, width, height) {
            return Err(e);
        }
        if self.jpeg_quality < JPEG_QUALITY_MIN || self.jpeg_quality > JPEG_QUALITY_MAX {
            return Err(CamError::Quality {
                min: JPEG_QUALITY_MIN,
                max: JPEG_QUALITY_MAX,
            });
        }
        Ok(())
    }
}

/// The operation of a [`Request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Op {
    /// Read the state.
    GetState,
    /// Apply the camera settings, all or nothing. A new mode restarts the stream; the same
    /// mode ([`CameraSettings::same_mode`]) only changes the controls.
    SetCamera(CameraSettings),
    /// Apply the stream settings.
    SetStream(StreamSettings),
    /// Keep the stream alive and read the state (with its counters): the host's periodic
    /// request.
    Poll,
}

/// Host to board (`0x20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Request {
    /// Sequence number, echoed by the [`Response`]. A repeat keeps its number.
    pub seq: u32,
    /// What to do.
    pub op: Op,
}

/// The sensor mode the driver chose for the applied [`CameraSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModeInfo {
    /// Sensor clock, MHz (80 or 96).
    pub sensor_clock_mhz: u8,
    /// The clock is above the datasheet's 80 MHz.
    pub exceeds_datasheet_clock: bool,
    /// Line length, sensor clock cycles.
    pub hts: u16,
    /// Frame length, lines.
    pub vts: u16,
    /// Power-saving mode on.
    pub psv: bool,
    /// Frame rate at the nominal clock, millihertz.
    pub frame_rate_mhz: u32,
    /// Frame rate minus the requested rate, millihertz.
    pub rate_error_mhz: i32,
    /// Longest exposure of the mode, lines.
    pub max_exposure_lines: u16,
    /// Line period, ns.
    pub line_ns: u32,
    /// Bytes of a frame from the sensor.
    pub frame_bytes: u32,
}

/// Board counters since boot. Each wraps at `u32::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Counters {
    /// Whole frames received from the sensor.
    pub frames_captured: u32,
    /// Frames sent completely.
    pub frames_sent: u32,
    /// Captured frames not sent because the previous one was still being sent.
    pub frames_skipped: u32,
    /// Capture errors (short frames, receiver or DMA errors).
    pub capture_errors: u32,
    /// Datagrams sent, all kinds.
    pub datagrams_sent: u32,
}

/// The board's JPEG encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct EncoderState {
    /// Quality the next JPEG frame is encoded at: [`StreamSettings::jpeg_quality`], or lower
    /// while the automatic quality holds it down.
    pub quality: u8,
    /// Bytes of the last JPEG frame encoded (the file, header included); 0 before the first.
    pub frame_len: u32,
    /// Time the encoder took for that frame, µs.
    pub encode_us: u32,
    /// Encoder errors since boot (output too small, codec or DMA error, timeout): the frame
    /// is lost. Wraps at `u32::MAX`.
    pub errors: u32,
    /// Captured frames the encoder could not take since boot because it was still busy (not
    /// counted in [`Counters::frames_skipped`], which is the link). Wraps at `u32::MAX`.
    pub frames_skipped: u32,
}

/// The board's complete state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct State {
    /// Camera settings as applied (origin resolved).
    pub camera: CameraSettings,
    /// Stream settings as applied.
    pub stream: StreamSettings,
    /// What the sensor driver chose.
    pub mode: ModeInfo,
    /// Changes whenever the frame layout changes: format, size, readout, origin or encoding.
    pub generation: u32,
    /// Counters.
    pub counters: Counters,
    /// The JPEG encoder.
    pub encoder: EncoderState,
    /// Time since boot, µs (the clock of [`FrameChunk::timestamp_us`]).
    pub uptime_us: u64,
}

/// Why the board refused a request. The first six mirror `ov9281::Error`, rates in
/// millihertz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CamError {
    /// Width or height outside the readout's range (its largest given).
    Size {
        /// Largest width.
        max_width: u16,
        /// Largest height.
        max_height: u16,
    },
    /// Width or height not a multiple of the format's.
    Alignment {
        /// Width multiple: 8 RAW8, 64 RAW10.
        width_multiple: u8,
        /// Height multiple: 1 RAW8, 2 RAW10.
        height_multiple: u8,
    },
    /// Window outside the pixel array, or origin off the grid.
    Window,
    /// Rate above the window's fastest by more than the tolerance.
    TooFast {
        /// Fastest rate, millihertz.
        max_mhz: u32,
    },
    /// Rate below the slowest by more than the tolerance.
    TooSlow {
        /// Slowest rate, millihertz.
        min_mhz: u32,
    },
    /// No setting of the selected clock within the tolerance.
    Tolerance {
        /// Nearest rate, millihertz.
        nearest_mhz: u32,
    },
    /// The CSI-2 receiver refused the frame geometry.
    Receiver,
    /// The sensor did not answer on its I2C (SCCB) bus.
    SensorBus,
    /// Exposure outside 1..=`max_lines` for the mode.
    Exposure {
        /// Longest exposure of the mode, lines.
        max_lines: u16,
    },
    /// Gain outside `min..=max` sixteenths.
    Gain {
        /// Smallest gain, sixteenths.
        min: u8,
        /// Largest gain, sixteenths.
        max: u8,
    },
    /// The encoding cannot be produced from the sensor format.
    Encoding {
        /// Sensor format.
        format: Format,
        /// Encoding asked for.
        encoding: Encoding,
    },
    /// [`Encoding::Jpeg`] cannot be made of the sensor mode ([`Encoding::check`]).
    Jpeg {
        /// Why.
        reason: JpegRefusal,
        /// Sensor format of the mode.
        format: Format,
        /// Width of the mode.
        width: u16,
        /// Height of the mode.
        height: u16,
    },
    /// JPEG quality outside `min..=max`.
    Quality {
        /// Lowest quality.
        min: u8,
        /// Highest quality.
        max: u8,
    },
}

/// A rate in millihertz shown in hertz with three decimals.
struct Hz(u32);

impl fmt::Display for Hz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03} Hz", self.0 / 1000, self.0 % 1000)
    }
}

impl fmt::Display for CamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Size {
                max_width,
                max_height,
            } => write!(
                f,
                "size not supported: at most {max_width}x{max_height}, at least 128x2"
            ),
            Self::Alignment {
                width_multiple,
                height_multiple,
            } => write!(
                f,
                "width must be a multiple of {width_multiple} and height of {height_multiple}"
            ),
            Self::Window => f.write_str(
                "window outside the 1280x800 array, or origin off its grid (x a multiple of 8, \
                 binned y even)",
            ),
            Self::TooFast { max_mhz } => {
                write!(
                    f,
                    "frame rate above the fastest for this window, {}",
                    Hz(max_mhz)
                )
            }
            Self::TooSlow { min_mhz } => {
                write!(f, "frame rate below the slowest, {}", Hz(min_mhz))
            }
            Self::Tolerance { nearest_mhz } => write!(
                f,
                "no rate within the tolerance; the nearest is {}",
                Hz(nearest_mhz)
            ),
            Self::Receiver => f.write_str("the CSI-2 receiver refused the frame geometry"),
            Self::SensorBus => f.write_str("the sensor did not answer on its I2C bus"),
            Self::Exposure { max_lines } => {
                write!(f, "exposure outside 1..={max_lines} lines for this mode")
            }
            Self::Gain { min, max } => write!(
                f,
                "gain outside {min}..={max} sixteenths ({}x..={}x)",
                min as f32 / 16.0,
                max as f32 / 16.0
            ),
            Self::Encoding { format, encoding } => {
                write!(
                    f,
                    "encoding {encoding:?} cannot be sent from a {format:?} sensor"
                )
            }
            Self::Jpeg {
                reason: JpegRefusal::Format,
                format,
                ..
            } => write!(
                f,
                "JPEG needs a RAW8 sensor format; the mode is {format:?} (the encoder takes \
                 8-bit pixels)"
            ),
            Self::Jpeg {
                reason: JpegRefusal::Size,
                width,
                height,
                ..
            } => write!(
                f,
                "JPEG needs a width and a height that are multiples of {JPEG_ALIGN}; the mode \
                 is {width}x{height}"
            ),
            Self::Quality { min, max } => {
                write!(f, "JPEG quality outside {min}..={max}")
            }
        }
    }
}

impl core::error::Error for CamError {}

/// The answer of a [`Response`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Reply {
    /// The request was applied (or only read); the state after it.
    State(State),
    /// The request was refused; nothing changed.
    Error(CamError),
}

/// Board to host (`0x21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Response {
    /// The [`Request::seq`] answered.
    pub seq: u32,
    /// The answer.
    pub reply: Reply,
}

/// Board to host (`0x22`): one piece of one frame.
///
/// A frame of `frame_len` bytes is cut into chunks of [`CHUNK_MAX`] bytes, the last one
/// shorter; chunk `i` has offset `i * CHUNK_MAX`. [`Message::decode`] rejects a chunk off this
/// grid or whose frame length is not possible for the encoding, width and height
/// ([`Encoding::frame_len_ok`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameChunk<'a> {
    /// Frame number, counted by the board from boot over captured frames (sent or not).
    pub frame: u32,
    /// [`State::generation`] of the frame.
    pub generation: u32,
    /// Pixel encoding.
    pub encoding: Encoding,
    /// Width in pixels.
    pub width: u16,
    /// Height in lines.
    pub height: u16,
    /// Board time of the frame's capture (end of frame), µs since boot.
    pub timestamp_us: u64,
    /// Bytes of the whole frame: `encoding.frame_bytes(width, height)` for a raw encoding;
    /// the JPEG file's length for [`Encoding::Jpeg`] (1..=[`JPEG_FRAME_MAX`]).
    pub frame_len: u32,
    /// Offset of `data` in the frame.
    pub offset: u32,
    /// The bytes, at most [`CHUNK_MAX`].
    #[serde(serialize_with = "serialize_bytes")]
    pub data: &'a [u8],
}

/// Serialises a byte slice as postcard bytes (length, then a copy), not element by element.
fn serialize_bytes<S: serde::Serializer>(data: &&[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(data)
}

/// Chunks of a frame of `frame_len` bytes.
pub const fn chunk_count(frame_len: u32) -> u32 {
    frame_len.div_ceil(CHUNK_MAX as u32)
}

impl FrameChunk<'_> {
    /// Index of this chunk in its frame (`offset / CHUNK_MAX`).
    pub const fn index(&self) -> u32 {
        self.offset / CHUNK_MAX as u32
    }

    /// `true` when the chunk lies on the chunk grid of a frame of its encoding and size.
    fn well_formed(&self) -> bool {
        let chunk = CHUNK_MAX as u32;
        let len = self.data.len() as u32;
        self.encoding
            .frame_len_ok(self.width, self.height, self.frame_len)
            && self.offset.is_multiple_of(chunk)
            && self.offset < self.frame_len
            && len == chunk.min(self.frame_len - self.offset)
    }
}

/// A camera stream message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Message<'a> {
    /// `0x20`.
    Request(Request),
    /// `0x21`.
    Response(Response),
    /// `0x22`.
    Chunk(FrameChunk<'a>),
}

impl<'a> Message<'a> {
    /// Encodes the message with its header into `buf`; returns the datagram (a prefix of
    /// `buf`). A buffer of [`MAX_DATAGRAM`] bytes holds every message.
    pub fn encode<'b>(&self, buf: &'b mut [u8]) -> Result<&'b [u8], Error> {
        match self {
            Self::Request(r) => encode(kind::CAM_REQUEST, r, buf),
            Self::Response(r) => encode(kind::CAM_RESPONSE, r, buf),
            Self::Chunk(c) => {
                let n = c.encode_header(buf)?;
                buf[n..n + c.data.len()].copy_from_slice(c.data);
                Ok(&buf[..n + c.data.len()])
            }
        }
    }

    /// Decodes one datagram. A chunk borrows its data from `datagram`.
    pub fn decode(datagram: &'a [u8]) -> Result<Self, Error> {
        let (k, body) = read_header(datagram)?;
        match k {
            kind::CAM_REQUEST => decode_body(body).map(Self::Request),
            kind::CAM_RESPONSE => decode_body(body).map(Self::Response),
            kind::FRAME_CHUNK => {
                let c: FrameChunk<'a> = decode_body(body)?;
                if c.data.len() > CHUNK_MAX {
                    Err(Error::ChunkTooLong)
                } else if !c.well_formed() {
                    Err(Error::ChunkLayout)
                } else {
                    Ok(Self::Chunk(c))
                }
            }
            other => Err(Error::Kind(other)),
        }
    }
}

impl Request {
    /// [`Message::encode`] for a request.
    pub fn encode<'b>(&self, buf: &'b mut [u8]) -> Result<&'b [u8], Error> {
        Message::Request(*self).encode(buf)
    }
}

impl Response {
    /// [`Message::encode`] for a response.
    pub fn encode<'b>(&self, buf: &'b mut [u8]) -> Result<&'b [u8], Error> {
        Message::Response(*self).encode(buf)
    }
}

impl FrameChunk<'_> {
    /// [`Message::encode`] for a chunk.
    pub fn encode<'b>(&self, buf: &'b mut [u8]) -> Result<&'b [u8], Error> {
        Message::Chunk(*self).encode(buf)
    }

    /// Writes the chunk's datagram into `buf` except its data bytes, and returns `n`, the
    /// offset of the data: the datagram is `buf[..n + self.data.len()]` once the caller has
    /// put the data at `buf[n..]`. Only the length of `self.data` is used; its bytes are not
    /// read. So a sender can copy the data itself (with a faster copy, or a transform such as
    /// RAW10 to RAW8), or send header and data from two buffers. The bytes are those of
    /// [`encode`](Self::encode).
    ///
    /// Fails like `encode`: [`Error::ChunkTooLong`] above [`CHUNK_MAX`] bytes of data,
    /// [`Error::BufferFull`] if `buf` cannot hold the header and the data.
    pub fn encode_header(&self, buf: &mut [u8]) -> Result<usize, Error> {
        if self.data.len() > CHUNK_MAX {
            return Err(Error::ChunkTooLong);
        }
        // The fields in `FrameChunk`'s order; postcard writes `data` as its length (a varint,
        // as it writes a `usize`), then the bytes.
        let head = ChunkHead {
            frame: self.frame,
            generation: self.generation,
            encoding: self.encoding,
            width: self.width,
            height: self.height,
            timestamp_us: self.timestamp_us,
            frame_len: self.frame_len,
            offset: self.offset,
            data_len: self.data.len(),
        };
        let n = encode(kind::FRAME_CHUNK, &head, buf)?.len();
        if buf.len() - n < self.data.len() {
            return Err(Error::BufferFull);
        }
        Ok(n)
    }
}

/// What postcard writes of a [`FrameChunk`] before its data bytes.
#[derive(Serialize)]
struct ChunkHead {
    frame: u32,
    generation: u32,
    encoding: Encoding,
    width: u16,
    height: u16,
    timestamp_us: u64,
    frame_len: u32,
    offset: u32,
    data_len: usize,
}
