# poc-viewer

Live view and control of the OV9281 camera of the ESP32-P4-ETH proof of concept, over the
camera stream of `espion-proto` (`docs/protocol.md`, "Camera stream"; protocol version 4). The
board runs `firmware/poc-p4-stream` with its `Stream` service; the viewer shows its frames as
they arrive and sends every camera setting the `ov9281` driver knows (format, size, crop or 2x2
binning, origin, frame rate and tolerance, exposure, gain, test pattern) and the stream settings
(on or off, JPEG, RAW8 or RAW10 on the wire, the JPEG quality and automatic quality, a
frame-rate limit). Not part of the product.

- `src/net.rs`: the network thread: one request in flight, repeated every 200 ms until answered,
  a poll every 250 ms (which keeps the board's stream alive), chunk reassembly into the newest
  complete frame, statistics.
- `src/camera.rs`: the board's checks of a `SetCamera`, run here as the user edits, so the
  window shows the driver's verdict (and the HTS, VTS, clock and longest exposure it chose)
  before anything is sent. The board's answer stays authoritative.
- `src/stream.rs`: the stream settings as the window edits them: the JPEG quality set by hand,
  kept apart from the ceiling sent with automatic quality.
- `src/jpeg.rs`: JPEG frames: the header check (one baseline, 8-bit, greyscale file of the
  frame's size, SOI to EOI) and decoding to RAW8 with `zune-jpeg`.
- `src/decode.rs`: the decoding stage between the network thread and the window, on its own
  thread: it decodes only what can be shown (below).
- `src/fake.rs`: a fake camera speaking the whole protocol, with synthetic frames, JPEG
  included; the reference behaviour of the firmware, and what the end-to-end tests run against.
- `src/main.rs`, `src/app.rs`, `src/display.rs`: the window. Frames go to the GPU as they came
  off the wire, a JPEG frame as the decoding stage made it (RAW8 or packed RAW10, expanded in
  the shader).

## Run

From the repository root, in the dev shell.

Against the board (link-local, on the USB adapter `eth0`; the default address):

```sh
cargo run -p poc-viewer --release
cargo run -p poc-viewer --release -- '[fe80::4553:9281%eth0]:5009'   # the same, spelt out
```

The board needs nothing from this computer's network set-up: the viewer binds its own
link-local address on the camera's interface and talks first, so the board's answers and frames
come back as replies. The viewer adopts the board's settings at its first answer, and again
after the link was lost or the board restarted. Settings can also be sent at start, for example
`--size 640x400 --binned --rate 250`, `--format raw10 --encoding raw10` or `--encoding jpeg
--quality 75 --auto-quality false` (`--help` lists them; `--quality` applies with automatic
quality off).

Against the fake camera (on loopback, no board):

```sh
cargo run -p poc-viewer --release -- --fake                       # as fast as it can (JPEG q90)
cargo run -p poc-viewer --release -- --fake --fake-mbps 100       # the board's link
cargo run -p poc-viewer --release -- --fake --fake-loss 0.001     # 0.1 % of datagrams lost
cargo run -p poc-viewer --release --bin poc-fake-cam              # the fake on its own, [::1]:5009
```

## The fake camera

It starts as the board does: 1280x800 RAW8 at 120 Hz, stream on, JPEG at quality 90 with
automatic quality. It checks every request as the board does (the `ov9281` driver, the
receiver's geometry, `Encoding::check` and `StreamSettings::check` of `espion-proto`) and
answers a refusal with its values. JPEG goes capture, encoder, link: the encoder (`jpeg-encoder`
on its own thread, a baseline greyscale file of the RAW8 rendering at the quality in use, IJG
tables) and the link each have one waiting slot holding the newest frame, and a frame replaced
there counts (`encoder.frames_skipped`, `frames_skipped`). Automatic quality takes one step of
`espion_proto::cam::auto_quality` every 200 ms with the link's busy share of the period, so
with `--fake-mbps` it settles where the frames fit. What the firmware must match is in
`docs/protocol.md`; the fake is the reference for it.

`jpeg-encoder` is under `(MIT OR Apache-2.0) AND IJG`: a binary that includes the fake camera
(both of this crate's) is "based in part on the work of the Independent JPEG Group".

## Decoding

JPEG frames are decoded off the UI thread by the decoding stage (`src/decode.rs`), with
`zune-jpeg` 0.5 (pure Rust, SIMD on x86, strict mode). The stage holds at most one decoded
picture for the window and decodes nothing while it waits; the network thread meanwhile keeps
only the newest complete frame. When the window takes the picture, the stage takes the newest
frame and decodes it, so frames replaced in between are never decoded ("Replaced before
shown"). A frame that fails the header check or the decoder is counted ("Decode failures") and
dropped. The network thread never waits for a decode. Raw frames pass through untouched.

The decoder was chosen by measurement on this PC (i7-13700KF, one thread, greyscale output,
the board's own files from `firmware/esp32p4-jpeg` run 5; logs in
`/tmp/poc-p4-eth/viewer-jpeg/decode-bench-*.log`):

| File | Bytes | `zune-jpeg` 0.5.15, mean per frame | libjpeg-turbo 3 (`tjbench -pixelformat gray`) |
|---|---|---|---|
| camera 1280x800 q90 | 47 093 | 0.822 ms (1217 frames/s) | 1389 Mpixel/s = 0.737 ms |
| camera 1280x800 q90 (soak) | 47 722 | 0.825 ms | 1386 Mpixel/s = 0.739 ms |
| test picture 1280x800 q90 | 143 109 | 1.426 ms | 838.5 Mpixel/s = 1.221 ms |
| camera 1280x800 q75 | 19 251 | 0.373 ms | 2203 Mpixel/s = 0.465 ms |
| camera 640x400 q90 | 17 736 | 0.232 ms | 1268 Mpixel/s = 0.202 ms |
| camera 320x200 q90 | 2 776 | 0.038 ms | 1789 Mpixel/s = 0.036 ms |

libjpeg-turbo is up to 0.2 ms per frame faster at quality 90 and slower at 75; both decode a
1280x800 frame in well under the 6.5 ms between frames at 153.98 Hz, and the stage decodes at
most one frame per presented frame (about 120 per second). `zune-jpeg` needs no C library in
the dev shell and is the decoder the header check already uses.

## The window

One column of settings and status on the left (390 points wide, scrolls), the picture in the
rest:

- **Status**: connected or DISCONNECTED (no answer for 1.5 s), the addresses and the receive
  buffer the kernel granted, then frames per second received whole, decoded and presented,
  Mbit/s of UDP payload; for JPEG the compressed frame size (last and mean over the last
  second), the compression ratio against RAW8, the decode time (last and mean) and the decode
  failures; the board's encoder (quality in use with the set quality and the automatic switch,
  encode time, encoder errors, frames the encoder could not take); frames dropped for a
  missing chunk, cut by a mode change, replaced before they were shown, frames the board
  skipped (link busy), the board's capture errors, the request round trip, the frame shown
  (size, encoding, bytes, number, generation) and the UI frame time. Below it, in red, the
  board's last refusal (`Refused the camera settings: ...`, with its values), until a request
  of the same kind succeeds, and the last decode failure.
- **Sensor mode**: format, readout, width, height, origin (or centred), rate and tolerance;
  the driver's verdict on the edited mode; **Apply mode** sends it (disabled while the local
  check fails). When the stream's encoding cannot be made of the edited mode (JPEG with a
  RAW10 mode or a size not a multiple of 8, RAW10 with a RAW8 mode) the window says so and
  Apply moves the stream to RAW8 first.
- **Controls (applied at once)**: exposure in lines (up to the mode's longest), gain, test
  pattern. Sent on every change; the board changes them without restarting the stream.
- **Stream (applied at once)**: send frames; **send JPEG**, **send RAW8** or **send RAW10
  packed** (only what the board's mode allows is enabled; "JPEG not available: ..." says why
  not); **JPEG quality** (1..=100); **automatic JPEG quality**; frames per second at most. With
  automatic quality on, the board chooses the quality, up to 90 (sent as the ceiling); the
  slider is disabled and shows the quality the board has in use. With it off, the slider sets
  the quality; the viewer keeps that value while automatic quality is on and sends it again
  when it is turned off. The board's state never replaces it with the ceiling.
- **Display**: fit to window or 1:1 pixels, display gain (brightness on screen only).
- **Board: as applied** (collapsed): the board's state in full: settings, HTS / VTS / PSV,
  rate and its error, line time, longest exposure, generation, counters, uptime.

Every control has its own label, so the `egui` MCP tools find it by name.

## Measured on the real camera

2026-11-10, this desktop (NVIDIA RTX 3080, 3840x2160 at 119.88 Hz, native Wayland, vsync on),
the board running `poc-p4-stream` on `eth0`, `--selftest 8` (measured over the last 6 s),
every window at its fastest rate (96 MHz), gain 8x, display gain 8 (screenshots and logs in
`/tmp/poc-p4-eth/stream/desktop2/`):

| Stream | Received fps | Presented fps | Mbit/s | Dropped / cut / replaced | Board skipped | UI frame time mean / p99 | Limit |
|---|---|---|---|---|---|---|---|
| crop 1280x800 RAW8, 153.984 Hz | 11.2 | 11.2 | 92.2 | 0 / 0 / 0 | 860 | 0.43 / 0.80 ms | link |
| crop 1280x720 RAW8, 169.851 Hz | 12.1 | 12.1 | 91.1 | 0 / 0 / 0 | 947 | 0.37 / 0.74 ms | link |
| crop 640x480 RAW8, 245.850 Hz | 36.6 | 36.6 | 91.5 | 0 / 0 / 0 | 1257 | 0.43 / 0.84 ms | link |
| binned 640x400 RAW8, 302.189 Hz | 44.1 | 44.1 | 91.9 | 0 / 0 / 0 | 1550 | 0.40 / 0.72 ms | link |
| binned 320x200 RAW8, 557.874 Hz | 181.1 | 119.9 | 94.3 | 0 / 0 / 367 | 2262 | 0.37 / 0.60 ms | link (received), display refresh (presented) |
| crop 1280x800 RAW10, 131.781 Hz | 8.9 | 8.9 | 91.5 | 0 / 0 / 0 | 738 | 0.41 / 0.77 ms | link |
| binned 640x400 RAW10, 302.189 Hz | 35.8 | 35.8 | 93.1 | 0 / 0 / 0 | 1600 | 0.42 / 0.77 ms | link |

The board reported 0 capture errors in every run; the request round trip read 3.9 to 5.3 ms.
The UI never limits: under 1 ms per pass at p99 (one pass of 3.2 ms in the 640x480 run).

## Measured against the fake camera (JPEG)

2026-11-11, this desktop (as above), the fake camera in the viewer (`--fake`, loopback),
`--selftest 8` (10 for the last three), JPEG at quality 90 with automatic quality unless said;
logs and screenshots in `/tmp/poc-p4-eth/viewer-jpeg/desktop/`. The fake's frames are its
synthetic picture (about 71 kB at 1280x800 q90, more than the camera's 45 to 50 kB):

| Stream | Received fps | Decoded fps | Presented fps | Decode mean / max | Mean JPEG | Mbit/s | UI frame time mean / p99 | Limit |
|---|---|---|---|---|---|---|---|---|
| 1280x800, 120.049 Hz | 120.1 | 119.6 | 119.6 | 0.80 / 1.82 ms | 71 098 B | 69.5 | 0.44 / 0.76 ms | sensor rate |
| 1280x800, 153.984 Hz | 154.0 | 119.6 | 119.6 | 0.78 / 1.71 ms | 71 074 B | 89.0 | 0.41 / 0.60 ms | display refresh (presented); sensor rate (received) |
| binned 640x400, 302.189 Hz | 302.2 | 119.9 | 119.9 | 0.23 / 0.48 ms | 18 736 B | 46.1 | 0.32 / 0.51 ms | display refresh; sensor rate |
| binned 320x200, 557.874 Hz | 557.8 | 119.8 | 119.8 | 0.08 / 0.15 ms | 5 304 B | 24.1 | 0.30 / 0.47 ms | display refresh; sensor rate |
| 1280x800, 120.049 Hz, link 95 Mbit/s | 120.0 | 119.7 | 119.7 | 0.77 / 1.73 ms | 71 098 B | 69.4 | 0.40 / 0.54 ms | sensor rate (the frames fit) |
| 1280x800, 153.984 Hz, link 95 Mbit/s | 154.0 | 119.5 | 119.5 | 0.76 / 1.66 ms | 62 406 B | 78.2 | 0.54 / 0.81 ms | display; sensor rate (received): automatic quality settled at 88, the line 87 % busy (computed), no frame skipped |
| 1280x800, 153.984 Hz, link 95 Mbit/s, set 100 | 153.9 | 119.6 | 119.6 | 0.74 / 2.72 ms | 64 840 B | 81.2 | 0.47 / 0.79 ms | display; sensor rate (received): automatic quality settled at 89, no frame skipped |
| the same, automatic quality off | 66.5 | 66.5 | 66.5 | 1.28 / 2.44 ms | 165 472 B | 89.6 | 0.35 / 0.58 ms | link: 700 frames skipped by the fake |

No frame was dropped for a missing chunk and nothing failed to decode in any run. Decoding
never limits: it runs once per presented frame, under 2.5 ms at worst. The display shows at
most one new frame per refresh (119.88 Hz); faster sources are received whole and the
frames between two refreshes are replaced without being decoded.

## Self-test switch

`--selftest SECONDS` runs that long, measures over the last two thirds (at most 2 s of warm-up),
prints the statistics (received and presented frames per second, Mbit/s, dropped and replaced
frames, UI frame time mean / p50 / p99 / max, texture upload time, JPEG frames and their mean
size, decoded frames per second and the decode time, the board's counters and encoder over
the window, round trip), saves a screenshot (`--screenshot PATH`, default
`/tmp/poc-p4-eth/viewer/selftest.png`) and exits. `--no-vsync` presents without vsync.

```sh
cargo run -p poc-viewer --release -- --selftest 8 --screenshot /tmp/shot.png \
  --size 640x400 --binned --rate 250
```

## Tests

- `cargo test -p poc-viewer`: the fake camera, the network layer and the decoding stage over
  loopback (`tests/e2e.rs`: raw frames bit-exact; JPEG frames bit-exact against the fake's
  encoder and within the codec's error of the picture; quality and automatic quality; the
  refusals; switching encodings; loss with variable-length frames; chunks that disagree with
  their frame; a board that restarts its frame numbers; only the frames taken are decoded; a
  frame that does not decode), the checks,
  the JPEG header check and the address parsing (unit tests). No board needed.
- `tests/board.rs`, against the real board, ignored by default. With the board on `eth0`
  running `poc-p4-stream` (`POC_CAMERA` overrides the address):

  ```sh
  cargo test -p poc-viewer --release --test board -- --ignored --nocapture --test-threads=1 settings
  POC_ONLY=refusals,raw10 cargo test ...   # only these sections
  POC_SOAK_SECS=600 cargo test -p poc-viewer --release --test board -- --ignored --nocapture soak
  ```

  `settings` has the sections `sizes`, `rates`, `refusals`, `brightness`, `patterns`, `raw10`,
  `stream`, `jpeg`, `quality`, `auto`, `jpeg_refusals`, `switching`, `agreement`, `rtt`,
  `faults`, `garbage` and `timeout`: it applies each setting, checks the answer
  against the `ov9281` driver run here (the state, or the refusal with its values), and for each
  accepted mode the frames: right size and layout, whole, numbers increasing, the rate at the
  host against the sensor rate and the link rate for that frame size, no capture error on the
  board; for JPEG every frame taken decodes as a baseline greyscale JPEG of the mode's size,
  the quality is applied and reported (larger files for higher quality), automatic quality
  lowers and raises the quality, the refusals carry their values, and a JPEG frame agrees with
  a raw frame of the same scene (`POC_ONLY=jpeg,quality,auto,jpeg_refusals,switching,agreement`
  runs only those). It prints one line per step and fails at the end with every failed check.
  `soak`
  streams for `POC_SOAK_SECS` with a mode change every 20 s, dropping and restarting the client
  and sending malformed datagrams in between. Both use the board's test hooks on UDP 5010
  (`firmware/poc-p4-stream/src/stream/diag.rs`): the CPU report, fault injection, a sensor
  stall.

## Driving it as an agent (`inspection`)

```sh
cargo build -p poc-viewer --release --features inspection
egui-headless -s 1600x1000 -p 5719 env EGUI_INSPECTION=1 target/release/poc-viewer --fake
```

`egui-headless` (dev shell) runs it in a headless compositor with software rendering; the
inspection server listens on `127.0.0.1:5719` only with the feature built in and
`EGUI_INSPECTION=1` set. Then the `egui` MCP tools attach to that port. Frame rates measured
there say nothing about the real display.
