# poc-p4-stream

A throwaway proof of concept on the Waveshare ESP32-P4-ETH dev board (the board of
[`poc-p4-eth`](../poc-p4-eth/README.md)): the OV9281 camera streamed live over Ethernet to
[`poc-viewer`](../../host/poc-viewer/README.md), with every camera setting taken from it (the
camera stream of `espion-proto`, `docs/protocol.md`), on Ethernet and IPv6 with esp-hal's EMAC
driver and embassy-net. Not part of the product.

The `Stream` service (the default) streams the camera as greyscale JPEG from the P4's hardware
encoder (`esp32p4-jpeg`), or raw; the `Bench` service (the network test it was built on) stays
selectable.

- Addressing: IPv6 only, the fixed link-local address `fe80::4553:9281/64`, neighbour discovery
  by smoltcp; no IPv4, no DHCP, no SLAAC. The MAC is the chip's factory MAC from eFuse
  (`80:f1:b2:da:e4:67` on the bench board): Espressif's OUI, unique per chip, so several boards
  on one switch never collide; the P4 has no radio that uses it too.
- The host always talks first (its firewall drops unsolicited UDP): the board sends only to the
  address and port a request came from.
- Status: the camera streams in JPEG at the sensor's full rate in every window measured (1280x800
  at 153.98 Hz to 320x200 binned at 557.9 Hz) while the frames fit the 100 Mbit/s link, and the
  automatic quality keeps the rate when they would not; raw frames stream at the wire rate.
  `poc-viewer`'s board test passes against the final build and viewer: every section (`sizes`
  to `timeout`, the JPEG ones included), 0 failed checks (`board-final.log` in
  `/tmp/poc-p4-eth/stream-jpeg/`; an earlier run's one `auto` failure led to the IJG-scaled
  room rule below). The
  10.6-minute soak with JPEG at 1280x800 153.98 Hz every other step (mode and encoding changes
  every 20 s over JPEG and raw, RAW8 and RAW10 modes, five sizes, a viewer restart each step,
  malformed-datagram bursts, injected encoder errors) ran without a crash or stall (see
  [Soak](#soak-jpeg)).
  The network alone sends and
  receives at the wire limit with every byte checked (see [Measured results](#measured-results)).
  esp-hal's Ethernet driver needs three workarounds on the P4 (see
  [The Ethernet driver](#the-ethernet-driver)).

## Pins

From the Waveshare schematic
([ESP32-P4-ETH-datasheet.pdf](https://files.waveshare.com/wiki/ESP32-P4-ETH/ESP32-P4-ETH-datasheet.pdf),
sheet 1, the IP101GRI block U10), each matching Waveshare's ESP-IDF example
(`waveshareteam/ESP32-P4-Platform`, `examples/esp-idf/11_ethernetbasic`, the `ethernet_init`
Kconfig defaults for the P4: MDC 31, MDIO 52, PHY reset 51, PHY address 1, RMII clock input)
and [ESPHome's board page](https://devices.esphome.io/devices/waveshare-esp32-p4-eth/) (MDC 31,
MDIO 52, power 51, `CLK_EXT_IN` GPIO50, `phy_addr: 1`). The data pins are the P4's fixed EMAC
IO_MUX pads, the ones esp-hal's driver documents.

| Signal | U10 (IP101GRI) pin | P4 |
|---|---|---|
| TXD0 | 9 | GPIO34 |
| TXD1 | 8 | GPIO35 (also the BOOT strap, which the CH343 auto-reset circuit drives during reset) |
| TX_EN | 5 | GPIO49 |
| RXD0 | 18 | GPIO29 |
| RXD1 | 17 | GPIO30 |
| CRS_DV | 19 | GPIO28 |
| MDC | 22 | GPIO31 |
| MDIO | 23 (1.5 k pull-up R59) | GPIO52 |
| 50 MHz RMII reference clock | 14 (50M_CLKO), tied to 10 (50M_CLKI) | GPIO50, input (`ExternalRefClock`) |
| RESET (active low; pulls R67/R70 not fitted) | 32 | GPIO51 |

The PHY makes the reference clock from its 25 MHz crystal (pins 2, 3). PHY address 1: strap
PHY_AD0 (pin 11) high through R65, PHY_AD3 (pin 12) low through R63. RMII mode: strap COL/RMII
(pin 4) high through R49. The firmware holds the reset low for 10 ms and waits 10 ms after it
[INFERENCE: the IP101G's minimum reset width, not measured]. The PHY answers at MDIO address 1
with ID `0x0243 0x0c54` (IC Plus IP101G).

The camera is set up as in `poc-p4-eth` (SCCB on GPIO7/GPIO8, LDO VO3 at 2.5 V, the
`esp32p4-csi` whole-frame path, mode 1280x800 RAW8 at 120 fps from the `ov9281` crate).

## Build, flash, run

From this directory, inside the dev shell (or prefix with `nix develop -c` from the repo root).
`PORT` is the CH343's serial device: `ls /dev/serial/by-id/` lists it as
`usb-1a86_USB_Single_Serial_<serial>-if00`.

```sh
PORT=/dev/serial/by-id/usb-1a86_USB_Single_Serial_<serial>-if00
cargo build --release
E=target/riscv32imafc-unknown-none-elf/release/poc-p4-stream
espflash flash   --chip esp32p4 --port "$PORT" --non-interactive "$E"
espflash monitor --chip esp32p4 --port "$PORT" --non-interactive --no-addresses \
  --monitor-baud 2000000 --elf "$E"
```

`ESPFLASH_PORT=$PORT cargo run --release` does both (the runner in `.cargo/config.toml` has no
port). The monitor resets the board, and the firmware runs on after the monitor ends. The
stream service is ready once the log shows `stream: ready` and
`net: link up, 100 Mbit/s full duplex`. Then, from the repository root:

```sh
cargo run -p poc-viewer --release                     # the window, [fe80::4553:9281%eth0]:5009
cargo test -p poc-viewer --release --test board -- --ignored --nocapture --test-threads=1 settings
python3 firmware/poc-p4-stream/tools/streamdiag.py mark     # the board's CPU report: mark,
python3 firmware/poc-p4-stream/tools/streamdiag.py report   # then report,
python3 firmware/poc-p4-stream/tools/streamdiag.py irq      # the interrupt handlers,
python3 firmware/poc-p4-stream/tools/streamdiag.py net      # and the send path (copies, driver)
```

Cargo features: `accounting` (default) builds the CPU accounts; `--no-default-features`
leaves them out; `--features idle-counter` adds the idle counter (see
[What the firmware does](#what-the-firmware-does)).

With `SERVICE = Service::Bench` the board runs the network test service instead (ready at
`bench: ready`); on the host, with the board on the interface `eth0` (default; `--iface`
otherwise):

```sh
ping -6 -c 20 fe80::4553:9281%eth0
python3 tools/netbench.py tx --source internal --seconds 20   # also psram, camera, capture
python3 tools/netbench.py tx --source camera --seconds 20 --pgm /tmp/frame.pgm
python3 tools/netbench.py rx --rate 0 --seconds 60            # host to board, unpaced
python3 tools/netbench.py rx --rate 100 --size 64 --seconds 18  # beside a tx run
python3 tools/netbench.py done                                # the board logs "bench: host done"
```

`tools/netbench.py` (Python standard library only) binds every socket to the link-local address
of the interface, sends the request first, and prints the host's figures (rate, lost, duplicate,
reordered and bad datagrams, the host's UDP6 and interface counters) and the board's report.
The board logs every report too, after the run. `bench: ready` in the log means the service
listens.

### Choosing what runs

`SERVICE` at the top of `src/main.rs`: `Service::Stream` (default) runs the camera stream,
`Service::Bench` the network test service below, `Service::Network` only the network (ping,
neighbour discovery).

## What the firmware does

1. Switches UART0 to 2 Mbaud, prints the chip, enables PIE (the P4's SIMD extension,
   `esp32p4-pie`) on core 0, where the executor runs, and runs its copy and checksum-sum
   self-tests against the scalar references (4096 cases each, about 0.4 s; a failure panics),
   brings up PSRAM (250 MHz).
2. `net::start`: resets the PHY, creates the EMAC driver (which initialises the PHY over MDIO),
   switches the TX DMA to threshold mode, creates the embassy-net stack with the static
   address, spawns the runner (`net_task`) and the link task, and waits up to 8 s for the
   link. The link task wakes the runner every 500 ms so the PHY's link is read, and logs each
   change (speed and duplex read from the PHY).
3. The service: `stream::start` (below, [The camera stream](#the-camera-stream)) or
   `bench::start`, which spawns the two test tasks; the TX task fills its source buffers and
   finds the camera first, then logs `bench: ready`.

The test service (`src/bench.rs`, the module docs have the formats):

- **UDP 5010, board to host**: a 4-byte request `[kind, 0, seconds u16 LE]`; the board sends
  1452-byte datagrams to the requester as fast as the stack takes them, then one report
  datagram. Each datagram has a 16-byte header (sequence number, frame number, offset in the
  frame, frame length) and 1436 bytes of the frame. Kinds: `I` a 64 KiB buffer in internal RAM;
  `P` a 1 024 000-byte buffer in PSRAM, its cache lines written back and invalidated before
  each pass (cold, as after a DMA write); `F` camera frames, the newest complete one each time;
  `C` capture only (frames counted and dropped, no datagrams but the report); `D` nothing, the
  board logs `bench: host done`. In `I` and `P` every byte is a known pattern the host checks.
- **UDP 5011, host to board**: datagrams `[seq u32, stream u32, payload]` whose payload is a
  known pattern shifted per sequence number; the board checks every byte and counts datagrams,
  bad ones, gaps and late ones per stream, and answers a query (stream `0xFFFF_FFFF`) with its
  counts.

The reports carry the CPU time: `copy_cyc`, cycles per datagram in the application's copy into
the socket buffer (PIE's copy since 2026-11-11, [step 1](#measured-step-1-the-datagram-copy-through-pie-2026-11-11)),
and `copy_misaligned`, how many of those copies had source and destination unlike modulo 4
(the ROM `memcpy` copies them byte by byte, see
[copy alignment](#measured-baseline-and-copy-alignment-2026-11-11)); from the driver wrapper
(`net/device.rs`), per frame, the cycles in smoltcp's part of the TX and RX tokens
(`tx_stack_cyc`: headers, the payload copy into the DMA buffer and the UDP checksum) and in the
driver's part (`tx_driver_cyc`: descriptor, cache write-back, poll demand), and the token
requests (`token_cyc` each); from the executor's trace hooks (`src/cpu.rs`), the share of time
the executor was busy (`busy_pct`) and each task's cycles per datagram and polls; from the
interrupt-handler wrappers (`src/irq.rs`), the handlers' share (`irq pct`), the part of it that
fell outside executor passes (`outside_pct`, counted as idle before), the wake path
(`wake_pct`, `wakes`, `wake_cyc`: from the end of such a handler to the next executor pass:
esp-rtos's scheduler interrupt, the context switch, the executor's wake-up), each handler's
calls and mean own cycles (`eth`, `tick`, `csi_dma`, `csi_host`, `csi_bridge`, `jpeg`,
`dma2d_rx`, `dma2d_tx`), and `busy_all_pct`, the executor's busy share plus the handlers outside
it and the wake path; and the EMAC's DMA status, missed-frame counter and MAC debug register.

**Interrupt handlers.** The drivers bind their handlers (`esp32p4-csi`, `esp32p4-jpeg`,
esp-hal's EMAC handler, esp-rtos's timer tick); `irq::install` then puts a trampoline in the
vector table entry that times the handler it replaced and calls it (the source's mapping and
priority stay the driver's). A handler's own time leaves out the handlers that nested in it.
Not counted: the trap entry and exit around the handlers, and the time from the executor going
idle to the core's idle loop (the sleep path; measured only with `idle-counter`, below).

**The `accounting` feature** (on by default) builds the trace hooks, the handler wrappers and
the driver wrapper's statistics. `cargo build --release --no-default-features` leaves them out
(the driver wrapper then only sets the checksum capabilities); the reports then say
`accounting=off`. **The `idle-counter` feature** (off by default) replaces esp-rtos's idle hook
by a loop that counts idle cycles (steps of `mcycle` shorter than 256 cycles; it spins
instead of `wfi`): the reports get `idle_pct`, `busy_idle_pct` and
`busy_idle_cyc_per_datagram` (bench) or `core0_idle_pct` (stream), with `accounting` also the
sleep path (`sleep_pct`, `sleeps`, `sleep_cyc`; `core0_sleep_pct`). It measures builds with
and without `accounting` alike.

| File | What |
|---|---|
| `src/main.rs` | Boot: log baud, chip, PSRAM, the network, the service selected by `SERVICE`, the RAM report |
| `src/net/mod.rs` | The network module: pins and their sources, PHY reset, EMAC driver, TX threshold mode, the DMA storage in its linker section, embassy-net with the static IPv6 address, the runner and link tasks, EMAC register snapshot |
| `src/net/ip101.rs` | IP101 PHY driver (`Phy` trait): link polled every 500 ms, speed and duplex from ANAR and ANLPAR, the last report for the log |
| `src/net/device.rs` | `embassy-net-driver` wrapper around esp-hal's driver: per-frame cycle counts (feature `accounting`), and the checksum capabilities (UDP and ICMPv6 computed by smoltcp) |
| `src/cpu.rs` | Cycle counter, per-task CPU time and polls from the executor's `trace` hooks (feature `accounting`), the idle counter (feature `idle-counter`) |
| `src/irq.rs` | Interrupt-handler wrappers: calls and own cycles per handler and core, the time outside executor passes, the wake and sleep paths (feature `accounting`) |
| `src/camera.rs` | OV9281 and CSI receiver: set-up, any mode's start (lanes checked in their stop state before stream-on), controls, stop, the newest frame, the fault switch of the stream's error-path test |
| `src/stream/mod.rs` | The camera stream service: the shared stream state, the control task (requests, all-or-nothing `SetCamera`, repeats), the hand-off to the capture, encode and send tasks |
| `src/stream/check.rs` | The `SetCamera` checks in the protocol's order, as the viewer's fake camera makes them |
| `src/stream/capture.rs` | The capture task: frames, counters, the stream rules, mode changes and recovery |
| `src/stream/jpeg.rs` | The encode task: the JPEG pipeline, its output buffers, encoder errors, the automatic quality |
| `src/stream/send.rs` | The send task: answers and frame chunks, encoded straight into the socket's ring (`espion_proto` writes the chunk header, PIE copies the bytes) |
| `src/stream/diag.rs` | The stream's test hooks on UDP 5010: CPU, interrupt-handler, send-path and encoder reports, fault injection, stall, log marker, too-small encoder outputs, encode-only |
| `tools/streamdiag.py` | Host side of the stream's test hooks |
| `src/bench.rs` | The test service: TX runs from internal RAM, PSRAM or the camera, the RX check, the reports |
| `dram_dma.x` | Linker section for the Ethernet DMA storage at 0x4FF40000 or above (see below) |
| `tools/netbench.py` | Host side of the test service |

## The camera stream

`src/stream/`: the protocol of `espion_proto::cam` (`docs/protocol.md`, "Camera stream",
version 3) on UDP 5009, behaving as the viewer's fake camera (`host/poc-viewer/src/fake.rs`,
the reference) does, with the real sensor.

- **Requests.** `GetState` and `Poll` answer the full state; `SetStream` checks the encoding
  against the running mode (`Encoding::check`: RAW10 only from a RAW10 mode; JPEG only from a
  RAW8 mode whose width and height are multiples of 8, refused as `Jpeg { reason, format,
  width, height }` with the mode's values) and the JPEG quality (1..=100, `Quality { 1, 100 }`),
  and sets the quality in use to the set quality; `SetCamera` checks, in the protocol's order,
  the `ov9281` builder (size, alignment, window, rate), the receiver's geometry (`esp32p4-csi`'s
  rules), the stream's encoding against the new mode (the same `Encoding::check`), exposure
  (1..=the mode's longest) and gain (16..=248), and applies nothing unless all pass; the answer
  is the state or the error with its values. Start-up settings are the fake's: 1280x800 RAW8
  crop, 120 Hz +- 0.5, exposure 200 lines, gain 2x, stream on, JPEG at quality 90 with the
  automatic quality, no limit. Every valid request makes its source the destination and
  restarts the 3 s stream timeout; anything else on the port is counted and dropped. A repeat
  of the last request (same number, op and source, which the host sends while it waits) gets
  that request's result and is not executed again.
- **Mode changes.** A request whose mode differs from the running one (`ov9281::Mode`
  equality): the frame in flight is abandoned, the sensor stream turned off, the frame let out,
  the receiver stopped, the mode and controls written (`Ov9281::configure`: software reset,
  registers, standby), the receiver configured and started, the lanes checked in their stop
  state, stream on, the first frame awaited (two frame periods + 500 ms). The generation is
  bumped when format, size, readout or origin changed, and on an encoding change. Exposure,
  gain and test pattern alone are written while streaming. A start that fails (a sensor bus
  error, the receiver, lanes not in stop state, no first frame) answers `SensorBus` or
  `Receiver`, and the previous mode is started again, so the next request finds a running
  camera; if that fails too, the capture task retries every second and on every request.
- **Frames.** Four whole-frame buffers of 1 286 144 B (1280x800 RAW10) at the start of PSRAM,
  the newest complete frame taken. Frame numbers count captured frames from boot; each one is
  counted, and goes out only while a host polls and the stream is on, `max_fps` honoured
  (frames held back by it are not counted), the chunks in order on the 1409-byte grid to the
  last request's source. Raw: one in flight (a frame completing meanwhile counts as skipped,
  as do frames the receiver overwrote before the task took them); RAW10 goes out packed, or as
  RAW8 (the first 4 bytes of every 5, taken as each chunk is copied). JPEG: through the encode
  task ([JPEG](#jpeg)). Capture errors count short frames, the receiver's DMA, CSI host and
  bridge error interrupts, frames the receiver dropped for lack of a buffer, and frames that
  did not come within two periods (the camera is then restarted).
- **Where it runs.** All on core 0, in the one embassy executor, with the network: control,
  capture, encode, send, diag, the runner and the link task; the EMAC, CSI, JPEG and 2D-DMA
  interrupts are on core 0 too. The send task encodes each chunk straight into the socket's TX
  ring (`send_to_with`): `FrameChunk::encode_header` writes the chunk's header and PIE copies
  the bytes (`Pie::copy`, any alignment), the one CPU copy of the pixels or of the JPEG file
  (RAW10 sent as RAW8 is reduced to its high bits in that place instead); smoltcp copies the
  datagram into the DMA buffer with its checksum pass. Core 1 stays off (below: the two-core
  attempt hung).
- **RAM** (boot log): 182 272 B of internal RAM static, of which the service's sockets and
  buffers are 22 740 B (camera socket: 8 x 1452 B TX ring, 4 KiB RX ring; the hooks' socket);
  399 296 B left for the main stack. PSRAM: the four frame buffers, 5 144 576 B, then the
  encoder's three output buffers, 6 291 456 B.

### JPEG

The stream's default encoding, `Encoding::Jpeg`: one baseline greyscale JPEG file per frame,
made by the P4's hardware encoder (`esp32p4-jpeg`) straight from the receiver's PSRAM buffer of
a RAW8 frame; no CPU pass over the pixels. `src/stream/jpeg.rs`.

**Pipeline**, all on core 0, one frame in work and one waiting at each stage, never a queue:

1. The capture task offers every RAW8 frame it would send to the encode task: the frame waits
   in a one-frame slot. A newer frame arriving before the encoder took it replaces it, and the
   replaced one counts as a frame the encoder could not take (`encoder.frames_skipped`; frames
   the receiver overwrote before the capture task took them count there too). At most two
   captured frames are held (the one being encoded, the one waiting), which the receiver's
   four buffers allow (0 receiver drops in every run).
2. The encode task sets the encoder to the frame's size and the quality in use (only when they
   change: `Encoder::set_config`, header and tables), takes a free output buffer, encodes
   (`Encoder::encode` in a 100 ms timeout) and gives the receiver's buffer back at once.
3. The file waits for the send task in the next one-frame slot (`JOB`). A newer file finished
   before the send task took it replaces it, and the replaced one counts as skipped because
   the link was busy (`counters.frames_skipped`). So the link never waits for an encode, and
   the send task always gets the newest file. The send task sends it like a raw frame: the
   chunks on the 1409-byte grid, `frame_len` the file's length, each chunk copied once from
   PSRAM into the socket's ring.

**Buffers.** Three output buffers (one being sent, one waiting, one being encoded into) of
`JPEG_FRAME_MAX` = 2 MiB each in PSRAM after the receiver's buffers, 128-byte aligned (the
driver's rule for PSRAM). The encoder gets the part `Config::max_output_len` gives for the
size (the header and 2 bytes per pixel, 2 048 384 B at 1280x800), so any file the protocol
allows fits. A buffer is freed when its file is sent, replaced or abandoned (an `Out` handle,
as the receiver's `Frame`).

**Errors.** A file that does not fit (`OutputTooSmall`), a codec or DMA error, or no end
within 100 ms costs that frame: it counts in `encoder.errors`, the driver resets the hardware
before the next frame, the stream goes on. The control task never waits for the encoder. A
mode change aborts the stream epoch: frames and files of the old epoch are dropped where they
are, and the next frame reconfigures the encoder.

**Automatic quality** (`espion_proto::cam::auto_quality`, the same rule in the fake camera).
Every `AUTO_QUALITY_PERIOD_MS` = 200 ms, while JPEG frames go out with `auto_quality` on, the
encode task takes:

- `skipped`: a file was replaced in the send slot during the period (the link was busy);
- `busy_pct`: the share of the period the send task spent on frames, from a frame's first
  chunk to its last chunk queued (`SEND_BUSY_US`).

and sets the quality for the next period: above the set quality, the set quality; if
`skipped`, down by max(2, q / 8), not below `AUTO_QUALITY_MIN` = 10; else up by 1 towards the
set quality only if `busy_pct * S(q) < AUTO_QUALITY_ROOM_PCT * S(q + 1)` with
`AUTO_QUALITY_ROOM_PCT` = 80 and `S` the IJG scale (`ijg_scale`: 5000 / q below 50, 200 - 2q
up to 99, 1 at 100), that is if the step's expected growth still fits under 80 % (at q75 below
77 % busy, q90 below 72 %, q97 below 53 %); else unchanged. Without the "room" condition the
quality would climb until the link saturates, drop and climb again, a sawtooth with skips
every few periods; with a plain "+1 below 80 %" it still jumped off the cliff near q95-100,
where one step grows the file by 30-100 % (seen on the board: 98 → skip → 86 → 76 → ...). The
send task's busy time ends when the last chunk is
queued, about 24 datagrams (the socket's 8 and the EMAC's 16 descriptors) before it is on the
wire, so 80 % of send-task time is about 90 % of the link: at 1280x800 153.98 Hz it settles
at 83-88 Mbit/s of JPEG payload. A `SetStream` sets the quality in use back to the set
quality. With `auto_quality` off the set quality is used and a busy link costs frames.

On the board (1280x800 at 153.98 Hz, longest exposure, set quality 90; `auto1`, `auto2` in
`/tmp/poc-p4-eth/stream-jpeg/`):

| Phase | Quality in use, once a second | Host fps | Bytes/frame | Link skips |
|---|---|---|---|---|
| gain 15.5x, auto on, from q90 | 73, 74, 74, ... 74, 75 (t = 11 s), ... 76 (t = 18 s) | 153.82 over 20 s | 69 274 | 5 (first second) |
| then gain 2x (controls only), auto on | 81, 86, 90, 90, ... | 154.01 over 12 s | 14 670 | 0 |
| gain 15.5x, auto off, q90 | 90 | 69.94 | 165 378 | 504 in 7.2 s |

It drops in the first second, then creeps up by 1 now and then while the send task is below
80 % busy (once in 7 to 8 s here), and comes back to the set quality within 3 s when the
frames get small again.

#### Measured (JPEG)

All on 2026-11-11, this board, the link to the host's ASIX AX88179 at 100 Mbit/s full duplex,
the room dark (night). A host-side receiver for the bring-up (`jrx`, scratch, in
`/tmp/poc-p4-eth/stream-jpeg/rx/`, with the test hooks' CPU report) set each case and measured
6 s after 2 s of warm-up (`table1/`); fixed quality (auto off), longest exposure of the mode,
gain 2x (mean pixel 4) and 15.5x (mean pixel 8 to 23, noise makes the files 3 to 11 times
larger). Every case: every frame a valid JPEG of the mode's size (SOI, SOF0 size, EOI), 0
incomplete frames at the host, 0 frames the encoder could not take, 0 encoder errors, 0
capture errors, 0 receiver drops. Encode µs is the mean over the window as the encode task
sees it (from the start to its wake-up after the end-of-frame interrupt); encoder busy is its
share of the window.

| Window, sensor rate | Gain | q | Host fps | Mbit/s | Bytes/frame | Encode µs | Encoder busy % | Link skips | Core 0 % (net / send / encode / capture) | RTT ms | Limit |
|---|---|---|---|---|---|---|---|---|---|---|---|
| crop 1280x800, 120.0 Hz | 2x | 90 | 120.09 | 15.91 | 16563 | 5989 | 71.9 | 0 | 16.9 (9.8 / 6.4 / 0.3 / 0.1) | 1.37 | sensor |
| crop 1280x800, 120.0 Hz | 2x | 75 | 120.07 | 12.40 | 12906 | 5988 | 71.9 | 0 | 13.6 (8.2 / 4.8 / 0.3 / 0.1) | 1.37 | sensor |
| crop 1280x800, 120.0 Hz | 15.5x | 90 | 102.57 | 92.67 | 112926 | 6238 | 74.9 | 105 | 99.0 (54.5 / 39.6 / 1.8 / 1.9) | 4.91 | link |
| crop 1280x800, 120.0 Hz | 15.5x | 75 | 120.08 | 33.80 | 35187 | 5991 | 72.0 | 0 | 35.3 (20.6 / 13.9 / 0.3 / 0.2) | 2.09 | sensor |
| crop 1280x800, 128.3 Hz | 2x | 90 | 128.37 | 16.46 | 16032 | 5989 | 76.9 | 0 | 17.5 (10.2 / 6.5 / 0.3 / 0.2) | 1.38 | sensor |
| crop 1280x800, 128.3 Hz | 2x | 75 | 128.35 | 13.10 | 12758 | 5988 | 76.9 | 0 | 14.4 (8.7 / 5.0 / 0.3 / 0.1) | 1.38 | sensor |
| crop 1280x800, 128.3 Hz | 15.5x | 90 | 106.07 | 92.61 | 109138 | 6236 | 80.1 | 133 | 99.1 (54.7 / 40.1 / 1.6 / 1.5) | 4.83 | link |
| crop 1280x800, 128.3 Hz | 15.5x | 75 | 128.36 | 35.75 | 34812 | 5996 | 77.0 | 0 | 37.8 (21.8 / 15.1 / 0.3 / 0.2) | 2.91 | sensor |
| crop 1280x800, 153.98 Hz | 2x | 90 | 154.03 | 18.50 | 15010 | 5992 | 92.3 | 0 | 20.1 (12.0 / 7.2 / 0.4 / 0.2) | 1.43 | sensor |
| crop 1280x800, 153.98 Hz | 2x | 75 | 154.03 | 15.57 | 12639 | 5990 | 92.3 | 0 | 17.1 (9.5 / 6.7 / 0.3 / 0.2) | 1.38 | sensor |
| crop 1280x800, 153.98 Hz | 15.5x | 90 | 70.28 | 92.62 | 164723 | 6354 | 97.9 | 503 | 99.5 (56.3 / 40.0 / 1.2 / 1.0) | 5.10 | link |
| crop 1280x800, 153.98 Hz | 15.5x | 75 | 153.95 | 83.13 | 67493 | 6035 | 92.9 | 0 | 86.7 (47.7 / 37.4 / 0.6 / 0.3) | 3.51 | sensor |
| crop 1280x720, 141.5 Hz | 2x | 90 | 141.59 | 14.82 | 13084 | 5392 | 76.4 | 0 | 16.3 (9.0 / 6.4 / 0.3 / 0.2) | 1.36 | sensor |
| crop 1280x720, 141.5 Hz | 2x | 75 | 141.58 | 12.85 | 11345 | 5392 | 76.3 | 0 | 14.3 (7.9 / 5.6 / 0.3 / 0.2) | 1.37 | sensor |
| crop 1280x720, 141.5 Hz | 15.5x | 90 | 136.37 | 92.61 | 84884 | 5657 | 80.1 | 32 | 99.3 (57.0 / 38.9 / 1.3 / 1.0) | 4.97 | link |
| crop 1280x720, 141.5 Hz | 15.5x | 75 | 141.63 | 34.97 | 30864 | 5397 | 76.5 | 0 | 36.3 (21.1 / 14.2 / 0.4 / 0.2) | 2.10 | sensor |
| crop 1280x720, 169.85 Hz | 2x | 90 | 169.94 | 18.22 | 13402 | 5394 | 91.7 | 0 | 20.1 (11.1 / 8.1 / 0.4 / 0.2) | 1.41 | sensor |
| crop 1280x720, 169.85 Hz | 2x | 75 | 169.89 | 15.40 | 11332 | 5393 | 91.6 | 0 | 17.2 (9.5 / 6.8 / 0.4 / 0.2) | 1.38 | sensor |
| crop 1280x720, 169.85 Hz | 15.5x | 90 | 79.42 | 92.62 | 145766 | 5703 | 96.9 | 543 | 99.5 (56.8 / 39.7 / 1.1 / 0.9) | 5.02 | link |
| crop 1280x720, 169.85 Hz | 15.5x | 75 | 170.15 | 82.59 | 60674 | 5430 | 92.3 | 0 | 86.0 (47.4 / 37.0 / 0.6 / 0.4) | 3.25 | sensor |
| crop 640x480, 204.9 Hz | 2x | 90 | 205.04 | 7.16 | 4362 | 1815 | 37.2 | 0 | 8.6 (4.4 / 3.2 / 0.5 / 0.3) | 1.36 | sensor |
| crop 640x480, 204.9 Hz | 2x | 75 | 205.03 | 6.57 | 4003 | 1815 | 37.2 | 0 | 7.4 (3.5 / 2.8 / 0.5 / 0.3) | 1.36 | sensor |
| crop 640x480, 204.9 Hz | 15.5x | 90 | 205.07 | 78.11 | 47613 | 1821 | 37.4 | 0 | 79.8 (45.9 / 32.3 / 0.6 / 0.4) | 3.20 | sensor |
| crop 640x480, 204.9 Hz | 15.5x | 75 | 205.06 | 33.87 | 20647 | 1817 | 37.3 | 0 | 35.1 (21.1 / 12.9 / 0.5 / 0.3) | 1.38 | sensor |
| crop 640x480, 245.85 Hz | 2x | 90 | 245.92 | 9.04 | 4592 | 1815 | 44.7 | 0 | 10.7 (5.5 / 4.0 / 0.6 / 0.3) | 1.36 | sensor |
| crop 640x480, 245.85 Hz | 2x | 75 | 245.91 | 8.03 | 4079 | 1815 | 44.6 | 0 | 9.0 (4.3 / 3.5 / 0.5 / 0.3) | 1.36 | sensor |
| crop 640x480, 245.85 Hz | 15.5x | 90 | 234.82 | 92.57 | 49277 | 1974 | 48.6 | 66 | 98.9 (59.1 / 37.2 / 0.8 / 0.6) | 4.73 | link |
| crop 640x480, 245.85 Hz | 15.5x | 75 | 245.95 | 40.03 | 20345 | 1818 | 44.7 | 0 | 41.5 (24.9 / 15.3 / 0.6 / 0.3) | 1.47 | sensor |
| binned 640x400, 251.8 Hz | 2x | 90 | 251.89 | 9.38 | 4656 | 1517 | 38.2 | 0 | 11.0 (5.6 / 4.1 / 0.6 / 0.3) | 1.36 | sensor |
| binned 640x400, 251.8 Hz | 2x | 75 | 251.88 | 7.18 | 3563 | 1517 | 38.2 | 0 | 8.4 (4.0 / 3.2 / 0.6 / 0.3) | 1.36 | sensor |
| binned 640x400, 251.8 Hz | 15.5x | 90 | 251.88 | 50.52 | 25071 | 1522 | 38.4 | 0 | 52.3 (31.1 / 19.7 / 0.7 / 0.3) | 1.68 | sensor |
| binned 640x400, 251.8 Hz | 15.5x | 75 | 251.90 | 18.21 | 9034 | 1518 | 38.2 | 0 | 19.7 (10.8 / 7.6 / 0.6 / 0.3) | 1.36 | sensor |
| binned 640x400, 302.2 Hz | 2x | 90 | 302.26 | 10.60 | 4384 | 1517 | 45.9 | 0 | 12.5 (6.3 / 4.7 / 0.7 / 0.4) | 1.36 | sensor |
| binned 640x400, 302.2 Hz | 2x | 75 | 302.26 | 8.57 | 3545 | 1517 | 45.9 | 0 | 10.0 (4.7 / 3.8 / 0.7 / 0.4) | 1.36 | sensor |
| binned 640x400, 302.2 Hz | 15.5x | 90 | 302.36 | 77.12 | 31882 | 1526 | 46.2 | 0 | 79.1 (46.0 / 31.3 / 0.9 / 0.4) | 2.74 | sensor |
| binned 640x400, 302.2 Hz | 15.5x | 75 | 302.30 | 28.11 | 11621 | 1519 | 45.9 | 0 | 30.7 (17.1 / 11.9 / 0.7 / 0.4) | 1.36 | sensor |
| binned 320x200, 464.9 Hz | 2x | 90 | 465.46 | 4.64 | 1245 | 400 | 18.6 | 0 | 6.9 (2.6 / 2.2 / 1.1 / 0.7) | 1.36 | sensor |
| binned 320x200, 464.9 Hz | 2x | 75 | 465.45 | 4.29 | 1152 | 400 | 18.6 | 0 | 6.7 (2.6 / 2.1 / 1.1 / 0.7) | 1.36 | sensor |
| binned 320x200, 464.9 Hz | 15.5x | 90 | 465.50 | 29.00 | 7786 | 400 | 18.7 | 0 | 31.5 (16.9 / 12.3 / 1.1 / 0.7) | 1.82 | sensor |
| binned 320x200, 464.9 Hz | 15.5x | 75 | 465.46 | 11.47 | 3081 | 400 | 18.6 | 0 | 14.0 (6.6 / 5.2 / 1.1 / 0.7) | 1.36 | sensor |
| binned 320x200, 557.9 Hz | 2x | 90 | 558.00 | 5.54 | 1241 | 400 | 22.4 | 0 | 8.3 (3.2 / 2.6 / 1.3 / 0.8) | 1.36 | sensor |
| binned 320x200, 557.9 Hz | 2x | 75 | 558.00 | 5.10 | 1142 | 400 | 22.4 | 0 | 8.0 (3.1 / 2.5 / 1.3 / 0.8) | 1.38 | sensor |
| binned 320x200, 557.9 Hz | 15.5x | 90 | 558.00 | 35.34 | 7917 | 401 | 22.4 | 0 | 38.3 (20.5 / 15.0 / 1.3 / 0.8) | 1.41 | sensor |
| binned 320x200, 557.9 Hz | 15.5x | 75 | 558.01 | 14.18 | 3177 | 400 | 22.4 | 0 | 17.1 (8.1 / 6.4 / 1.3 / 0.8) | 1.36 | sensor |

**What limits each window.** At gain 2x every window delivers every frame the sensor makes, up
to each window's maximum: 154.03, 169.94, 245.92, 302.26 and 558.00 fps for the predicted 154,
170, 246, 302 and 558; at 15.5x as well at quality 75. The cases that fall short are the
noisy scene at quality 90 in 1280x800 (all three rates), 1280x720 (both) and 640x480 at
245.85 Hz: the link, at 92.6 Mbit/s of JPEG payload (8 220 datagrams/s with their chunk
headers, the wire limit) with the send task always busy and link skips counted, and the CPU at
99 % (it is the same per-datagram cost as raw frames: about 27 000 cycles in the network task
and 19 000 in the send task per datagram). Nothing in this crate can make that faster; the
automatic quality keeps the frame rate there by lowering the quality (154 fps at quality
74-76 in the table above). The encoder is the next limit: 92.3 % busy at 1280x800 153.98 Hz,
and above about 300 kB per file its time grows (below).

**CPU, core 0.** With JPEG the CPU load follows the bytes sent, not the pixels: 17-20 % at
1280x800 at 2x gain (15-19 Mbit/s), against 94 % for raw frames at the wire rate. The encode
task costs 0.3-1.8 % (one set-up per frame: about 20 register writes, a 384-byte header copy,
three cache calls), the capture task 0.1-1.9 %.

**Encoder with Ethernet traffic.** The same scene and settings with the files sent and with
the test hooks' encode-only switch (`X1`: files made and dropped, the link idle; `bus.txt`,
`bus2.txt`):

| 1280x800 at 153.98 Hz | Bytes/frame | Encode µs, sent | Encode µs, not sent |
|---|---|---|---|
| gain 2x, q75 (14 Mbit/s) | 12 700-12 900 | 5 989, 5 989 | 5 987, 5 987 |
| gain 15.5x, q75 (83 Mbit/s, core 0 93.5 %) | 71 300-71 700 | 6 083, 6 047 | 5 987, 5 987 |
| gain 2x, q100 (link full, core 0 99.6 %) | 211 000-213 000 | 6 365, 6 378 | 6 008, 6 000 |

No encoder error in any of them. Not sent, the encoder takes 5 987-6 008 µs for files up to
214 kB, 6 090 µs at 288 kB (gain 15.5x, q95) and 6 713 µs at 542 kB (gain 15.5x, q100: longer
than the frame period, 21 frames it could not take in 4 s). While the link is busy the encode
takes up to 6 % longer;
[INFERENCE: mostly the encode task's wake-up waiting behind the network and send tasks' polls,
which last up to about 0.4-0.6 ms at the wire rate (8 datagrams of 19 000 cycles, 16
descriptors of 27 000), rather than the encoder itself; the driver has no hardware timestamp
to tell them apart]. At 1280x800 153.98 Hz the sensor gives 6 494 µs per frame, so at the
wire rate with files above 200 kB an occasional frame is not encoded (2 in one 4 s window).

**Too-small output** (the test hooks' `C<n>`: the next n encodes get a 4 KiB output; `small.txt`):
20 injected at 1280x800 153.98 Hz: 20 encoder errors (`output_too_small`), 20 frames not sent
(930 captured, 910 sent), 0 skipped, the stream at 154.0 fps again in the next second. The
error came 1 505 µs into the encode: when the output is full, so at most one encode time.

**Request round trip** (the receiver's polls, every 200 ms): 1.36-1.43 ms at 2x gain, 2.7-5.1
ms with the link full (the request waits behind the queued datagrams), no poll lost. Control
stays answered whatever the encoder does: the control task never waits for it.

**Picture** (`corr/`, frames at 1280x800 120 Hz, decoded with libjpeg-turbo's `djpeg`):

| Picture | Board JPEG q90 vs the RAW8 frame | libjpeg-turbo `cjpeg -grayscale -baseline -quality 90` of the same RAW8 frame vs it |
|---|---|---|
| bars test pattern (static; 0/255 runs, 520 120 B at q90) | PSNR 44.98 dB, max error 9 | PSNR 46.85 dB, max error 9 |
| the scene at 2x (16 079 B) | PSNR 53.11 dB, max error 7 (a frame taken after it) | PSNR 53.55 dB, max error 7 |
| the scene at 15.5x (111 598 B) | PSNR 39.15 dB, max error 21 (a frame taken after it: sensor noise included) | PSNR 42.68 dB, max error 11 |

Two RAW8 bars frames are identical, so the bars row is the codec's error alone: the board's
encoder is within libjpeg-turbo's (same largest error, 1.9 dB lower PSNR on this extreme
picture; on camera scenes the driver measured them equal within 0.02 dB).

#### Soak (JPEG)

2026-11-11, the final build flashed and its log captured for 720 s (`soak-board.log`: no panic,
exception, reset or warning). `soak.py` (in `/tmp/poc-p4-eth/stream-jpeg/`, outputs `soak1/`,
`soak1b/`) ran 35 steps of 18 s on that one boot, 636 s: every other step crop 1280x800 RAW8 at
153.98 Hz in JPEG q90 with automatic quality, and in between, in turn: the same at gain 15.5x;
RAW10 sent as RAW10 at 120 Hz; JPEG q75 fixed; binned 640x400 JPEG at 302.2 Hz; RAW8 sent raw;
binned 320x200 JPEG at 557.9 Hz; binned 640x400 RAW10 sent as RAW8; crop 1280x720 JPEG at
169.85 Hz; crop 640x480 JPEG with the bars pattern at 245.85 Hz. Each step is a new receiver
process (a viewer restart on a new port); every 4th step got a burst of 2000 malformed
datagrams, every 5th JPEG step 5 too-small encoder outputs (`C5`).

- Every step: frames from its first second, 0 incomplete and 0 invalid frames at the host,
  frame numbers increasing, 0 capture errors. JPEG steps at the sensor rate (154.0, 169.75,
  301.9-302.2, 557.4-557.9 fps; 1280x800 at gain 15.5x 153.9-154.1 fps at quality 74-75), the
  bars step link-bound at 229 fps at quality 10, raw steps at the wire rate (8.75-8.87, 10.98,
  39.9 fps). Request round trip 1.36-1.50 ms in JPEG, 4.0-4.9 ms with the link full.
- Board counters at the end (uptime 655.5 s): captured 126 975, sent 105 600, skipped (link
  busy) 14 537, encoder could not take 21, encoder errors 15, capture errors 0, receiver drops
  0, EMAC missed 0. Sent + skipped + encoder skips + encoder errors = 120 173 <= captured; the
  rest were captured while no receiver polled (between steps) or cut by a mode change.
- The 15 encoder errors are the injected ones (`output_too_small`, 5 each in three steps); the
  injections in the two 320x200 steps caused none, since a 320x200 file (about 2.6 kB) fits in
  4 KiB. The 21 frames the encoder could not take fell in steps that changed size or quality
  (2 to 9 each: the first encode after a reprogramming takes up to 6.4 ms, and a burst of
  malformed datagrams delays the encode task).
- 120 of the 10 000 malformed datagrams reached the stream's socket and were counted as bad
  requests; smoltcp dropped the rest when the socket's 4 KiB ring was full.

#### Live window on the desktop

2026-11-11, `poc-viewer --selftest 8` built in release (`5040b13`), the desktop's native
Wayland session at 3840x2160 119.88 Hz with vsync, JPEG q90 with automatic quality, longest
exposure of each mode, gain 8x (the room dark, night: at 2x the picture is nearly black), display gain 4
(`desktop.sh`, outputs and screenshots in `/tmp/poc-p4-eth/stream-jpeg/desktop/`). Measured over
the last 6 s.

| Mode (sensor rate) | Received fps (raw, previous step) | Decoded / presented fps | Mbit/s | Bytes/frame | Decode mean / max | UI frame time mean / p99 | Dropped / cut / replaced | Board link skips / encoder skips | Limit |
|---|---|---|---|---|---|---|---|---|---|
| crop 1280x800, 153.98 Hz | 153.9 (11.2) | 120.1 / 120.1 | 67.1 | 53 473 | 1.05 / 1.97 ms | 0.40 / 0.59 ms | 0 / 0 / 203 | 0 / 0 | sensor (received), display (presented) |
| crop 1280x720, 169.85 Hz | 169.9 (12.1) | 119.9 / 119.9 | 65.0 | 46 931 | 0.90 / 2.05 ms | 0.39 / 0.59 ms | 0 / 0 / 300 | 0 / 0 | sensor, display |
| crop 640x480, 245.85 Hz | 245.8 (36.6) | 119.7 / 119.7 | 28.6 | 14 281 | 0.33 / 0.68 ms | 0.32 / 0.49 ms | 0 / 0 / 756 | 0 / 0 | sensor, display |
| binned 640x400, 302.2 Hz | 302.2 (44.1) | 119.9 / 119.9 | 27.0 | 10 956 | 0.26 / 0.57 ms | 0.31 / 0.48 ms | 0 / 0 / 1 093 | 0 / 0 | sensor, display |
| binned 320x200, 557.9 Hz | 557.9 (181.1) | 119.7 / 119.5 | 11.9 | 2 613 | 0.08 / 0.17 ms | 0.30 / 0.46 ms | 0 / 0 / 2 629 | 0 / 0 | sensor, display |
| crop 1280x800, 120 Hz | 119.9 | 119.8 / 119.8 | 56.3 | 57 628 | 1.05 / 2.63 ms | 0.40 / 0.59 ms | 0 / 0 / 1 | 0 / 0 | sensor |

The quality stayed at 90 in every run (no link skip, 0 encoder errors, 0 capture errors). The
viewer decodes only what it can show, so decoded = presented = the display's refresh; the frames
received in between are replaced before being shown. The screenshots show the live scene with
continuous edges and no torn rows; at display gain 4 the dark background shows sensor noise and,
in the darkest areas of the 320x200 and 640x480 pictures, a faint 8x8 block pattern in that
noise; no block artefacts on the edges at quality 90.

### Measured: raw frames (protocol version 2)

All on 2026-11-10, this board, the link to the host's ASIX AX88179 at 100 Mbit/s full duplex,
`poc-viewer`'s board test (`host/poc-viewer/tests/board.rs`, run `full2`: 0 failed checks;
output and board log in `/tmp/poc-p4-eth/stream/full2.*`). Host rates are whole frames over the
window (2.5 to 4 s per mode); every mode below had 0 incomplete frames at the host, frame
numbers increasing, every frame of the right size, and 0 capture errors on the board.

| Mode (sensor rate) | Host fps | Mbit/s of payload | Limit |
|---|---|---|---|
| crop 1280x800 RAW8, 120.049 Hz | 11.00 | 89.9 | link |
| crop 1280x800 RAW8, 153.984 Hz (96 MHz) | 10.80 | 90.8 | link |
| crop 1280x720 RAW8, 139.920 Hz | 12.33 | 92.3 | link |
| crop 1024x768 RAW8, 60.004 Hz | 13.33 | 86.8 | link (every 4th or 5th frame) |
| crop 640x480 RAW8, 245.850 Hz (96 MHz) | 36.80 | 91.9 | link |
| crop 320x240 RAW8, 99.957 Hz | 100.00 | 62.3 | sensor |
| crop 1280x64 RAW8, 999.698 Hz | 141.67 | 94.1 | link |
| crop 128x2 RAW8 (smallest), 1000.663 Hz | 1000.67 | 2.2 | sensor |
| binned 640x400 RAW8, 302.189 Hz (96 MHz) | 43.20 | 90.0 | link |
| binned 320x200 RAW8, 557.874 Hz (96 MHz) | 180.80 | 94.1 | link |
| binned 128x2 RAW8 (smallest), 200.027 Hz | 200.00 | 0.4 | sensor |
| crop 1280x800 RAW10 sent as RAW10, 120.049 Hz | 8.67 | 91.8 | link |
| crop 1280x800 RAW10 sent as RAW8, 120.049 Hz | 9.67 | 78.1 | board CPU |
| binned 640x400 RAW10 sent as RAW10, 200.027 Hz | 33.33 | 86.9 | link |
| binned 640x400 RAW10 sent as RAW8, 200.027 Hz | 40.00 | 83.3 | board CPU |
| crop 1216x704 RAW10 at (32,48) sent as RAW8, 99.957 Hz | 11.00 | 77.2 | board CPU |
| any window at 2 Hz | 2.00 | | sensor |

The test also passed: 2 Hz and the fastest rate at 80 and 96 MHz for 5 windows (the clock the
driver chose checked), non-centred origins (crop at (0,0), (640,320), (32,48), binned at
(96,36)), every refusal with the driver's values (`TooFast { 153984 }`, `TooSlow { 419 }`,
`Tolerance { 120000 }`, `Size`, `Alignment` RAW8 and RAW10, `Window`, `Exposure { 889 }` above
and at 0, `Gain`, `Encoding` for RAW10 from a RAW8 sensor) with the state unchanged, exposure
and gain against the mean (crop 640x480 at 30 Hz: 400 lines at 2x 12.0, 1600 lines 37.3,
400 lines at 8x 37.2), every test pattern (bars: every even row equal to row 0 and every odd to
row 1, the same in two frames; rolling bars differ; squares and random repeat), RAW8 from
RAW10 equal to the high bits of the RAW10 frame byte for byte (bars, 1280x800), `max_fps` 5 and
30 (5.00 and 30.00 fps), stream off (0 chunks in 1 s) and on, the timeout (one request, the
last chunk 3003 ms after it, the stream back on the next request).

**Request round trip under load** (the viewer's own polls, 40 in 10 s each, while frames go
out): crop 1280x800 RAW8 mean 4.85 ms (median 4.77, max 5.72); RAW10 sent as RAW10 4.78 ms
(max 5.40); RAW10 sent as RAW8 5.45 ms (max 6.08); binned 320x200 at 465 Hz 4.25 ms (max 5.66);
no retry. A request waits behind the datagrams already queued (the socket's 8 and the 16 DMA
descriptors, about 2.9 ms of wire time) [INFERENCE: what the 4-5 ms is made of].

**CPU load at the wire rate** (core 0, 400 MHz, 10 s windows from the test hooks; core 1 is
off):

| Stream | Core 0 busy | net task | send task | capture | control | Cycles per datagram (net + send) |
|---|---|---|---|---|---|---|
| crop 1280x800 RAW8 | 93.7 % | 55.9 % | 35.7 % | 0.9 % | 0.1 % | 28 187 + 18 022 |
| crop 1280x800 RAW10 as RAW10 | 95.4 % | 56.5 % | 36.8 % | 0.7 % | 0.1 % | 28 156 + 18 347 |
| crop 1280x800 RAW10 as RAW8 | 97.1 % | 50.5 % | 44.3 % | 1.7 % | 0.2 % | 28 666 + 25 126 |
| binned 320x200 RAW8 at 465 Hz | 91.3 % | 54.8 % | 35.2 % | 0.7 % | 0.1 % | 27 006 + 17 343 |

EMAC missed frames 0, receiver DMA, host and bridge errors 0 in every window. The send task's
18 000 cycles per datagram are the chunk encoding (postcard header and the copy of 1409 bytes
from PSRAM) and embassy-net's socket calls; the net task's 28 000 are smoltcp (headers, copy
into the DMA buffer, UDP checksum: 16 000-21 000) and the driver. RAW10 as RAW8 adds the
4-of-5 selection to the copy (7 000 cycles more per datagram) and makes the CPU the limit, at
about 90 % of the link rate.

**Error paths** (the test hooks' fault injection): a mode change whose sensor writes all fail
(`F1`) was answered `SensorBus` after 85 ms and one whose stream never starts (`F2`)
`Receiver` after 667 ms; both left the previous mode streaming at its full rate, and the next
change succeeded. A sensor stall (`S`: stream off behind the receiver's back) was noticed,
counted as one capture error, and the camera restarted: frames again 610 ms after it. 2000
malformed datagrams sent back to back (wrong magic, wrong version, truncated, a bad body): the
socket took 15 of them (the rest overflowed its 4 KiB ring and were dropped by smoltcp), all
counted as bad, and the stream went on at 92.0 Mbit/s.

### The two-core attempt

Moving the camera to core 1 (the capture task in an esp-rtos thread-mode executor started
with `esp_rtos::start_second_core`, 16 KiB stack; `Csi::new` and the I2C bus created there so
their interrupts went to core 1; `src/cpu.rs` given per-core accounts first) hung the board:
at the link rate (crop 1280x800 RAW8) core 0 stopped answering ping within 2 s, with nothing in
the log (no panic, no exception, no reset); at low rates (crop 640x480 at 30 Hz, the pattern
tests) it ran for several seconds (logs `core1a`, `core1b` and the ping trace `core1b.ping`
in `/tmp/poc-p4-eth/stream/`). Ruled out: the single-executor CPU accounts (per core before the
run) and the RAW10 path (it hung in RAW8 too). Not ruled out [INFERENCE]: a lock-order
inversion between esp-hal's global critical section and esp-rtos's scheduler lock (two
spinlocks): every embassy-sync signal and channel, and the CSI driver's DMA interrupt, wake a
task on the other core from inside a critical section, and a wake takes the scheduler lock;
or core 1's stack. Single core meets the requirements (wire rate, requests answered in about
5 ms, no capture error), so the stream runs on core 0 alone.

## Measured results

All on 2026-11-10, this board, link to the host's USB adapter (ASIX AX88179) at 100 Mbit/s
full duplex, the final build (logs in the run named `final`: board log and host output).
MB = 10^6 bytes. The wire limit for 1452-byte UDP payloads over IPv6 is 8127 datagrams/s
(1538 bytes on the wire each: + 8 UDP, 40 IPv6, 14 Ethernet, 4 FCS, 8 preamble, 12 gap) =
11.80 MB/s = 94.4 Mbit/s of UDP payload. Every run sends whole frames, whose last datagram is
shorter, so the datagram rate can exceed 8127/s at the same 100 Mbit/s.

### Boot and ping

```
net: EMAC DMA operation mode 0x00202042 -> 0x00002042 (TX store-and-forward true -> false, threshold code 0)
net: EMAC up, MAC [80, f1, b2, da, e4, 67], PHY at MDIO 1 id 0x0243 0x0c54, DMA 16 RX + 16 TX descriptors (51200 B)
net: stack up, address fe80::4553:9281/64, 4 sockets (1216 B of stack resources)
net: link up, 100 Mbit/s full duplex (BMSR 0x786d, ANAR 0x01e1, ANLPAR 0xcde1)
```

`ping -6 -c 20 fe80::4553:9281%eth0`: 20 sent, 20 received, rtt min/avg/max 1.303/1.470/4.141 ms;
3 of 3 at each of `-s 56, 400, 1000, 1452`. The host's neighbour table holds the board's MAC
(`REACHABLE`).

### Board to host, 20 s each

| Source | Datagrams | Host: MB/s (Mbit/s) of UDP payload | Datagrams/s | Lost, dup., reordered, bad | Board CPU busy |
|---|---|---|---|---|---|
| Internal RAM (64 KiB, hot) | 163 778 | 11.796 (94.37) | 8188 | 0, 0, 0, 0 | 88.7 % |
| PSRAM (1 024 000 B, cold per pass) | 162 773 | 11.800 (94.40) | 8137 | 0, 0, 0, 0 | 90.8 % |
| Camera frames (PSRAM, newest frame) | 162 705 | 11.801 (94.41) | 8137 | 0, 0, 0, 0 | 91.9 % |

CPU cycles per datagram (400 MHz, one core; from the reports):

| Source | App copy into the socket (`copy_cyc`) | App task total | Runner total | of it: smoltcp TX (`tx_stack`) | driver TX (`tx_driver`) | token requests (4 per datagram) |
|---|---|---|---|---|---|---|
| Internal | 13 044 | 15 446 | 27 127 | 10 169 | 2 090 | 4.8 x 1 160 |
| PSRAM | 16 251 | 18 604 | 25 442 | 10 218 | 2 095 | 4.0 x 1 329 |
| Camera | 16 706 | 19 128 | 25 458 | 10 242 | 2 078 | 4.0 x 1 231 |

`tx_stack` is smoltcp writing the headers, copying the payload from the socket buffer into the
DMA buffer and computing the UDP checksum; `tx_driver` the descriptor hand-over, the cache
write-back of the buffer and the poll demand; a token request checks the RX and TX descriptors
(one cache invalidation each). The rest of the runner (about 8 000 cycles) is smoltcp's poll
loop. The PSRAM source costs 3.95 ms of cache write-back and invalidation over 227 passes.

**What limits the rate: the wire.** Every source reaches 100 Mbit/s on the wire with no loss,
and the executor is idle 8–11 % of the time, so the CPU is not the limit; the app task waits
for space in the socket buffer, which drains at the wire's pace. The CPU margin is thin,
though: about 45 000 cycles per datagram on one core. (2026-11-11: the executor's idle time
held interrupt handlers and the wake and sleep paths; the idle counter finds the core 1.4 to
4.8 % idle in these runs, 46 500 to 48 500 cycles per datagram: see
[CPU accounting](#measured-cpu-accounting-with-and-without-the-accounting-feature-2026-11-11).)
The copies were not the limit, so none
was removed; a word-by-word copy measured the same as the ROM `memcpy` esp-rom-sys links
(13 080 against 13 043 cycles in the same test; dropped). `GenericPhy` instead of the IP101 driver (same build
otherwise, internal RAM, 10 s): the same 11.795 MB/s, but the executor busy 99.8 % (never idle)
and the runner at 33 707 cycles per datagram against 27 056 with the IP101 driver.

### Host to board

Every byte of every datagram checked on the board (`bad_bytes`), counts from the board's reply:

| Run | Sent by the host | Received | Bad datagrams / bytes | Lost | Late | EMAC missed frames |
|---|---|---|---|---|---|---|
| 1452 B at 1000/s, 20 s | 20 000 | 20 000 | 0 / 0 | 0 | 0 | 0 |
| 1452 B at 4000/s, 20 s | 80 000 | 80 000 | 0 / 0 | 0 | 0 | 0 |
| 1452 B unpaced, 60 s (8129/s, 11.803 MB/s: the wire limit) | 487 728 | 487 728 | 0 / 0 | 0 | 0 | 0 |
| 64 B at 100/s, 18 s, while the board sends PSRAM frames at full rate | 1 800 | 1 800 | 0 / 0 | 0 | 0 | 0 |

The TX run beside the 64-byte stream: 162 772 datagrams, 11.800 MB/s at the host, none lost
or bad, and the executor 92.0 % busy; the RX task took 0.6 % of the CPU (317 cycles per sent
datagram, 1801 polls).

### Camera

1280x800 RAW8 at 120.049 Hz requested from the `ov9281` crate (HTS 728, VTS 914), three
1 028 096-byte buffers in PSRAM, 20 s each:

| Run | Capture | Sent | Host |
|---|---|---|---|
| Capture only (`C`) | 2402 frames, 120.0 fps, dropped 0, overwritten 0, DMA/host/bridge errors 0 | — | — |
| PSRAM source alone (`P`) | — | 11.802 MB/s | 11.800 MB/s, 0 lost or bad |
| Capture and send (`F`) | 2401 frames, 120.0 fps, dropped 0, overwritten 2172 (expected: the sender takes about one frame in ten), DMA/host/bridge errors 0, every frame 1 024 000 B | 11.797 MB/s, 227 whole frames | 11.801 MB/s, 0 lost, 227 whole frames = 11.35 fps |

Capture and sending do not disturb each other: the capture keeps 120.0 fps with no error
while the board sends at the wire limit, and the send rate is the PSRAM-alone rate. A whole
frame takes 714 datagrams, 87.9 ms of wire time, hence 11.35 fps. The host wrote the last whole
frame (number 2381) as a PGM; it shows the bench scene (ceiling and lamp) with straight,
continuous edges: no torn or shifted rows.

### Measured: baseline and copy alignment, 2026-11-11

The baseline of the stream phase of the performance plan: the committed tree (`b47fe4c`) built
with `SERVICE = Service::Bench`, this board, the link to the host's ASIX AX88179 (`eth0`) at
100 Mbit/s full duplex (ping 5 of 5, 1.36/1.91/3.99 ms). `tools/netbench.py tx`, 20 s per
source; logs in `/tmp/p4perf-logs/StreamBase/` (`base.*`). Cycles per datagram:

| Source | Datagrams | Host MB/s (datagrams/s) | Lost, dup., reordered, bad | CPU busy | `copy_cyc` | TX task | Runner | `tx_stack_cyc` | `tx_driver_cyc` | Token requests per datagram x `token_cyc` |
|---|---|---|---|---|---|---|---|---|---|---|
| Internal | 163 778 | 11.796 (8188) | 0, 0, 0, 0 | 89.5 % | 13 076 | 15 521 | 27 302 | 10 165 | 2 063 | 4.84 x 1 193 |
| PSRAM | 162 773 | 11.800 (8137) | 0, 0, 0, 0 | 91.4 % | 16 276 | 18 630 | 25 604 | 10 276 | 2 059 | 3.96 x 1 275 |
| Camera | 162 706 | 11.801 (8137) | 0, 0, 0, 0 | 92.6 % | 16 738 | 19 167 | 25 629 | 10 302 | 2 071 | 3.94 x 1 194 |

The camera ran at 120.0 fps with 0 dropped frames; the host got 227 whole frames. Against the
2026-11-10 figures ([Board to host](#board-to-host-20-s-each)): the same host rates and
datagram counts, the busy share 0.6 to 0.8 points higher, the cycle counts within 1 % and
`token_cyc` within 4 %: inside the 2 to 3 % by which two builds of one source differ.

**Copy alignment.** A temporary build (not kept) recorded for every datagram the source and
destination address modulo 4 and the length of the application's copy into the socket ring
(the bench's closure; `FrameChunk::encode` in `stream/send.rs`) with its cycles, and for
smoltcp's copy from the ring into the EMAC buffer the ring slot's address modulo 4 (taken in
the application's closure) paired, in sending order, with the payload's address modulo 4 in
the EMAC buffer and the frame's `tx_stack` cycles (taken in `net/device.rs`). The bench ran 20 s
per source (`align-bench.*`), the stream 10 s windows with the scratch receiver `jrx`
(`align2-stream.case1` to `case3`). Full-size datagrams only; the stream's copy cycles include
the chunk's postcard header.

| Run | App copy, source and destination alike mod 4: share, cycles | App copy, unlike | smoltcp copy, slot and EMAC payload alike: share, `tx_stack` | smoltcp copy, unlike |
|---|---|---|---|---|
| Bench, internal RAM (1436 B) | 0 % | 100 % (source 0, destination 2), 13 183 | 100 % (both 2), 10 233 | 0 % |
| Bench, PSRAM | 0 % | 100 %, 16 302 | 100 %, 10 266 | 0 % |
| Bench, camera frames | 0 % | 100 %, 16 760 | 100 %, 10 269 | 0 % |
| Stream, raw 1280x800 RAW8, 120 Hz, link-bound (1409 B chunks) | 27.1 %, 7 303 | 72.9 %, 17 907 | 23.7 %, 10 305 | 76.3 %, 21 019 |
| Stream, JPEG q90, gain 15.5x, 153.98 Hz, link-bound | 24.3 %, 9 277 | 75.7 %, 19 401 | 28.7 %, 10 353 | 71.3 %, 21 145 |
| Stream, JPEG q90, gain 2x, 120 Hz (14 kB files, 13.6 Mbit/s) | 0 % | 100 %, 17 336 | 39.6 %, 10 070 | 60.4 %, 21 266 |

- **The application's copy: the inference holds.** In the bench the socket's TX ring
  (`TX_TX_BUF`, a byte array, alignment 1) starts at an address that is 2 modulo 4, and every
  datagram is 1452 bytes, so every slot is too; the 16-byte header puts the payload at 2
  modulo 4, while the source (a 64-byte aligned buffer read in 1436-byte steps) is at 0. Every
  bench copy takes the ROM `memcpy`'s byte loop. In the stream a copy whose ends are alike
  costs 10 100 to 10 600 cycles less than one whose ends are not, and three quarters of the
  copies are unlike: the chunk offsets step by 1409 (1 modulo 4), the header length varies, and
  the slots move by datagram lengths that are not multiples of 4. With small, regular JPEG files
  the pattern repeats from frame to frame: at gain 2x every full chunk's copy was unlike.
- **smoltcp's copy: alike in the bench, unlike in three quarters of the stream.** The UDP
  payload sits at offset 62 of the 64-byte aligned EMAC buffer (14 + 40 + 8 bytes of headers),
  2 modulo 4. The bench's slots are 2 modulo 4 too, so its `tx_stack` of about 10 200 cycles is
  the fast case. In the stream an unlike slot adds about 10 800 cycles to `tx_stack`: this is
  why the stream's `tx_stack_cyc` is 18 000 to 18 500 and the bench's 10 200.
- **For the fix (plan step 7.1):** both copies are alike only when the slot is 2 modulo 4 and
  the payload's place in the slot (slot plus header length) matches the source modulo 4. In the
  stream that is about 21 000 cycles per datagram to win, more than the plan's estimate for the
  application copy alone.

### Measured: CPU accounting with and without the `accounting` feature, 2026-11-11

This tree, this board and link. Each build with `--features idle-counter`: accounting on, or
`--no-default-features --features idle-counter` off. The busy share is `100 % - idle` from the
idle counter, which runs in both; cycles per datagram are the busy cycles of the window (at
400 MHz) over the datagrams sent. Bench: `netbench.py tx`, 20 s each (`accidle2-bench.*`,
`noaccidle2-bench.*`); stream: 10 s windows with `jrx` and the test hooks' report
(`accidle3-stream.*`, `noaccidle2-stream.*`); logs in `/tmp/p4perf-logs/StreamBase/`.

| Run | Host rate, both builds | Idle, on / off | Busy, on / off | Cycles per datagram, on / off | Cost of the accounts |
|---|---|---|---|---|---|
| Bench, internal RAM, run 1 | 11.795 / 11.796 MB/s | 1.4 / 4.8 % | 98.5 / 95.1 % | 48 130 / 46 501 | 1 629 cycles per datagram, 3.4 points |
| Bench, internal RAM, run 2 | 11.795 / 11.796 MB/s | 1.4 / 4.8 % | 98.5 / 95.1 % | 48 128 / 46 488 | 1 640, 3.4 points |
| Bench, camera frames | 11.801 MB/s | 1.4 / 1.6 % | 98.5 / 98.3 % | 48 460 / 48 346 | not visible: the core is nearly full in both |
| Stream, raw 1280x800 RAW8, link-bound | 90.01 / 89.93 Mbit/s | 5.5 / 6.2 % | 94.5 / 93.8 % | 47 620 / 47 268 | 352, 0.7 points |
| Stream, JPEG q90, gain 2x, 120 Hz | 14.96 / 14.90 Mbit/s | 82.6 / 83.3 % | 17.4 / 16.7 % | 49 571 / 47 970 | 1 601, 0.7 points |

So the trace hooks, the handler wrappers and the driver wrapper's statistics cost about 1 600
cycles per datagram (3.4 % of the 46 500), where the core has room. Near a full core (the
camera bench, the raw stream at the wire rate) the difference shrinks: the executor then sleeps
less often, and each sleep and wake avoided saves about 3 300 cycles (below), which hides the
accounts' cost [INFERENCE: from the wake and sleep counts, not measured apart]. The default
stays `accounting` on; the cost is the price of the figures in every report.

**What the accounts now see** (accounting and idle counter on; same runs):

| Run | Executor busy | Handlers (outside the executor) | Wake path: count, cycles each | Sleep path: count, cycles each | Busy with them (`busy_all`) | Busy by the idle counter |
|---|---|---|---|---|---|---|
| Bench, internal RAM | 90.4 % | 1.6 % (1.3 %) | 113 450, 1 462 | 52 088, 1 862 | 95.0 % | 98.5 % |
| Bench, camera frames | 92.8 % | 1.4 % (0.9 %) | 75 994, 1 546 | 73 228, 1 872 | 96.9 % | 98.5 % |
| Stream, raw RAW8, link-bound | 93.0 % | 0.9 % (0.2 %) | 7 819, 1 643 | (0.3 %) | 93.8 % | 94.5 % |
| Stream, JPEG q90, gain 2x | 16.1 % | 0.3 % (0.2 %) | 9 269, 1 509 | (0.3 %) | 17.0 % | 17.4 % |

Handlers per call (own cycles): the EMAC handler 785 in the bench (one call per datagram:
163 759 for 163 778 datagrams), 426 to 608 in the stream; the CSI DMA handler 886 to 1 345 per
frame; the JPEG codec's 948 and the 2D-DMA RX channel's 329 per frame; esp-rtos's timer tick 48
to 152 calls in 10 s, 2 500 to 3 000 cycles each, about 26 000 while camera frames go out. In the
bench the executor's busy share left out 4.6 points of real work (95.0 against 90.4 %): the
wake path 2.0, the handlers outside the executor 1.3 and the sleep path 1.2. Another 3.5 points
remain unseen by the accounts (95.0 against 98.5 %) [INFERENCE: the trap entry and exit around
each handler, and esp-rtos's software interrupt outside the wake and sleep windows]. In the
stream the executor sleeps rarely (8 000 to 9 000 times in 10 s), and the accounts come within
0.4 to 0.7 points of the idle counter.

### Measured: step 1, the datagram copy through PIE, 2026-11-11

Plan step 7.1. The application's copy into the socket ring is `Pie::copy` (`esp32p4-pie`: 16
bytes per aligned load and store, the source shifted into place with `esp.src.q` when it is
aligned unlike the destination). The bench copies its payload with it; the stream's send task
lets `FrameChunk::encode_header` (`espion_proto`; the bytes on the wire are unchanged, the
crate's tests compare them with the derived encoding) write the chunk's header and copies the
bytes with PIE. PIE is enabled once on core 0, where the thread-mode executor runs, and every
task of that executor shares the token (one thread; no interrupt handler uses PIE).

Boot self-tests on the board (every build since): `self_test_copy` 0 failed of 4096 cases
(every pair of source and destination alignment 0..16, lengths 0..=1600, guard bytes around;
108 M cycles), `self_test_sum` 0 failed of 4096 (41.6 M cycles).

Before and after: the same tree and flags (no frame pointers, `c2a842d`), built twice, the
"before" build with `copy_from_slice` (the ROM `memcpy`) in place of `Pie::copy`; flashed in
one session. Logs and ELFs in `/tmp/p4perf-logs/StreamSend/` (`s1-bench-rom.*`,
`s1-bench-pie.*`, `s1-stream-rom.*`, `s1-stream-pie.*`). Bench: `tools/netbench.py tx`, 20 s
per source:

| Source, copy | Datagrams | Host MB/s (datagrams/s) | Lost, dup., reordered, bad | CPU busy (`busy_all`) | `copy_cyc` | TX task | Runner | `tx_stack_cyc` | `tx_driver_cyc` | Token requests per datagram x `token_cyc` |
|---|---|---|---|---|---|---|---|---|---|---|
| Internal, ROM `memcpy` | 163 778 | 11.795 (8187) | 0, 0, 0, 0 | 89.4 % (93.4 %) | 13 041 | 15 452 | 27 117 | 10 142 | 2 086 | 4.84 x 1 161 |
| Internal, PIE | 163 780 | 11.795 (8187) | 0, 0, 0, 0 | 65.4 % (70.2 %) | 1 349 | 3 794 | 27 134 | 10 139 | 2 063 | 4.95 x 1 153 |
| PSRAM, ROM `memcpy` | 162 773 | 11.799 (8136) | 0, 0, 0, 0 | 91.2 % (93.5 %) | 16 222 | 18 505 | 25 500 | 10 216 | 2 105 | 3.97 x 1 200 |
| PSRAM, PIE | 162 775 | 11.799 (8136) | 0, 0, 0, 0 | 71.9 % (76.7 %) | 4 554 | 7 064 | 27 241 | 10 214 | 2 077 | 4.97 x 1 153 |
| Camera, ROM `memcpy` | 162 705 | 11.801 (8137) | 0, 0, 0, 0 | 92.4 % (94.7 %) | 16 690 | 19 069 | 25 561 | 10 247 | 2 085 | 3.95 x 1 163 |
| Camera, PIE | 162 707 | 11.801 (8138) | 0, 0, 0, 0 | 73.1 % (77.9 %) | 4 922 | 7 478 | 27 384 | 10 247 | 2 095 | 4.97 x 1 153 |

Every bench copy has its source at 0 and its destination at 2 modulo 16, so PIE takes its
shifting path: 0.94 cycles per byte from internal RAM (1436 bytes; the spike's aligned loop
did 0.49), 3.2 from cold PSRAM, where the cache fills set the pace.

Stream: the scratch receiver `jrx` (a copy in `/tmp/p4perf-logs/StreamSend/jrx/` that also
asks for the `N` report), 10 s windows after 2 s of warm-up; cycles per datagram from the
test hooks (`R` and `N`); `copy_cyc` here includes the chunk's header:

| Run, copy | Host | Core 0 busy (`busy_all`) | net task | send task | `copy_cyc` | Copies unlike mod 4 / mod 16 | `tx_stack_cyc` | Token requests per datagram x `token_cyc` |
|---|---|---|---|---|---|---|---|---|
| Raw 1280x800 RAW8 120 Hz (link), ROM | 89.93 Mbit/s, 10.98 fps | 93.3 % (93.8 %) | 28 046 | 17 806 | 15 350 | 76 % / 96 % | 18 534 | 2.05 x 1 153 |
| Raw 1280x800 RAW8 120 Hz (link), PIE | 89.93 Mbit/s, 10.98 fps | 84.9 % (87.1 %) | 32 391 | 8 944 | 6 083 | 24 % / 93 % | 18 095 | 3.65 x 1 191 |
| JPEG q90, gain 15.5x, 153.98 Hz (link), ROM | 92.73 Mbit/s, 78.5 fps | 98.8 % (99.0 %) | 26 549 | 19 578 | 16 745 | 75 % / 95 % | 17 776 | 1.83 x 1 092 |
| JPEG q90, gain 15.5x, 153.98 Hz (link), PIE | 92.62 Mbit/s, 74.8 fps | 92.0 % (93.6 %) | 31 388 | 10 899 | 7 520 | 75 % / 94 % | 17 400 | 3.55 x 1 203 |
| JPEG q90, gain 2x, 120 Hz, ROM | 13.55 Mbit/s, 120.05 fps | 14.5 % (15.1 %) | 25 041 | 18 538 | 16 547 | 96 % / 100 % | 15 867 | 1.92 x 1 244 |
| JPEG q90, gain 2x, 120 Hz, PIE | 13.62 Mbit/s, 120.06 fps | 11.2 % (11.7 %) | 25 054 | 7 515 | 5 505 | 97 % / 100 % | 15 652 | 1.94 x 1 297 |

Every run: 0 incomplete or bad frames at the host, 0 capture errors, 0 EMAC missed frames. The
frame rate of the noisy JPEG case follows the file sizes of the moment (the link carries the
same 92.6 Mbit/s).

- **Kept.** Net and send tasks together: 45 852 to 41 335 cycles per datagram in the raw
  stream (-10 %), 46 127 to 42 287 with link-bound JPEG (-8 %), 43 579 to 32 569 at 13.6 Mbit/s
  (-25 %). The copy itself costs 9 000 to 11 000 cycles less.
- **The runner costs more per datagram where the link is the limit** (bench PSRAM and camera,
  both link-bound streams: +1 700 to +4 800 cycles): the send task refills the socket ring at
  once now, so the runner wakes for fewer datagrams at a time (token requests per datagram
  1.8-2.1 to 3.6 in the stream) [INFERENCE: from the token and poll counts]. Step 2 targets
  that cost.
- **smoltcp's copy is unchanged** (`tx_stack_cyc` within 2 %): in the stream its source (the
  slot in the socket ring) and its destination (offset 62 of the DMA buffer) are still unlike
  modulo 4 in three quarters of the datagrams, where the ROM `memcpy` copies byte by byte.

**Misaligned 32-bit accesses** (the bench service at boot, `log_misaligned_access`: 512 loads,
then 512 stores, 4 bytes apart in L1, interrupts masked; cycles per access including the loop):
loads 3.3 at a word boundary, 9.3 at +1, 6.3 at +2, 9.3 at +3; stores the same. The core does
them in hardware (no trap, no emulation), at 3 to 6 cycles more than an aligned one. A
`memcpy` for buffers unlike modulo 4 could use them (about 3 cycles per byte, against the ROM's
9) or shift-and-merge aligned words. Not done, for two reasons: `memcpy` is bound to the ROM by
a plain assignment in esp-rom-sys's linker script (`memcpy = 0x4fc0026c;` in
`ld/esp32p4/rom/additional.ld`, not a `PROVIDE`), so a definition in this crate does not
replace it and only a linker option (`--wrap` or `--defsym` in the build's link arguments)
could; and step 3 takes the chunk datagrams, nearly all of the stream's bytes, off smoltcp's
copy. smoltcp then copies only answers, neighbour discovery and ping.

## The Ethernet driver (esp-hal 1.2.2 `ethernet`, `unstable`)

Read in the registry source and measured here. The first three needed workarounds in this
crate; they are defects to take upstream.

1. **TX store-and-forward breaks frames on the P4.** `dma_start` sets `TX_STR_FWD` (and the
   checksum insertion needs it). With it, 56-byte pings passed, 400-byte ones failed with
   ICMPv6 checksum errors at the host, 1000 and 1452-byte ones were lost; a 10 s run left the
   board at 11.77 MB/s but the host received none of 81 680 datagrams (its NIC counted 16 320
   frames, the rest never arrived; UDP checksum errors on those), and afterwards even 56-byte
   pings failed. ESP-IDF disables TX store-and-forward on every chip
   (`emac_hal_init_dma_default`, threshold 64) and states a 256-byte RX FIFO for the P4
   [INFERENCE: the TX FIFO is smaller than a full frame too]. `net` stops the TX DMA, clears
   the bit, sets the 64-byte threshold, flushes the TX FIFO and restarts it.
2. **No checksum insertion then.** The driver sets CIC 3 on every TX descriptor and tells
   smoltcp to compute no checksum. In threshold mode the EMAC inserts none: with smoltcp left
   at that, every ping failed with ICMPv6 checksum errors at the host and the board was
   unreachable (neighbour discovery too). `net/device.rs` overrides the capabilities: smoltcp
   computes the UDP and ICMPv6 checksums on TX (part of `tx_stack`). In store-and-forward mode
   (before the fix) small frames did get correct inserted checksums. RX checksums stay with the
   EMAC (the driver drops frames its RX engine flags).
3. **The DMA-buffer check uses the wrong RAM range.** `InternalMemory::get_mut` asserts the
   address is in esp-metadata's "DRAM" range, 0x4FF40000..0x4FFADFC0, which assumes a 256 KiB
   L2 cache. With the 128 KiB the bootloader sets (`ESP_HAL_CONFIG_L2_CACHE_SIZE`, as
   `poc-p4-eth`), RAM starts at 0x4FF20000, the storage landed in `.bss` below 0x4FF40000 and
   the first `Ethernet::new` panicked (`assertion failed: is_valid_ram_address`). `dram_dma.x`
   adds a NOLOAD section before the stack at 0x4FF40000 or above; `net` zeroes the storage
   there before use (all-zero is the value `EthernetDmaStorage::new` builds).
4. **`GenericPhy` busy-polls.** Its `poll_link` reads BMSR twice and ANLPAR and calls
   `wake_by_ref` every time; embassy-net calls it at the end of every runner poll, so the
   runner never sleeps (measured above: 99.8 % busy). `net/ip101.rs` reads the PHY every
   500 ms, woken by a 500 ms task. The MDC divider at a 400 MHz CPU clock is /124 = 3.2 MHz,
   above the 2.5 MHz of IEEE 802.3 clause 22 (from the source); MDIO works on this board.
5. **RX cache maintenance works.** The driver invalidates each RX descriptor and buffer before
   reading; 60 s at the wire limit with every byte checked found no bad byte (below).
6. Costs to know: every TX descriptor asks for a completion interrupt (one runner wake per
   frame), and each token request makes one or two ROM cache calls (about 1 200 cycles).
7. **smoltcp 0.13.1 (embassy-net 0.9.1) can panic in `UdpSocket::send_to_with`**
   (`unwrap of f.take() failed`, udp.rs:304): `PacketBuffer::enqueue_with_infallible` runs the
   fill closure, then fails when the ring-wrap padding took the last metadata slot. It happened
   here with 8 metadata slots for 8 datagrams of payload ring. Give a socket's TX ring more
   metadata slots than it can hold datagrams, plus one (`bench::meta_slots`). Fixed in smoltcp
   0.14.

Not verified: 10 Mbit/s or half duplex, link loss and re-negotiation, the PHY reset timing,
multicast, the EMAC's PTP clock and timestamps.

## Not done, and what remains

- **The link** limits JPEG only for very detailed or noisy pictures at high quality (above
  about 76 kB per frame at 154 fps); the automatic quality answers that. Raw frames stay
  link-bound (11 fps at 1280x800).
- **The encoder at the sensor's maximum**: 92.3 % busy at 1280x800 153.98 Hz. Files above
  about 300 kB (q95-100 with noise) or the link at its limit lengthen the encode past the
  frame period and cost an occasional frame (counted in `encoder.frames_skipped`). Running the
  encode task in an interrupt-priority executor would remove the wake-up wait behind the
  network's polls [INFERENCE: not tried; that the wait is the cause is not measured].
- **CPU**: at the wire rate core 0 is 99 % busy, almost all per datagram (about 46 000 cycles:
  smoltcp, the driver, the chunk copy). Core 1 is off ([the two-core attempt](#the-two-core-attempt)).
- Not verified: an encoder timeout (never seen; the path is the same as the driver's errors),
  the encoder's codec and DMA errors on this firmware (only `OutputTooSmall` was provoked),
  quality below 20 on the camera, a scene in daylight (the room was dark for every run).
- Keep `net/device.rs`'s checksum override and `net::tx_threshold_mode` until esp-hal fixes the
  driver; size TX socket metadata rings as in item 7 of [The Ethernet driver](#the-ethernet-driver)
  and give TX rings at least twice the largest datagram (smoltcp wants it contiguous).
