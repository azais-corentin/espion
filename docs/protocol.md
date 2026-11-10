# UDP protocol

`crates/espion-proto` is the source of truth for this page. Any format change bumps the
protocol version and updates this page and its changelog in the same commit.

> **Status.** The tracking messages (`0x01`..`0x11`) are a draft, not implemented. Settled:
> header, versioning (exact match), boards identified by their Ethernet MAC, Q11.5 blob
> coordinates; their other fields may still change. The [camera stream](#camera-stream)
> (`0x20`..`0x22`) is implemented (version 3).

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
| 2 | 1 | version | Protocol version, currently `3` |
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
  `fe80::4553:9281`; the host reaches it with the interface scope (`fe80::4553:9281%eth0`) and
  binds its own link-local address on that interface, so nothing leaves elsewhere.
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
| op | `Op` | `GetState`, `SetCamera(CameraSettings)`, `SetStream(StreamSettings)` or `Poll` |

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

`StreamSettings`:

| Field | Type | Meaning |
|---|---|---|
| enabled | `bool` | Send frames (while a host polls) |
| encoding | `Encoding` | `Raw8`, `Raw10` or `Jpeg` on the wire |
| max_fps | `u16` | Most frames sent per second; 0: no limit |
| jpeg_quality | `u8` | JPEG quality, 1..=100 (`JPEG_QUALITY_MIN`..=`JPEG_QUALITY_MAX`, IJG scale); with `auto_quality` the ceiling. Checked and kept whatever the encoding |
| auto_quality | `bool` | Automatic JPEG quality (below) |

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

`ModeInfo`: `sensor_clock_mhz: u8` (80 or 96), `exceeds_datasheet_clock: bool` (96 MHz),
`hts: u16`, `vts: u16`, `psv: bool`, `frame_rate_mhz: u32` (at the nominal 24 MHz XVCLK),
`rate_error_mhz: i32` (rate minus request), `max_exposure_lines: u16`, `line_ns: u32`,
`frame_bytes: u32` (from the sensor).

`EncoderState`:

| Field | Type | Meaning |
|---|---|---|
| quality | `u8` | Quality the next JPEG frame is encoded at: `jpeg_quality`, or lower while the automatic quality holds it down |
| frame_len | `u32` | Bytes of the last JPEG frame encoded (the whole file); 0 before the first |
| encode_us | `u32` | Time the encoder took for that frame, µs |
| errors | `u32` | Encoder errors since boot (output too small, codec or DMA error, timeout); that frame is lost. Wraps |
| frames_skipped | `u32` | Captured frames the encoder could not take since boot because it was still busy with the previous one; not in `counters.frames_skipped` (the link). Wraps |

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
  goes capture → encoder → link, one frame in each: a captured frame the encoder cannot take
  because it is still busy with the previous one is skipped and counted in
  `encoder.frames_skipped`; an encoded frame that finds the link still busy with the
  previous one is skipped and counted in `frames_skipped`. A frame the encoder fails on is
  counted in `encoder.errors` and not sent.
- **Automatic JPEG quality** (`auto_quality` on, while the stream sends `Jpeg`): at the end
  of every 200 ms (`AUTO_QUALITY_PERIOD_MS`) the board takes one step of
  `cam::auto_quality(in_use, set, skipped, busy_pct)`, which the board and the fake camera
  share: `skipped` is whether `frames_skipped` grew in the period (the link), `busy_pct` the
  share of the period the link was busy sending frames (from the first to the last chunk of
  each). Skipped: the quality in use drops by an eighth (at least 2) down to 10
  (`AUTO_QUALITY_MIN`, or the set quality if lower), never up. Otherwise it rises by 1
  towards `jpeg_quality` while `busy_pct` is below 80 (`AUTO_QUALITY_ROOM_PCT`) and holds
  above, so it settles below the link's limit instead of oscillating against it. Never above
  `jpeg_quality`. With `auto_quality` off the quality in use is `jpeg_quality`.
- **Start-up defaults** (board and fake camera): 1280x800 RAW8 crop, 120 Hz, stream on,
  encoding `Jpeg`, quality 90, automatic quality on, no `max_fps` limit.
- **Loss.** No retransmission. A frame with a missing chunk is dropped by the host, never
  repaired: when a chunk of a newer frame arrives (or after a timeout), the incomplete frame
  is discarded and counted.
- **Link budget** (computed, 100 Mbit/s): a 1452-byte datagram takes 1538 bytes of line
  time with UDP, IPv6, Ethernet header and FCS, preamble and inter-frame gap, so at most
  8127 chunks/s, 11.45 MB/s of pixels: 1280x800 RAW8 (727 chunks) at most 11.2 fps, RAW10
  (909 chunks) 8.9 fps, 640x400 RAW8 (182 chunks) 44.7 fps. A 45 kB JPEG (1280x800 at
  quality 90 on the encoder's test scene, `firmware/esp32p4-jpeg`) takes 32 chunks: at most
  254 fps, so the sensor's 153.98 Hz fits with room.

## Changelog

| Version | Change |
|---|---|
| 1 | Initial draft (not implemented). Revised before implementation for Ethernet + PTP: Announce (`0x03`) dropped, boards identified by their Ethernet MAC, `SyncState` carries the grandmaster clockIdentity instead of a timescale id |
| 2 | Camera stream added (`0x20` CamRequest, `0x21` CamResponse, `0x22` FrameChunk) over IPv6 link-local, port 5009, for `poc-p4-stream` and `poc-viewer`. The tracking kinds are unchanged and still a draft |
| 3 | Camera stream: encoding `Jpeg` (one baseline greyscale JPEG file per frame, variable `frame_len` up to `JPEG_FRAME_MAX`, RAW8 modes with both sides multiples of 8); `StreamSettings` gains `jpeg_quality` and `auto_quality`; `State` gains `encoder: EncoderState`; `CamError` gains `Jpeg` and `Quality`; start-up default `Jpeg` at quality 90 with automatic quality. Everything else of version 2 stays |
