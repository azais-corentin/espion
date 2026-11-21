# poc-viewer

Live view and control of the OV9281 camera of the ESP32-P4-ETH proof of concept, over the
camera stream of `espion-proto` (`docs/protocol.md`, "Camera stream"; protocol version 6). The
board runs `firmware/poc-p4-stream` with its `Stream` service; the viewer shows its frames as
they arrive and sends every camera setting the `ov9281` driver knows (format, size, crop or 2x2
binning, origin, frame rate and tolerance, exposure or automatic exposure, gain, test pattern),
the stream settings (on or off, JPEG, RAW8 or RAW10 on the wire, the JPEG quality and automatic
quality, a frame-rate limit) and the marker detection settings (on or off, the threshold, the
area filter). It draws the markers the board finds (IR spots) over the picture, with their
trails and their statistics. Not part of the product.

- `src/net.rs`: the network thread: one request in flight, repeated every 200 ms until answered,
  a poll every 250 ms (which keeps the board's stream alive), chunk reassembly into the newest
  complete frame, the last 1024 blob frames, statistics.
- `src/blobs.rs`: the markers as the window shows them: the blob frames of the last seconds,
  the statistics of one marker (mean, spread, detection rate) and the trails.
- `src/camera.rs`: the board's checks of a `SetCamera`, run here as the user edits, so the
  window shows the driver's verdict (and the HTS, VTS, clock and longest exposure it chose)
  before anything is sent. The board's answer stays authoritative.
- `src/stream.rs`: the stream settings as the window edits them: the JPEG quality set by hand,
  kept apart from the ceiling sent with automatic quality.
- `src/echo.rs`: the detection settings sent and not yet shown by the board, so the board's
  answer to an older request does not undo a newer edit while a change the board makes itself
  (the threshold scaled by a mode change to the other format) reaches the editor.
- `src/jpeg.rs`: JPEG frames: the header check (one baseline, 8-bit, greyscale file of the
  frame's size, SOI to EOI) and decoding to RAW8 with `zune-jpeg`.
- `src/decode.rs`: the decoding stage between the network thread and the window, on its own
  thread: it decodes only what can be shown (below).
- `src/fake.rs`: a fake camera speaking the whole protocol, with synthetic frames, JPEG
  included, and marker detection; the reference behaviour of the firmware, and what the
  end-to-end tests run against.
- `src/timing.rs`: where a frame's time goes from the start of its exposure to the GPU (below,
  "Pipeline timing").
- `src/main.rs`, `src/app.rs`, `src/display.rs`, `src/ui/`: the window. `src/ui/` holds its
  look (`theme.rs`) and its controls, painted by hand (`widgets.rs`, the picture, the sensor
  window picker, the timing diagrams). Frames go to the GPU as they came off the wire, a JPEG
  frame as the decoding stage made it (RAW8 or packed RAW10, expanded in the shader).
  `src/selftest.rs`: the self-test switch.

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
quality off). Automatic exposure is on unless `--exposure` or `--gain` is given;
`--auto-exposure false` turns it off.

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

With the test pattern off it shows a **markers scene**: a dark background (level 40 of 1023
with noise of 4, the same for every frame number but shifted row by row), the frame number in
dark digits (level 0, so it never makes a blob), and three Gaussian spots (sigma 1.5 px, peak
brightness 1.0, 0.85 and 0.7 of the brightest) that move along Lissajous paths, each in its
third of the picture, by more than half a pixel per frame. Their brightness grows with exposure
times gain, so automatic exposure has something to settle on. The true centres are known
(`fake::marker_centres`), which is what the accuracy test checks the blobs against. The test
patterns keep their white bar and white frame number.

Detection runs as on the board: a detector thread takes each captured frame while it is free;
a frame captured while it is busy is not detected and counts in `detect.frames_skipped`. It runs
whether or not frames are sent. It works in two steps: the threshold **finds** the blobs
(`espion-vision`'s detector, RAW8 or RAW10 at the sensor's depth), then
`espion_vision::refine` **measures** each one (an iterated Gaussian-weighted centroid with the
local background, the median of a ring around the
spot, taken off), giving its centre, width (sigma) and flux. The area filter, the automatic
threshold (from the frame's background and the highest peak kept) and the overflow count follow
`docs/protocol.md`; the blob frames go to an active host only, after pending answers. Automatic
exposure takes one step every 100 ms with the highest peak of the blobs sent in that period.

Measured against the true centres (`blobs_are_at_the_true_centres_of_their_frame`, 1280x800,
120 blobs per stream):

| Stream | Largest error | Mean signed error x / y |
|---|---|---|
| RAW8 | 0.024 px | +0.0002 / −0.0009 px |
| RAW10 | 0.027 px | −0.0000 / −0.0006 px |
| JPEG | as RAW8 (the blobs come from the sensor's frame, not the JPEG) | |

The threshold alone (the centroid of the pixels above it) was off by up to 0.2 px: it cuts the
spot's skirt unevenly, which pulls the centre towards the pixel grid. Refining removes that.

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

Dark neutral greys around the picture; colour means a pipeline stage (exposure amber, readout
teal, encode indigo, send rust, decode blue, draw rose, detection lime, the markers' measure a
paler lime), the same in the timing diagrams, on the settings that act on that stage and on its
counters. The markers drawn over
the picture are detection's lime: a yellow-green no other stage uses and far from the grey
picture, so a cross shows on any frame. Red is for faults and refusals only. Type is IBM Plex
Sans, with IBM Plex Mono for every number that changes (`assets/fonts`, SIL Open Font License
1.1, `OFL.txt`).

- **Status bar** (top): the link (Connected, or Disconnected after 1.5 s without an answer)
  and the camera's address; frames per second received whole and presented, Mbit/s of UDP
  payload, frames per second **Detected** (blob frames received) and the **Latency** of the
  markers from the pipeline timing (the start of the exposure to the blobs here; "–" while the
  board does not detect), each with its last minute as a line; the number of faults when there are any
  (frames dropped for a missing chunk and decode failures since the viewer started, the
  board's capture and encoder errors since it booted, a detector fault); "Waiting for an
  answer" while a request is unanswered;
  **Counters** opens the drawer.
- **Rail** (left, 340 points, scrolls). At its top, in red, the board's last refusal (`The
  board refused the camera settings: ...`, with its values) until a request of the same kind
  succeeds, and the last decode failure.
  - **Sensor mode**: format, readout, the **sensor window** (the 1280x800 active area to scale:
    drag the window to move it, its lower right corner to resize it, on the driver's grid),
    width and height, **Centred origin** or the origin, rate and tolerance; the driver's
    verdict on the edited mode (rate, clock, HTS, VTS, PSV, longest exposure, the sizes and
    rates allowed); **Apply mode** sends it (disabled while the local check fails, filled while
    the edit differs from the board's mode). Changing the readout keeps the window's place on
    the sensor (binning halves the size, cropping doubles it). When the stream's encoding cannot
    be made of the edited mode the rail says so and Apply moves the stream to RAW8 first.
  - **Exposure and gain** (applied at once): **Automatic exposure**, exposure (in ms and lines,
    up to the mode's longest), gain, test pattern. The board changes them without restarting
    the stream. With automatic exposure on, the board chooses exposure and gain, the sliders
    are disabled and show the values it has in use ("board").
  - **Stream** (applied at once): **Send frames**; the encoding (**JPEG**, **RAW8**, **RAW10
    packed**; one the board's mode cannot make is disabled and says why); **Automatic JPEG
    quality**; **JPEG quality**; frames per second at most. With automatic quality on, the
    board chooses the quality, up to 90 (sent as the ceiling), and the slider shows the quality
    it has in use. With it off, the slider sets the quality; the viewer keeps that value while
    automatic quality is on and sends it again when it is turned off.
  - **Markers** (applied at once): **Detect markers**; **Automatic threshold** and
    **Threshold** (disabled while automatic, then showing the board's threshold in use, at the
    sensor's depth); **Automatic area filter**, **Min area** and **Max area** (pixels at or
    above the threshold, both bounds kept; automatic keeps from 2 up). A value the board would
    refuse is said in the rail before it is sent. In a mode the board streams as whole frames
    only, it does not detect: the rail, the Markers panel and the Detected figure say so, the
    settings stay editable and are kept for the next mode.
  - **Display**: **Fit**, **1:1** or the zoom, and the display gain (brightness on screen
    only); **Show centroids** (a cross at each blob's centre, a circle of twice its width, its
    number), **Show trail** (each marker's centres over **Trail length**, fading), **Threshold
    mask** (the pixels at or above the threshold in use, tinted lime) and the **Statistics
    window** of the Markers panel.
- **Picture**: fitted, or zoomed with the wheel about the pointer and moved by dragging; a
  double click fits it again. The pixel under the pointer is shown (x, y and its value at the
  frame's depth). With frames off, an empty canvas of the mode's size with the markers on it
  ("Frames off: the board's blobs only"). Each marker drawn is also an accessible label
  (`Marker N: x .. y ..`).
- **Markers** (under the picture): the newest blob frame (number, blobs, threshold, detect
  time, overflow), each of its blobs (x, y, width, flux, area, peak), and, when the window
  holds frames with exactly one blob, that marker's statistics over the statistics window:
  mean position, spread (sigma x, y), peak, area, width, flux and the detection rate.
- **Pipeline timing** (under the picture): the ribbon, always; **Expand timing** adds the
  staircase (next section).
- **Counters** (drawer, right): everything the board and the viewer count, by stage. Sensor:
  the mode as applied (HTS, VTS, PSV, rate and its error, line time, longest exposure, bytes per
  frame, exposure and gain), frames captured, capture errors. Encoder: quality in use, encode
  time, last JPEG, errors, frames it could not take. Link: received, Mbit/s, compressed frame
  size and ratio, frames sent and skipped by the board, datagrams, frames dropped for a missing
  chunk or cut by a mode change, the request round trip, the addresses and the receive buffer.
  Decode: frames per second, time, failures. Display: presented, replaced before shown, the
  frame shown, the UI frame time. Board: generation, uptime, stream. Detector: the settings,
  frames detected and skipped (busy), detect time mean and max, the threshold in use and the
  background (median, sigma).

Every control has its own accessible name (an option of a choice is `name: option`, e.g.
`Readout: 2x2 binned`), so the `egui` MCP tools find it.

## Pipeline timing

Where a frame's time goes, from the start of its exposure to its markers arriving here and its
picture's hand-off to the GPU, as the typical frame of the last 2 s, redrawn once a second (a
new figure glides into place in 150 ms), so it can be read while frames come at hundreds per
second. After the sensor a frame goes two ways, the two lanes: the **image** and the
**markers**.

| Lane | Stage | Source |
|---|---|---|
| Sensor | Exposure | Computed: exposure lines x line time, right before the readout |
| Sensor | Readout | Computed: output lines x line time, ending at the frame's capture time |
| Image | Encode (JPEG) | The board (`State::timing`, protocol version 4 and later) |
| Image | Send | From the board taking the frame to its last chunk arriving here |
| Image | Decode (JPEG) | Here, every frame presented |
| Image | Draw | Here: the pass that took the picture, to the end of its texture upload |
| Markers | Detect | The board (`FrameBlobs::timing`, protocol version 6): core 1's detector on the frame's strips, from its start on the first strip, during the readout, to the release of the last |
| Markers | Measure | The board: closing the frame and measuring its blobs on the assembled frame, to the result's hand-off to core 0 (the next frame's strips go first) |
| Markers | Send | From the board queuing the blobs to their arrival here |
| Markers | Draw | Here: the first pass that drew the blobs (with frames on, over their picture), to the renderer's hand-off |

The image's board times come with the state, 4 times a second, so they are a sample of the
frames; the network thread joins each with the frame's arrival here. The markers' come with
every blob frame. The smallest "arrival minus board time" of the window, over the frame chunks
and the blob frames alike, places the board's clock on this PC's (with frames off, the blob
frames alone); what stays unknown is the smallest transit of one datagram, by which this PC's
stages sit too early. The gaps between stages are waits: for the encoder, for the link, for the
decoding stage (it decodes the newest frame once the window took the picture before), for core
0 (it takes core 1's results every millisecond), for the window's next pass, and with frames on
for the markers' picture. What follows the upload or the hand-off (vsync, the compositor,
scan-out) is not measured. Each stage shows its median and, as a whisker, its slowest sample of
the window; computed stages are dashed.

- **Ribbon**: one row per lane on one time axis, each with its latency on the right: the image
  row (the sensor's stages, then the picture's) to the GPU, the markers row to the blobs here
  and when they were drawn. With frames off the sensor's stages get a row of their own; a lane
  without samples is left out. Under the rows, the frames that start meanwhile (one per sensor
  period).
- **Staircase**: one row per stage, grouped by lane, three consecutive frames a period apart,
  so the overlap of the stages of successive frames shows; ticks on frame 1's readout where its
  strips end. Under the axis, frame 1's latencies, and core 1's figures of the window: strips
  and their length, the longest wait from a strip's block to the detector, the longest detect,
  copy wait and hold of a strip against the hold allowed, the measure's processor time, late
  strips and frames cut. The axis grows at once when the frames no longer fit and shrinks only
  with a tenth to spare.

Measured 2026-11-11 on this desktop (as below, 119.88 Hz, vsync on) against the board,
`--selftest 8`, exposure 200 lines; ms, median of the last 2 s, stages in order with the
waits between them folded into the next start:

| Stream | Exposure | Readout | Encode | Send | Decode | Draw | Exposure to GPU | UI frame time mean / p99 |
|---|---|---|---|---|---|---|---|---|
| crop 1280x800 JPEG q90, 120.049 Hz | 1.82 | 7.28 | 5.99 | 1.27 | 0.40 | 0.38 | 26.2 | 0.42 / 0.64 |
| crop 1280x800 JPEG q90, 153.984 Hz | 1.52 | 6.07 | 5.99 | 1.20 | 0.38 | 0.38 | 26.7 | 0.42 / 0.65 |
| binned 640x400 RAW8, 250.105 Hz (link-bound, 41.7 fps) | 1.82 | 3.64 | | 22.02 | | 0.25 | 28.4 | 0.27 / 0.48 |

In the JPEG runs about 8 ms of the latency is the decoded picture waiting for the window's next
pass: the decoding stage decodes the newest frame as soon as the window took the previous
picture, so a picture is shown one display refresh after it is ready.

Measured 2026-11-21 on the headless compositor (software rendering, so the draw and the waits
for it say nothing about the desktop) against the board, crop 1280x800 RAW8 at 120.049 Hz,
automatic exposure (19 lines), one IR marker; ms after the start of the exposure, from the
diagram's summary:

| Stream | Detect | Measure | Markers here | Image to GPU | Core 1 per strip, longest |
|---|---|---|---|---|---|
| JPEG q90 | 0.65 to 7.53 | to 8.19 | 8.8 | 37.3 | 17 strips of 48 lines; detect 0.21, held 0.24 of 1.53 |
| frames off | 0.63 to 7.52 | to 8.18 | 8.8 | | detect 0.19, held 0.21 of 1.53 |

The markers are here about 1.3 ms after the end of the readout (7.45 ms). Of that, 0.7 ms is the
measure on the assembled frame, and the rest is the wait for core 0's next millisecond and the
send. With detection on, the image's encode starts about 0.5 ms later than with it off, because
core 0 gets the frame at the same hand-off, after the measure.

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
the window, the detector (settings, frames detected and skipped, detect time, blob frames
received per second) and the one marker's statistics, round trip, the pipeline timing's last
snapshot), saves a screenshot
(`--screenshot PATH`, default `/tmp/poc-p4-eth/viewer/selftest.png`) and exits. `--no-vsync`
presents without vsync.

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
  frame that does not decode; the state's timing of the last frame sent and its join with the
  frame's arrival; the markers: blobs within 0.1 px of the true centres with a mean error under
  0.02 px per axis, detection and automatic exposure on at start, blobs with frames off and
  none with detection off, a manual threshold and area filter applied and echoed, the
  automatic threshold following its rule, automatic exposure bringing the brightest peak into
  its band, and the detection refusals), the checks, the JPEG header check, the address
  parsing, the markers scene, the marker statistics and the pipeline timing (unit tests: the
  image's steps chained in order, the clock offset from the fastest datagram of either kind, raw
  streams, the markers' lane with frames off and its first draw, the cut frames of the window,
  the window and the interval, the axis); the blob frames' timing in order and on the board's
  clock. No board needed. The throughput tests (link limit, automatic
  quality, missing chunks, decoding only the frames taken) need a quiet machine.
- `tests/board.rs`, against the real board, ignored by default. With the board on `eth0`
  running `poc-p4-stream` (`POC_CAMERA` overrides the address):

  ```sh
  cargo test -p poc-viewer --release --test board -- --ignored --nocapture --test-threads=1 settings
  POC_ONLY=refusals,raw10 cargo test ...   # only these sections
  POC_SOAK_SECS=600 cargo test -p poc-viewer --release --test board -- --ignored --nocapture soak
  ```

  `settings` has the sections `sizes`, `rates`, `refusals`, `brightness`, `patterns`, `raw10`,
  `stream`, `jpeg`, `quality`, `auto`, `jpeg_refusals`, `switching`, `agreement`, `rtt`,
  `faults`, `garbage`, `timeout` and `markers`: it applies each setting, checks the answer
  against the `ov9281` driver run here (the state, or the refusal with its values), and for each
  accepted mode the frames: right size and layout, whole, numbers increasing, the rate at the
  host against the sensor rate and the link rate for that frame size, no capture error on the
  board; for JPEG every frame taken decodes as a baseline greyscale JPEG of the mode's size,
  the quality is applied and reported (larger files for higher quality), automatic quality
  lowers and raises the quality, the refusals carry their values, and a JPEG frame agrees with
  a raw frame of the same scene (`POC_ONLY=jpeg,quality,auto,jpeg_refusals,switching,agreement`
  runs only those). It prints one line per step and fails at the end with every failed check.
  `markers` (`POC_ONLY=markers`) needs one IR source in the camera's view: everything
  automatic settles within 5 s, then every frame has exactly one blob and the brightest peak is
  in the automatic exposure's band; every blob frame's timing is consistent (detected on strips
  that cover the frame, starting before the capture; strips' end, publication and queueing in
  that order) and its medians are printed; a manual threshold and area filter are applied; detection
  off sends nothing; with frames off the blobs come at the sensor rate less the skipped frames;
  per mode the coverage (frames detected / captured) and the detect time; and it prints the
  source's jitter (sigma x, y) over 10 s.
  `soak`
  streams for `POC_SOAK_SECS` with a mode change every 20 s, dropping and restarting the client
  and sending malformed datagrams in between. Both use the board's test hooks on UDP 5010
  (`firmware/poc-p4-stream/src/stream/diag.rs`): the CPU report, fault injection, a sensor
  stall.

## Driving it as an agent (`inspection`)

```sh
cargo build -p poc-viewer --release --features inspection
egui-headless -s 1600x1000 -p 5719 target/release/poc-viewer --fake
```

`egui-headless` (dev shell) runs it in a headless compositor with software rendering and sets
`EGUI_INSPECTION` to `127.0.0.1:PORT`; the inspection server listens there only with the
feature built in. Then the `egui` MCP tools attach to that port. Frame rates measured there say
nothing about the real display.
