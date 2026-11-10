# poc-viewer

Live view and control of the OV9281 camera of the ESP32-P4-ETH proof of concept, over the
camera stream of `espion-proto` (`docs/protocol.md`, "Camera stream"; protocol version 2). The
board runs `firmware/poc-p4-stream` with its `Stream` service; the viewer shows its frames as
they arrive and sends every camera setting the `ov9281` driver knows (format, size, crop or 2x2
binning, origin, frame rate and tolerance, exposure, gain, test pattern) and the stream settings
(on or off, RAW8 or RAW10 on the wire, a frame-rate limit). Not part of the product.

- `src/net.rs`: the network thread: one request in flight, repeated every 200 ms until answered,
  a poll every 250 ms (which keeps the board's stream alive), chunk reassembly into the newest
  complete frame, statistics.
- `src/camera.rs`: the board's checks of a `SetCamera`, run here as the user edits, so the
  window shows the driver's verdict (and the HTS, VTS, clock and longest exposure it chose)
  before anything is sent. The board's answer stays authoritative.
- `src/fake.rs`: a fake camera speaking the whole protocol, with synthetic frames; the
  reference behaviour of the firmware, and what the end-to-end tests run against.
- `src/main.rs`, `src/app.rs`, `src/display.rs`: the window. Frames go to the GPU as they came
  off the wire (RAW8 or packed RAW10, expanded in the shader).

## Run

From the repository root, in the dev shell.

Against the board (link-local, on the USB adapter `eth0`; the default address):

```sh
cargo run -p poc-viewer --release
cargo run -p poc-viewer --release -- '[fe80::4553:9281%eth0]:5009'   # the same, spelt out
```

The board needs nothing from this computer's network set-up: the viewer binds its own
link-local address on the camera's interface and talks first, so the board's answers and frames
come back as replies. The viewer adopts the board's settings at its first answer. Settings can
also be sent at start, for example `--size 640x400 --binned --rate 250` or `--format raw10
--encoding raw10` (`--help` lists them).

Against the fake camera (on loopback, no board):

```sh
cargo run -p poc-viewer --release -- --fake                       # as fast as it can
cargo run -p poc-viewer --release -- --fake --fake-mbps 100       # the board's link
cargo run -p poc-viewer --release -- --fake --fake-loss 0.001     # 0.1 % of datagrams lost
cargo run -p poc-viewer --release --bin poc-fake-cam              # the fake on its own, [::1]:5009
```

## The window

One column of settings and status on the left (390 points wide, scrolls), the picture in the
rest:

- **Status**: connected or DISCONNECTED (no answer for 1.5 s), the addresses and the receive
  buffer the kernel granted, then frames per second received whole and presented, Mbit/s of
  UDP payload, frames dropped for a missing chunk, frames replaced before they were shown,
  frames the board skipped (link busy), the board's capture errors, the request round trip,
  the frame shown (size, encoding, number, generation) and the UI frame time. Below it, in red,
  the board's last refusal (`Refused the camera settings: ...`, with the driver's values), until
  a request of the same kind succeeds.
- **Sensor mode**: format, readout, width, height, origin (or centred), rate and tolerance;
  the driver's verdict on the edited mode; **Apply mode** sends it (disabled while the local
  check fails).
- **Controls (applied at once)**: exposure in lines (up to the mode's longest), gain, test
  pattern. Sent on every change; the board changes them without restarting the stream.
- **Stream (applied at once)**: send frames, RAW8 or RAW10 packed on the wire, frames per
  second at most.
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

## Self-test switch

`--selftest SECONDS` runs that long, measures over the last two thirds (at most 2 s of warm-up),
prints the statistics (received and presented frames per second, Mbit/s, dropped and replaced
frames, UI frame time mean / p50 / p99 / max, texture upload time, the board's counters over
the window, round trip), saves a screenshot (`--screenshot PATH`, default
`/tmp/poc-p4-eth/viewer/selftest.png`) and exits. `--no-vsync` presents without vsync.

```sh
cargo run -p poc-viewer --release -- --selftest 8 --screenshot /tmp/shot.png \
  --size 640x400 --binned --rate 250
```

## Tests

- `cargo test -p poc-viewer`: the fake camera and the network layer over loopback
  (`tests/e2e.rs`), the checks and the address parsing (unit tests). No board needed.
- `tests/board.rs`, against the real board, ignored by default. With the board on `eth0`
  running `poc-p4-stream` (`POC_CAMERA` overrides the address):

  ```sh
  cargo test -p poc-viewer --release --test board -- --ignored --nocapture --test-threads=1 settings
  POC_ONLY=refusals,raw10 cargo test ...   # only these sections
  POC_SOAK_SECS=600 cargo test -p poc-viewer --release --test board -- --ignored --nocapture soak
  ```

  `settings` has the sections `sizes`, `rates`, `refusals`, `brightness`, `patterns`, `raw10`,
  `stream`, `rtt`, `faults`, `garbage` and `timeout`: it applies each setting, checks the answer
  against the `ov9281` driver run here (the state, or the refusal with its values), and for each
  accepted mode the frames: right size and layout, whole, numbers increasing, the rate at the
  host against the sensor rate and the link rate for that frame size, no capture error on the
  board. It prints one line per step and fails at the end with every failed check. `soak`
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
