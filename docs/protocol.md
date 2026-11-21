# UDP protocol

`crates/espion-proto` is the source of truth for this page. Any format change bumps the
protocol version and updates this page and its changelog in the same commit.

> **Status.** The tracking messages (`0x01`..`0x11`) are a draft, not implemented. Settled:
> header, versioning (exact match), boards identified by their Ethernet MAC, Q11.5 blob
> coordinates; their other fields may still change. The [camera stream](#camera-stream)
> (`0x20`..`0x23`) is implemented (version 7).

## Transport (tracking)

- UDP over IPv4 on the wired Ethernet shared by boards and host (100 Mbit/s per board).
  Boards get their address by DHCP, so the network needs a DHCP server.
- One message per datagram, at most **1472 bytes** (1500-byte MTU minus IPv4 and UDP
  headers), so IP never fragments.
- The host is whoever last sent the board a Config or FrameEpoch. Until then the board
  broadcasts Status to port 5005 and sends no Centroids.
- No retransmission. A lost Centroids packet is a lost frame; the host copes with gaps.
- Ports (defaults, configurable):

  | Port | Direction | Traffic |
  |---|---|---|
  | 5005 (`HOST_PORT`) | board → host, unicast (Status: broadcast until the host is known) | Centroids, Status |
  | 5007 | host → board, unicast or broadcast | Config, FrameEpoch |
  | 319, 320 | multicast 224.0.1.129 | PTP event and general messages (IEEE 1588, not espion-proto; see [timing.md](timing.md)) |

## Packet layout

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 2 | magic | `0x45 0x53` (`"ES"`). Anything else is not an espion packet |
| 2 | 1 | version | Protocol version, currently `4` |
| 3 | 1 | kind | Message kind (table below) |
| 4 | … | body | [postcard](https://postcard.jamesmunns.com/wire-format)-encoded struct for (`version`, `kind`) |

The header is raw bytes, not postcard. A receiver checks magic and version before reading
the body. Packets with an unknown magic, version or kind are counted and dropped, never decoded.

## Versioning

- One version number covers everything: header, all kinds, all bodies.
- Bump it for **any** body change (field added, removed, reordered, retyped) or any change in
  what a field means. Postcard has no field tags, so no change is backward compatible.
- Never reuse a kind value, even after its message is removed.
- Firmware and host must speak the same version. The host logs each board's version and
  reports mismatches; it never decodes a foreign version.
- Every bump adds a changelog row.

## Message kinds

Tracking (draft, not implemented):

| Kind | Name | Direction | Rate |
|---|---|---|---|
| `0x01` | Centroids | board → host | Once per frame (120 Hz), split if needed |
| `0x02` | Status | board → host | 1 Hz |
| `0x03` | — | — | Reserved: was Announce in an earlier draft. PTP now does election and discovery |
| `0x10` | Config | host → board | On change |
| `0x11` | FrameEpoch | host → board | On change |
| `0x20` | CamRequest | host → board | [Camera stream](#camera-stream) |
| `0x21` | CamResponse | board → host | [Camera stream](#camera-stream) |
| `0x22` | FrameChunk | board → host | [Camera stream](#camera-stream) |
| `0x23` | FrameBlobs | board → host | [Camera stream](#camera-stream) |

Times on the wire are **network time**: PTP nanoseconds on the current grandmaster's clock
(see [timing.md](timing.md)), as `u64`.

### `0x01` Centroids

| Field | Type | Meaning |
|---|---|---|
| board | `[u8; 6]` | Ethernet MAC of the board |
| frame | `u64` | Frame number `k` since the frame epoch (same `k` on every board for the same instant) |
| capture_time | `u64` | Network time (ns) of the middle of the exposure |
| sync | `SyncState` | Locked / acquiring / free-running, plus the estimated error (ns) and the grandmaster's clockIdentity (`[u8; 8]`) |
| flags | `u8` | Bit 0: dark frame (LEDs off, ambient IR only) |
| part, parts | `u8`, `u8` | This datagram's index and the datagram count for this frame |
| blobs | `[Blob]` | Centroids found in the frame (this part) |

`Blob`: `x`, `y` as `u16` in 1/32 pixel (Q11.5: up to 2048 px, enough for full 1280 × 800
and binned 640 × 400), `area` as `u16` pixels, `peak` as `u8` intensity.

### Other messages

- **`0x02` Status:** firmware version, uptime, link state (speed, duplex), PTP port state
  (master/slave/listening/passive), grandmaster clockIdentity, PTP statistics (offset from
  master, mean path delay), PPS servo lock state, dropped-frame and dropped-packet counters.
- **`0x10` Config:** exposure (µs, default 500), threshold, min and max blob area,
  dark-frame interval N (one LED-off frame every N frames), frame-rate divider.
- **`0x11` FrameEpoch:** `t0: u64`, the network time (ns) of frame 0, and the frame period
  as a ratio `num`/`den` ns (`10⁹/120` by default). See [timing.md](timing.md).

## Camera stream

Live frames from one board's OV9281 and remote control of its settings
(`espion_proto::cam`). Used by the `poc-p4-stream` firmware on the Waveshare ESP32-P4-ETH and
the `poc-viewer` host app; not by the tracking path above.

### Transport

- UDP over **IPv6 link-local**, no IPv4, no DHCP. The board has the fixed address
  `fe80::4553:9281`, so it is reached the same way on the host's own port and through a
  switch or router the host is on. The host names the interface in the scope
  (`fe80::4553:9281%eth0`); without one, `poc-viewer` asks on every interface with a
  link-local address and keeps to the one the board answers on, so nothing leaves elsewhere
  once it is found.
- Board port **5009** (`CAM_PORT`). The host uses any port.
- **The host talks first.** The board sends nothing until a valid request arrives, then sends
  responses and frames only to the address and port the last valid request came from (a
  host firewall that drops unsolicited inbound UDP lets these through as replies).
- The board stops sending frames when no valid request arrived for **3000 ms**
  (`STREAM_TIMEOUT_MS`); the host polls well within it (`poc-viewer`: every 250 ms).
- One message per datagram, UDP payload at most **1452 bytes** (`MAX_DATAGRAM`: 1500 − 40
  IPv6 − 8 UDP), so IPv6 never fragments.
- Requests are idempotent and carry a sequence number the response echoes. The host repeats
  an unanswered request with the same number (`poc-viewer`: every 200 ms) and keeps one in
  flight; a response whose number is not the one in flight answers an earlier repeat. The
  board answers a repeat of the last request it executed (same number, same op, same source)
  with that execution's result and does not execute it again: repeats of a slow mode change
  queue up meanwhile, and a refused change must not be applied by its own repeat.

### `0x20` CamRequest (host → board)

| Field | Type | Meaning |
|---|---|---|
| seq | `u32` | Sequence number, echoed by the response |
| op | `Op` | `GetState`, `SetCamera(CameraSettings)`, `SetStream(StreamSettings)`, `SetDetect(DetectSettings)` or `Poll` |

- `GetState` reads the state; `Poll` is the host's periodic keep-alive and reads the state
  (with its counters) too. Any valid request refreshes the destination and the timeout.
- `SetCamera` is all or nothing. The board checks, in this order: the sensor mode (the
  `ov9281` builder: size, alignment, window, rate), the receiver's geometry, the stream's
  encoding against the new mode's format and size (`Encoding::check`), then exposure and
  gain; it applies nothing unless every check passes. A request whose mode resolves to the
  running one (same `ov9281::Mode`) only changes exposure, gain and test pattern; any other
  restarts the stream in the new mode.
- `SetStream` is all or nothing too: the encoding against the running mode
  (`Encoding::check`), then the JPEG quality (`StreamSettings::check`). It sets the quality
  in use (`EncoderState::quality`) to the set quality.
- `SetDetect` is all or nothing: the threshold against the running sensor format, then the
  area bounds (`DetectSettings::check`), checked whether the automatic choices are on or not.
- To go from JPEG to a RAW10 mode the host first sets a raw encoding, then the mode (and
  from a RAW10 mode to JPEG: the RAW8 mode first, then the encoding).

`CameraSettings`:

| Field | Type | Meaning |
|---|---|---|
| format | `Format` | `Raw8` or `Raw10` (sensor) |
| width, height | `u16`, `u16` | Output size (binned: binned pixels) |
| readout | `Readout` | `Crop` or `Binned` (2x2) |
| origin | `Option<Origin>` | Window corner `x`, `y` (`u16`) in sensor pixels; `None`: centred. In a state: the origin applied |
| rate_mhz | `u32` | Requested frame rate, millihertz |
| tolerance_mhz | `u32` | Largest acceptable difference from it, millihertz |
| exposure_lines | `u16` | Exposure, 1..=max for the mode |
| gain_sixteenths | `u8` | Analog gain, 16 = 1x .. 248 = 15.5x |
| test_pattern | `TestPattern` | `Off`, `Bars`, `RollingBars`, `GreySquares`, `BlackWhiteSquares`, `Random` |
| auto_exposure | `bool` | Automatic exposure (below, "Detection"): the board sets `exposure_lines` and `gain_sixteenths`, and a state reports the values in use; those of a `SetCamera` are checked as usual and are where it starts |

`StreamSettings`:

| Field | Type | Meaning |
|---|---|---|
| enabled | `bool` | Send frames (while a host polls) |
| encoding | `Encoding` | `Raw8`, `Raw10` or `Jpeg` on the wire |
| max_fps | `u16` | Most frames sent per second; 0: no limit |
| jpeg_quality | `u8` | JPEG quality, 1..=100 (`JPEG_QUALITY_MIN`..=`JPEG_QUALITY_MAX`, IJG scale); with `auto_quality` the ceiling. Checked and kept whatever the encoding |
| auto_quality | `bool` | Automatic JPEG quality (below) |

`DetectSettings`:

| Field | Type | Meaning |
|---|---|---|
| enabled | `bool` | Detect, and send `FrameBlobs` while a host polls, whether frames are sent or not |
| auto_threshold | `bool` | Automatic threshold (below, "Detection"); `threshold` is kept for when it is turned off |
| threshold | `u16` | Threshold with `auto_threshold` off, in the sensor format's units: 1..=255 RAW8, 1..=1023 RAW10. A mode change to the other format scales it (× 4 to RAW10, ÷ 4 to RAW8, at least 1) |
| auto_area | `bool` | Automatic area filter: keep the blobs of at least 2 pixels (`AUTO_MIN_AREA`); `min_area` and `max_area` are kept for when it is turned off |
| min_area, max_area | `u16`, `u16` | Area bounds with `auto_area` off, pixels, both inclusive: 1 ≤ `min_area` ≤ `max_area` |

### `0x21` CamResponse (board → host)

| Field | Type | Meaning |
|---|---|---|
| seq | `u32` | The request's sequence number |
| reply | `Reply` | `State(State)` after the request, or `Error(CamError)`: refused, nothing changed |

`State`:

| Field | Type | Meaning |
|---|---|---|
| camera | `CameraSettings` | As applied (origin resolved) |
| stream | `StreamSettings` | As applied |
| mode | `ModeInfo` | What the sensor driver chose (below) |
| generation | `u32` | Changes whenever the frame layout changes: format, size, readout, origin or encoding |
| counters | `Counters` | Since boot, each wrapping at `u32::MAX`: frames captured, frames sent, frames skipped because the previous one was still being sent, capture errors, datagrams sent |
| encoder | `EncoderState` | The JPEG encoder (below) |
| uptime_us | `u64` | µs since boot, the clock of the frame timestamps |
| timing | `Option<FrameTiming>` | The last frame sent whole and its time on the board (below); `None` before the first |
| detect | `DetectState` | The blob detector (below) |

`ModeInfo`: `sensor_clock_mhz: u8` (80 or 96), `exceeds_datasheet_clock: bool` (96 MHz),
`hts: u16`, `vts: u16`, `psv: bool`, `frame_rate_mhz: u32` (at the nominal 24 MHz XVCLK),
`rate_error_mhz: i32` (rate minus request), `max_exposure_lines: u16`, `line_ns: u32`,
`frame_bytes: u32` (from the sensor).

`EncoderState`:

| Field | Type | Meaning |
|---|---|---|
| quality | `u8` | Quality the next JPEG frame is encoded at: `jpeg_quality`, or lower while the automatic quality holds it down |
| frame_len | `u32` | Bytes of the last JPEG frame encoded (the whole file); 0 before the first |
| encode_us | `u32` | Time the encoder took for that frame, from its first rows to the end of the file, µs; for a frame fed during the readout, the rest of the readout is included |
| errors | `u32` | Encoder errors since boot (output too small, codec or DMA error, timeout); that frame is lost. Wraps |
| frames_skipped | `u32` | Captured frames the encoder could not take since boot because it was still busy with the previous one (or, fed during the readout, the frames lost in capture); not in `counters.frames_skipped` (the link). Wraps |

`DetectState`:

| Field | Type | Meaning |
|---|---|---|
| settings | `DetectSettings` | As applied |
| threshold | `u16` | Threshold the next frame is detected with (the automatic one or `settings.threshold`), in the sensor format's units |
| background_median, background_sigma | `u8`, `u8` | The last detected frame's background sample: its median and its 84th percentile minus the median, 8-bit units |
| detect_us_mean, detect_us_max | `u32`, `u32` | Detect time of the frames detected since the previous state was made, µs (0 when none) |
| frames_detected | `u32` | Frames detected since boot. Wraps |
| frames_skipped | `u32` | Captured frames the detector could not take because it was busy (detection on), and the frames in `frames_cut`. Wraps |
| frames_cut | `u32` | Frames whose detection stopped part-way because the detector fell behind the strips (a strip would have been held past `hold_budget_us`, or was written over): the rest was copied, not detected, no blobs sent. Wraps |
| blobs_dropped | `u32` | Items the detector lost: runs and components beyond its capacity (a blob that lost some comes out split, shrunk or not at all), blobs beyond its capacity or `FRAME_BLOBS_MAX`. Wraps |
| blobs_filtered | `u32` | Blobs found but dropped by the area filter. Wraps |
| fault | `bool` | The detector stopped after a fault (the board: its core trapped or stopped beating); nothing is detected until the board restarts |
| available | `bool` | The running mode can be detected; `false` for the modes the board streams as whole frames only (its detector works on strips of rows): nothing is detected, the settings are kept for the next mode. The fake camera detects every mode |

`FrameTiming`, the board's part of one frame's latency. A `Span` is `start_us`, `end_us`
(`u32`, `start_us` ≤ `end_us`): µs after that frame's `timestamp_us`. An `EncodeSpan` is
`start_us` (`i32`), `input_end_us`, `end_us` (`u32`), µs after the same timestamp, with
`start_us` ≤ `input_end_us` ≤ `end_us`: a board that encodes the frame while the sensor still
reads it out starts before the capture (`start_us` < 0).

| Field | Type | Meaning |
|---|---|---|
| frame | `u32` | The frame's number, as in its chunks |
| encode | `Option<EncodeSpan>` | The JPEG encode, from the encoder taking the frame's first rows (`start_us`) through its last rows handed over (`input_end_us`; `start_us` for a frame encoded whole) to the end of the file (`end_us`); `None` for a frame sent in a raw encoding. A positive `start_us` is the wait for the encoder |
| send | `Span` | From the send of the first chunk being taken up to the last chunk handed to the Ethernet MAC (not yet on the wire). The time before it, after the encode if any, is the wait for the link |

The host reads it at its poll rate, so it sees a sample of the frames, not each one. A host
that received the frame whole can place the board's clock against its own: the frame's last
chunk arrived no earlier than `timestamp_us + send.end_us`.

`CamError`, the first six mirroring `ov9281::Error` variant for variant (rates in mHz):

| Variant | Values | Meaning |
|---|---|---|
| Size | `max_width: u16`, `max_height: u16` | Size outside 128..=max x 2..=max of the readout |
| Alignment | `width_multiple: u8`, `height_multiple: u8` | Width or height not a multiple of the format's (RAW8 8 x 1, RAW10 64 x 2) |
| Window | — | Window outside the 1280x800 array, or origin off its grid |
| TooFast | `max_mhz: u32` | Above the window's fastest rate (96 MHz) |
| TooSlow | `min_mhz: u32` | Below the slowest rate |
| Tolerance | `nearest_mhz: u32` | No setting within the tolerance; the nearest rate |
| Receiver | — | The CSI-2 receiver refused the geometry |
| SensorBus | — | The sensor did not answer on I2C |
| Exposure | `max_lines: u16` | Exposure outside 1..=max for the mode |
| Gain | `min: u8`, `max: u8` | Gain outside min..=max sixteenths |
| Encoding | `format: Format`, `encoding: Encoding` | The encoding cannot be made from the format (RAW10 from a RAW8 sensor) |
| Jpeg | `reason: JpegRefusal`, `format: Format`, `width: u16`, `height: u16` | JPEG cannot be made of the mode (values: the mode's): `reason` `Format` (a RAW10 sensor format; checked first) or `Size` (width or height not a multiple of 8, `JPEG_ALIGN`) |
| Quality | `min: u8`, `max: u8` | JPEG quality outside `min..=max` (1..=100) |
| Threshold | `min: u16`, `max: u16` | Detection threshold outside `min..=max` for the sensor format (1..=255 RAW8, 1..=1023 RAW10) |
| Area | — | Detection area bounds: `min_area` below 1 or above `max_area` |

### `0x22` FrameChunk (board → host)

| Field | Type | Meaning |
|---|---|---|
| frame | `u32` | Frame number, counted over captured frames since boot (sent or not) |
| generation | `u32` | `State::generation` of the frame |
| encoding | `Encoding` | Pixel encoding of the frame |
| width, height | `u16`, `u16` | Frame size |
| timestamp_us | `u64` | Board time of the capture (end of frame), µs since boot |
| frame_len | `u32` | Bytes of the whole frame: `encoding.frame_bytes(width, height)` for a raw encoding; for `Jpeg` the file's length, 1..=2 097 152 (`JPEG_FRAME_MAX`), varying from frame to frame |
| offset | `u32` | Offset of `data` in the frame |
| data | `[u8]` | The bytes (postcard: length, then the bytes) |

Chunks are self-describing: a host shows a frame without having seen any response.

- **Encodings.** `Raw8`: one byte per pixel, rows top to bottom; from a RAW10 sensor the 8
  high bits. `Raw10`: CSI-2 packed, every 4 pixels P0..P3 in 5 bytes,
  `P0[9:2] P1[9:2] P2[9:2] P3[9:2]`, then `P3[1:0] << 6 | P2[1:0] << 4 | P1[1:0] << 2 | P0[1:0]`
  (the layout the P4 receives, `firmware/poc-p4-eth/README.md`); it needs a RAW10 sensor.
  `Jpeg`: one complete baseline greyscale JPEG file per frame, from SOI to EOI with its
  header (as the board's encoder writes it, `firmware/esp32p4-jpeg`), of the 8-bit picture
  of `width` x `height`. It needs a RAW8 sensor (the encoder takes 8-bit pixels) and a width
  and height that are multiples of 8 (`JPEG_ALIGN`). `frame_len` is the file's length; the
  receiver accepts at most 2 MiB (`JPEG_FRAME_MAX`: the board encoder's worst case for
  1280x800, a 384-byte header plus 2 bytes per pixel, fits). Chunks keep the 1409-byte grid
  of `frame_len`.
- **Chunking.** Every chunk but a frame's last carries exactly **1409 bytes** (`CHUNK_MAX`);
  chunk `i` has offset `i × 1409`. 1409 is what the worst case leaves: header 4 bytes plus
  the largest postcard varints of frame (5), generation (5), encoding (1), width (3),
  height (3), timestamp (10), frame length (5), offset (5) and the data length (2) is 43
  bytes (`CHUNK_OVERHEAD_MAX`), and 4 + 39 + 1409 = 1452. A receiver rejects a chunk off this
  grid, whose length disagrees with its offset and `frame_len`, or whose `frame_len` is not
  possible for the encoding, width and height (`Encoding::frame_len_ok`: exactly
  `frame_bytes` for a raw encoding; for `Jpeg` 1..=`JPEG_FRAME_MAX` with both sides non-zero
  multiples of 8). Chunks of one frame (same `frame` and `generation`) carry the same
  encoding, size and `frame_len`; a receiver drops a frame whose chunks disagree.
- **One frame in flight.** The board sends a frame's chunks in order, then the next frame's.
  A frame captured while the previous one is still being sent is skipped and counted
  (`frames_skipped`). Frames held back by `max_fps` are not counted. With `Jpeg` a frame
  goes capture → encoder → link, and each of the two stages has one frame in work and one
  waiting slot that holds the newest frame for it: a captured frame waits while the encoder
  is busy, and when a newer one is captured meanwhile the waiting one is dropped and counted
  in `encoder.frames_skipped` (so are frames the receiver overwrote before they were taken);
  an encoded file waits while the link sends the previous one, and when a newer file is
  finished meanwhile the waiting one is dropped and counted in `frames_skipped`. So the link
  never idles waiting for an encode. A frame the encoder fails on is counted in
  `encoder.errors` and not sent.
- **Automatic JPEG quality** (`auto_quality` on, while the stream sends `Jpeg`): at the end
  of every 200 ms (`AUTO_QUALITY_PERIOD_MS`) the board takes one step of
  `cam::auto_quality(in_use, set, skipped, busy_pct)`, which the board and the fake camera
  share: `skipped` is whether `frames_skipped` grew in the period (the link), `busy_pct` the
  share of the period the link was busy sending frames (from the first to the last chunk of
  each). Skipped: the quality in use drops by an eighth (at least 2) down to 10
  (`AUTO_QUALITY_MIN`, or the set quality if lower), never up. Otherwise it rises by 1
  towards `jpeg_quality` when the link has room for the step,
  `busy_pct × S(q) < 80 × S(q + 1)` (`AUTO_QUALITY_ROOM_PCT`; `S` the IJG scaling of the
  quantisation tables, `cam::ijg_scale`: 5000 / q below 50, 200 − 2q from 50, at least 1),
  so below 80 % busy less the step's expected growth (72 % at 90, 53 % at 97, 40 % at 98),
  and holds otherwise: it settles below the link's limit instead of oscillating against it.
  Never above `jpeg_quality`. With `auto_quality` off the quality in use is `jpeg_quality`.
- **Start-up defaults** (board and fake camera): 1280x800 RAW8 crop, 120 Hz, automatic
  exposure on, stream on, encoding `Jpeg`, quality 90, automatic quality on, no `max_fps`
  limit; detection on with the automatic threshold and area filter (`DetectSettings::DEFAULT`,
  manual threshold 128, area 2..=65535).
- **Loss.** No retransmission. A frame with a missing chunk is dropped by the host, never
  repaired: when a chunk of a newer frame arrives (or after a timeout), the incomplete frame
  is discarded and counted.
- **Link budget** (computed, 100 Mbit/s): a 1452-byte datagram takes 1538 bytes of line
  time with UDP, IPv6, Ethernet header and FCS, preamble and inter-frame gap, so at most
  8127 chunks/s, 11.45 MB/s of pixels: 1280x800 RAW8 (727 chunks) at most 11.2 fps, RAW10
  (909 chunks) 8.9 fps, 640x400 RAW8 (182 chunks) 44.7 fps. A 45 kB JPEG (1280x800 at
  quality 90 on the encoder's test scene, `firmware/esp32p4-jpeg`) takes 32 chunks: at most
  254 fps, so the sensor's 153.98 Hz fits with room.

### `0x23` FrameBlobs (board → host)

The blobs the board's detector found in one frame, after the area filter. One datagram per
detected frame.

| Field | Type | Meaning |
|---|---|---|
| frame | `u32` | Frame number, as in the frame's chunks |
| generation | `u32` | `State::generation` of the frame |
| timestamp_us | `u64` | Board time of the capture, as in the frame's chunks |
| format | `Format` | Sensor format of the frame: the units of `threshold` |
| width, height | `u16`, `u16` | Frame size |
| threshold | `u16` | Threshold the frame was detected with |
| detect_us | `u32` | Time the detector took for the frame, µs |
| timing | `DetectTiming` | Where the frame's detection spent its time on the board (below) |
| flags | `u8` | Bit 0 (`OVERFLOW`): blobs were lost (the detector's capacity or `FRAME_BLOBS_MAX`). Bit 1 (`AUTO_THRESHOLD`): the threshold was chosen automatically |
| blobs | `[u8]` | The blobs, 17 bytes each (`BLOB_BYTES`), in the detector's order (postcard: length, then the bytes) |

A blob is, all little-endian: `x`, `y` (`u32`), `width` (`u16`), `flux` (`u32`), `area`
(`u16`), then `peak` (`u8`). The threshold only finds the blob: `area` is its pixels at or
above the threshold, `peak` its brightest pixel, 8-bit (RAW10: its top 8 bits). The rest is
measured on the frame around it, so the threshold does not bias it (below, "Position"):
`x` and `y` the spot's centre in 1/256 pixel (`SUBPIXEL`), pixel centres at integer
coordinates; `width` its standard deviation in 1/256 pixel; `flux` the sum of its pixels
above the local background, in the format's units. This is not the tracking draft's `Blob`
(Q11.5 in `u16`), which is too coarse for this precision. At most **80 blobs**
(`FRAME_BLOBS_MAX`): header 4 bytes plus the largest varints of frame (5), generation (5),
timestamp (10), format (1), width (3), height (3), threshold (3), detect time (5), the timing
(four `u32` of 5, nine `u16` of 3: 47), flags (1) and the blob bytes' length (2) is 89 bytes
(`FRAME_BLOBS_OVERHEAD_MAX`), and 89 + 80 × 17 = 1449 ≤ 1452. A receiver rejects blob bytes
that are not whole blobs or more than that.

`DetectTiming`: µs relative to the frame's `timestamp_us` (on the board, the end of the last
strip's DMA block, as near as the board bounds it). The board detects each strip of rows while
the sensor reads out the next, so detection starts before that time. The `u16` fields
saturate.

| Field | Type | Meaning |
|---|---|---|
| strips_lead_us | `u32` | From the detector's start on the frame's first strip to the capture time |
| strips_end_us | `u32` | The last strip released (detected and copied into the frame buffer), after the capture time |
| published_us | `u32` | The frame's result handed to the board's network side, after the frame was closed and its blobs measured |
| queued_us | `u32` | This datagram queued for sending; it goes after any answer and before the next frame chunk |
| measure_us | `u16` | Processor time of the frame's end between `strips_end_us` and `published_us` (the frame buffer's cache invalidate, the measure of its blobs); the rest of that span is the next frame's strips, which go first |
| strips, strip_lines | `u16`, `u16` | Strips of the frame, rows of a full strip (the last one is shorter) |
| late_strips | `u16` | Strips held longer than one strip period |
| start_max_us | `u16` | Longest wait from a strip's block end to the detector's start on it |
| detect_max_us | `u16` | Longest detector time on one strip |
| copy_wait_max_us | `u16` | Longest wait for a strip's copy after the detector |
| hold_max_us | `u16` | Longest hold of a strip, from its block end to its release |
| hold_budget_us | `u16` | The hold a strip is allowed; past it the detector stops for the rest of the frame (`frames_cut`) |

The fake camera, which detects whole frames, sends one strip of the frame's height: lead 0,
`strips_end_us` its detect time.

#### Detection

- **Which frames.** With detection on, the detector takes every captured frame it can; one
  that arrives while it is busy with the previous one is counted (`detect.frames_skipped`)
  and not detected. Detection is independent of the frame stream: `FrameBlobs` go out while
  a host polls, whether frames are sent or not and whatever `max_fps` says. The board sends
  them after any answer and before the next frame chunk. A frame that is both sent and
  detected carries the same number, generation and timestamp in its chunks and its blobs.
- **Position** (board and fake camera share `espion_vision::refine`, integer arithmetic, so
  both give the same result for the same frame): for each blob, from its thresholded
  centroid, an iterated Gaussian-weighted centroid on the frame. The window width comes from
  the blob's area, the window spans 4 widths each side, and the local background is the
  median of the pixels on a square just outside it. Each pixel weighs (value − background)
  × a Gaussian of its distance to the current estimate. Each step moves twice the weighted
  mean offset, up to 8 steps, until it moves less than 1/512 pixel. Its fixed point is the
  centre of any symmetric spot, whatever the threshold or the window width. `width` is the
  standard deviation of the Gaussian spot with the same Gaussian-weighted second moments
  (background-subtracted, the window's width and the pixel's own size removed); `flux` the
  sum over the final window above the background. A spot within 4
  widths of the frame's edge has its window clipped, so its centre may be biased towards the
  inside.
- **Automatic threshold** (`auto_threshold` on; board and fake camera share
  `espion_vision::auto`): the detector samples every 4th pixel of every 16th row of each
  frame (RAW10: the top 8 bits) into a histogram. The background is its median and its
  84th percentile minus the median (σ). The next frame's threshold is
  `max(median + max(6σ, 8), ⌈peak / 2⌉)` in 8-bit units, between 1 and 255, with `peak` the
  highest peak of this frame's blobs that the area filter keeps (a lone hot pixel does not
  raise it). With no such blob it is the floor alone. RAW10 uses 4 times that value, so it
  selects the same pixels.
- **Automatic exposure** (`CameraSettings::auto_exposure` on): at the end of every 100 ms
  (`AUTO_EXPOSURE_PERIOD_MS`) the board takes one step of
  `cam::auto_exposure(now, peak, cap)`, shared with the fake camera. `peak` is the highest
  blob peak sent in the period (`None` without a blob). `cap` is `auto_exposure_cap(line_ns,
  max_exposure_lines)`: 0.5 ms (`AUTO_EXPOSURE_CAP_NS`, the product's LED pulse) in whole
  lines, within the mode's longest exposure. It holds with no blob or a peak within
  200..=240 (`AUTO_EXPOSURE_LOW`, `AUTO_EXPOSURE_HIGH`). Otherwise it aims lines × gain at
  the present product × 220 / peak (`AUTO_EXPOSURE_TARGET`), at most twice and at least half
  of it, and half of it for a saturated peak (255). It keeps the gain low: the exposure is
  the longest whole number of lines that reaches the aim at 1x (aim / 16, between 1 and the
  cap), and the gain makes up the rest, rounded up to a sixteenth (up to 15.5x). That lands
  within a sixteenth of the aim, finer than the band, so the step does not jump over the band
  even at exposures of a few lines, where one line is coarser than the band. Exposure and gain
  go to the sensor without restarting the stream.

## Changelog

| Version | Change |
|---|---|
| 1 | Initial draft (not implemented). Revised before implementation for Ethernet + PTP: Announce (`0x03`) dropped, boards identified by their Ethernet MAC, `SyncState` carries the grandmaster clockIdentity instead of a timescale id |
| 2 | Camera stream added (`0x20` CamRequest, `0x21` CamResponse, `0x22` FrameChunk) over IPv6 link-local, port 5009, for `poc-p4-stream` and `poc-viewer`. The tracking kinds are unchanged and still a draft |
| 3 | Camera stream: encoding `Jpeg` (one baseline greyscale JPEG file per frame, variable `frame_len` up to `JPEG_FRAME_MAX`, RAW8 modes with both sides multiples of 8); `StreamSettings` gains `jpeg_quality` and `auto_quality`; `State` gains `encoder: EncoderState`; `CamError` gains `Jpeg` and `Quality`; start-up default `Jpeg` at quality 90 with automatic quality. Everything else of version 2 stays |
| 4 | Camera stream: `State` gains `timing: Option<FrameTiming>` (the last frame sent whole: its number, the span of its JPEG encode and the span of its send, µs after its `timestamp_us`). The chunks and everything else of version 3 stay |
| 5 | Camera stream: blob detection on the board. New kind `0x23` FrameBlobs (one frame's blobs, at most 82: centre in 1/256 pixel, width and flux measured on the frame without the threshold's bias, area and peak from the threshold); `Op::SetDetect(DetectSettings)`; `State` gains `detect: DetectState`; `CameraSettings` gains `auto_exposure`; `CamError` gains `Threshold` and `Area`; `Error::BlobsLayout`. Start-up defaults: automatic exposure, detection on with the automatic threshold and area filter. Everything else of version 4 stays |
| 6 | Camera stream: `FrameBlobs` gains `timing: DetectTiming` (the frame's detection on the board: the strips' start and end, the result's publication, the datagram's queueing, the measure's processor time, and the longest per-strip start wait, detect, copy wait and hold against the hold allowed), so at most 80 blobs; `DetectState` gains `frames_cut`. Everything else of version 5 stays |
| 7 | Camera stream: `FrameTiming::encode` becomes `Option<EncodeSpan>` (`start_us: i32`, `input_end_us: u32`, `end_us: u32`), so an encode fed strip by strip during the readout can start before the capture and report when its last rows went in. Everything else of version 6 stays |
