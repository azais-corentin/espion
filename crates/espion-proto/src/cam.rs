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
}

impl Encoding {
    /// Bytes of a `width`-pixel line (RAW10 widths are multiples of 4).
    pub const fn line_bytes(self, width: u16) -> u32 {
        match self {
            Self::Raw8 => width as u32,
            Self::Raw10 => width as u32 * 5 / 4,
        }
    }

    /// Bytes of a frame (`u64`: a RAW10 frame of the largest `u16` sizes overflows `u32`).
    pub const fn frame_bytes(self, width: u16, height: u16) -> u64 {
        self.line_bytes(width) as u64 * height as u64
    }
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
/// grid or whose length disagrees with the encoding, width and height.
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
    /// Bytes of the whole frame: `encoding.frame_bytes(width, height)`.
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
        self.frame_len as u64 == self.encoding.frame_bytes(self.width, self.height)
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
                if c.data.len() > CHUNK_MAX {
                    return Err(Error::ChunkTooLong);
                }
                encode(kind::FRAME_CHUNK, c, buf)
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
}
