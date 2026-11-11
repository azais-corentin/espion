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
python3 firmware/poc-p4-stream/tools/streamdiag.py net      # the send path (copies, driver)
python3 firmware/poc-p4-stream/tools/streamdiag.py path     # and the frame path (encode, prefetch)
```

Cargo features: `accounting` (default) builds the CPU accounts; `--no-default-features`
leaves them out; `--features idle-counter` adds the idle counter (see
[What the firmware does](#what-the-firmware-does)); `--features markers` the marker pins and
the STROBE output ([Marker pins](#marker-pins-feature-markers)).

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

With `SERVICE = Service::BenchVision` the same test service runs on core 0 while core 1,
started bare (`espion-core1`), runs the blob detector or the pixel check on the camera's live
1280x800 RAW8 strips at 120 fps ([Co-load](#co-load-the-detector-on-core-1-beside-the-wire-rate-2026-11-11));
build it with `--features stats` for the cache lock's per-core figures. Core 1 starts at the
first `vision` request (the boot and the first runs are the same service without core 1):

```sh
python3 tools/netbench.py vision --job detect --autoload 1    # or --job check / --job off
python3 tools/netbench.py tx --source psram --seconds 60      # internal and psram only here
```

`tools/netbench.py` (Python standard library only) binds every socket to the link-local address
of the interface, sends the request first, and prints the host's figures (rate, lost, duplicate,
reordered and bad datagrams, the host's UDP6 and interface counters) and the board's report.
The board logs every report too, after the run. `bench: ready` in the log means the service
listens.

### Choosing what runs

`SERVICE` at the top of `src/main.rs`: `Service::Stream` (default) runs the camera stream,
`Service::Bench` the network test service below, `Service::BenchVision` that service with
core 1 on the camera's strips, `Service::Network` only the network (ping, neighbour
discovery).

## What the firmware does

1. Switches UART0 to 2 Mbaud, prints the chip, enables PIE (the P4's SIMD extension,
   `esp32p4-pie`) on core 0, where the executor runs, and runs its copy and checksum-sum
   self-tests against the scalar references (4096 cases each, about 0.4 s; a failure panics),
   brings up PSRAM (250 MHz).
2. `net::start`: resets the PHY, creates esp-hal's EMAC driver (which initialises the MAC and
   the PHY over MDIO), takes its DMA over with this crate's rings (`net/eth.rs`, TX threshold
   mode from the start; see [The Ethernet driver](#the-ethernet-driver-esp-hal-122-ethernet-for-the-bring-up-this-crates-dma-rings)),
   creates the embassy-net stack with the static address, spawns the runner (`net_task`) and
   the link task, and waits up to 8 s for the link. The link task wakes the runner every
   500 ms so the PHY's link is read, and logs each change (speed and duplex read from the
   PHY).
3. The service: `stream::start` (below, [The camera stream](#the-camera-stream)) or
   `bench::start`, which spawns the two test tasks; the TX task fills its source buffers and
   finds the camera first, then logs `bench: ready`.

The test service (`src/bench.rs`, the module docs have the formats):

- **UDP 5010, board to host**: a 4-byte request `[kind, prefetch, seconds u16 LE]` (`prefetch`
  0 leaves the cache prefetch of `src/prefetch.rs` as it is, `1 + n` selects mode `n`:
  `netbench.py tx --prefetch n`); the board sends
  1452-byte datagrams to the requester as fast as the stack takes them, then one report
  datagram. Each datagram has a 16-byte header (sequence number, frame number, offset in the
  frame, frame length) and 1436 bytes of the frame. Kinds: `I` a 64 KiB buffer in internal RAM;
  `P` a 1 024 000-byte buffer in PSRAM, its cache lines written back and invalidated before
  each pass (cold, as after a DMA write); `F` camera frames, the newest complete one each time;
  `C` capture only (frames counted and dropped, no datagrams but the report); `D` nothing, the
  board logs `bench: host done`; `V` (`Service::BenchVision` only, `[V, job, autoload, 0]`,
  job 0 off, 1 detector, 2 pixel check) ends core 1's run and starts the one asked for, and
  the board answers with one report datagram ("...: ok" or why it refused). A run whose
  prefetch mode writes the L1 data autoload registers is refused while core 1's run has the
  autoload on, and the other way round; `F` and `C` are refused in `BenchVision` (the camera
  feeds core 1). In `I` and `P` every byte is a known pattern the host checks.
  Datagrams go beside smoltcp (`net/chunk.rs`) once the host's MAC is known, through the
  socket before. At boot the service checks the chunk path's UDP checksum against smoltcp's
  on 4096 random datagrams (`bench: UDP checksum of the chunk path ...`; a failure panics).
- **UDP 5011, host to board**: datagrams `[seq u32, stream u32, payload]` whose payload is a
  known pattern shifted per sequence number; the board checks every byte and counts datagrams,
  bad ones, gaps and late ones per stream, and answers a query (stream `0xFFFF_FFFF`) with its
  counts.

The reports carry the CPU time: `copies` and `copy_cyc`, the datagrams that went through the
socket and the cycles of each in the application's copy into the socket buffer (PIE's copy
since 2026-11-11, [step 1](#measured-step-1-the-datagram-copy-through-pie-2026-11-11)),
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
it and the wake path; for the datagrams sent beside smoltcp (`direct`, since
[step 3](#measured-step-3-chunk-datagrams-beside-smoltcp-2026-11-11)) the cycles of each from
the room found to the frame queued (`direct_cyc`), of which the headers and the checksum
(`direct_hdr_cyc`), and how many waited for room (`direct_waits`); and the EMAC's DMA status,
missed-frame counter, MAC debug register and the TX frames it reported with an error since
boot (`tx_errors`, of which `tx_underflows`).

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
| `src/net/mod.rs` | The network module: pins and their sources, PHY reset, esp-hal's EMAC bring-up and its one-descriptor storage in its linker section, embassy-net with the static IPv6 address, the runner and link tasks, EMAC register snapshot |
| `src/net/eth.rs` | The EMAC's DMA rings and the `embassy-net-driver` driver: descriptors through the non-cacheable alias, one cache call per frame, TX threshold mode, TX completion interrupt every 8th frame, the RX checks, the `ETH_MAC` handler |
| `src/net/ip101.rs` | IP101 PHY driver (`Phy` trait): link polled every 500 ms, speed and duplex from ANAR and ANLPAR, the last report for the log |
| `src/net/device.rs` | `embassy-net-driver` wrapper around the driver: per-frame cycle counts (feature `accounting`) |
| `src/cpu.rs` | Cycle counter, per-task CPU time and polls from the executor's `trace` hooks (feature `accounting`), the idle counter (feature `idle-counter`), timed events (count, cycles, longest) |
| `src/prefetch.rs` | The cache prefetch around the CPU's reads of PSRAM (the datagram body copies, the RAW10-to-RAW8 selection): the modes, why no stale line reaches the CPU |
| `src/irq.rs` | Interrupt-handler wrappers: calls and own cycles per handler and core, the time outside executor passes, the wake and sleep paths (feature `accounting`) |
| `src/camera.rs` | OV9281 and CSI receiver: set-up, any mode's start (lanes checked in their stop state before stream-on), controls, stop, the newest frame, the fault switch of the stream's error-path test |
| `src/stream/mod.rs` | The camera stream service: the shared stream state, the control task (requests, all-or-nothing `SetCamera`, repeats), the hand-off to the capture, encode and send tasks |
| `src/stream/check.rs` | The `SetCamera` checks in the protocol's order, as the viewer's fake camera makes them |
| `src/stream/capture.rs` | The capture task: frames, counters, the stream rules, mode changes and recovery |
| `src/stream/jpeg.rs` | The encode task: the JPEG pipeline, its output buffers, encoder errors, the automatic quality |
| `src/stream/send.rs` | The send task: answers through the socket; frame chunks beside smoltcp (`net/chunk.rs`: `espion_proto` writes the chunk header, PIE copies the bytes), through the socket until the host's MAC is known |
| `src/net/chunk.rs` | UDP datagrams sent beside smoltcp, straight onto the TX ring: the peer's MAC learned from received frames, the headers, a PIE copy of the payload into the descriptor's buffer, the UDP checksum (PIE), the checksum self-test |
| `src/stream/diag.rs` | The stream's test hooks on UDP 5010: CPU, interrupt-handler, send-path, frame-path (encode set-up, wake-up, end) and encoder reports, fault injection, stall, log marker, too-small encoder outputs, encode-only, prefetch mode |
| `tools/streamdiag.py` | Host side of the stream's test hooks |
| `src/bench.rs` | The test service: TX runs from internal RAM, PSRAM or the camera, the RX check, the reports, the `vision` request (`Service::BenchVision`) |
| `src/vision.rs` | `Service::BenchVision`'s camera side on core 0: the pixel check's reference (whole frames, once), core 1 and the strip receiver started at the first request, each run's set-up, the monitor every millisecond and its lines once a second, the run's totals |
| `../espion-core1` (crate) | Core 1's runner (bare: the detector or the pixel check on each strip, the result queue) and core 0's handle and monitor |
| `dram_dma.x` | Linker section for esp-hal's Ethernet DMA storage at 0x4FF40000 or above (see below) |
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
  interrupts are on core 0 too. The send task sends each chunk beside smoltcp
  (`net/chunk.rs`): `FrameChunk::encode_header` writes the chunk's header after the Ethernet,
  IPv6 and UDP headers in a TX descriptor's buffer, PIE copies the bytes from PSRAM after it
  (`Pie::copy`, any alignment), the one CPU copy of the pixels or of the JPEG file (RAW10
  sent as RAW8 is reduced to its high bits in that place instead), and PIE sums the payload
  for the UDP checksum. Until a frame from the host was received (its MAC unknown), chunks go
  through the socket, PIE copying the bytes into its ring. Core 1 stays off (below: the
  two-core attempt hung).
- **RAM** (boot log, 2026-11-11): 134 272 B of internal RAM static, of which the network's
  DMA rings and esp-hal's storage are 53 376 B and the service's sockets and buffers 22 740 B
  (camera socket: 8 x 1452 B TX ring, 4 KiB RX ring; the hooks' socket); 447 296 B left for
  the main stack. PSRAM: the four frame buffers, 5 144 576 B, then the encoder's three output
  buffers, 6 291 456 B.

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
   PSRAM into a TX descriptor's buffer.

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

### Co-load: the detector on core 1 beside the wire rate, 2026-11-11

`Service::BenchVision`, one build (`--features stats`), one boot per session, this board, the
link at 100 Mbit/s. Logs in `/tmp/p4perf-logs/CoLoad/`: `co1.log` / `co1.host.txt` (the plan
below, `coload.sh`), `co2.log` / `co2.host.txt` (the follow-up on the preload, `co2.sh`),
parsed by `parse.py`. Core 1: live 1280x800 RAW8 at 120 fps, 64-row strips, 3 strips, PIE
skip; the room was dark (0 blobs per frame), so the detector is faster than in `poc-p4-eth`'s
runs (754 against 845 us): compare within this table only. Bench prefetch `preload_ahead`
unless named. Core 0's figures are the bench report's (`busy_pct`, the TX task's cycles per
datagram, the CPU accounts' `vision` task); lock figures are the run's maxima per core.

| Run (60 s unless named) | Datagrams, MB/s, lost/dup/reord/bad | Core 0 busy, TX task cyc/dg | Detect mean / max, ins/frame | Strip start latency max | Lock hold / wait max, core 0 · core 1 |
|---|---|---|---|---|---|
| internal, core 1 not started | 491 295, 11.796, 0/0/0/0 | 13.6 %, 6 338 | | | |
| psram, core 1 not started | 488 262, 11.801, 0/0/0/0 | 14.1 %, 6 589 | | | |
| (a) idle, autoload on | | | 754 / 779 us, 114 674 | 9 us | 9 / 7 us · 9 / 7 us |
| (b) internal, autoload on | 491 099, 11.792, 0/0/0/0 | 15.3 %, 6 635 (+ vision 498) | 758 / 785 | 11 | 5 / 4 · 2 / 2 |
| (c) psram, autoload on | 487 999, 11.794, 0/0/0/0 | 17.2 %, 7 360 (+ vision 601) | 769 / 805 | 19 | 14 / 10 · 8 / 15 |
| idle, autoload off | | | 974 / 1000, 114 388 | 9 | |
| internal, autoload off | 491 089, 11.791, 0/0/0/0 | 15.3 %, 6 627 | 980 / 988 | 11 | 9 / 9 · 2 / 2 |
| psram, autoload off | 488 005, 11.794, 0/0/0/0 | 16.3 %, 6 940 | 1028 / 1110 | 23 | 14 / 10 · 14 / 19 |
| psram, prefetch off, autoload on, 30 s | 244 005, 11.794, 0 | 22.4 %, 9 930 | 803 / 844 | 20 | 14 / 10 · 8 / 11 |
| psram, L2 autoload, autoload on, 30 s | 243 997, 11.794, 0 | 18.7 %, 8 159 | 765 / 801 | 19 | 14 / 10 · 8 / 16 |
| psram, `preload` (same chunk), autoload on, 30 s | 243 997, 11.794, 0 | 20.7 %, 9 099 | 771 / 817 | 19 | 14 / 10 · 7 / 16 |
| psram, `l1l2` on core 0, core 1 autoload off, 30 s | 244 004, 11.794, 0 | 18.7 %, 8 154 | 1011 / 1070 | 22 | 14 / 10 · 3 / 14 |
| RX 20 s (host to board, every byte checked), autoload on | 162 616, 11.802; bad, lost, late 0 | | 765 / 768 | 10 | 2 / 2 · 2 / 2 |
| (d) psram 600 s, pixel check, autoload on | 4 879 045, 11.792, 0/0/0/0 | 32.8 %, 15 194 | check: 57 824 000 rows, 0 bad | 23 | 14 / 10 · 14 / 19 |

In every run: late strips, lapped strips, abandoned frames, dropped results, stalls, length
and DMA errors, `ISR runs`, EMAC `missed` and `tx_errors` 0; core 1's heartbeat 82 000-85 600
per second (35 000 in the check); core 0 spent 450 us per second in `service()` and the
drain idle, 1 850 under psram load (longest call 60 us). (d): 939 640 strips, 72 280 of 72 280
frames good. Refusals, each answered: `--prefetch 1` (`l1l2`) while core 1's autoload was on;
a `vision` request with the autoload while `l1l2` was in use; `--source camera`.

What it costs core 0: the detector adds the `vision` task (500-600 cycles a datagram: the
millisecond monitor) and the CSI interrupts (handlers 130-140 → 470-490 cycles a datagram);
from internal RAM nothing else (6 338 → 6 635), from PSRAM with `preload_ahead` 770 more
(7 360) with core 1's autoload on, 350 (6 940) with it off. The pixel check (`co2.log`, 30 s
each) shows where that goes: psram `preload_ahead` 15 183 cycles a datagram with core 1's
autoload on, 6 898 with it off; prefetch off 9 906 and 9 903 (on, off); internal 6 628 and
6 619. So the cost comes only from core 0's manual L1 data preload beside core 1's L1 data
autoload, and grows with the share of time core 1's autoload section is open (about 9 % of
the time for the detector, 75 % for the check, which reads each strip for 500 us): core 0's
next `l1d_preload` waits for the previous one, which does not finish while core 1's section
is open [INFERENCE: the two share one prefetch engine; the ROM's own preload suspends the
autoload, which fits; not documented]. No wrong byte: the host checked every datagram of
every run, and core 1's pixel check found 0 bad rows beside psram with `preload_ahead` and
with prefetch off, its autoload on and off, and beside internal (`co2.log`, 30 s each, plus
(d)); beside `l2`, `preload` and `l1l2` only the detector ran, so core 1's bytes there are
not checked.
For the detector the interaction costs 6 % of core 0's TX cycles (7 360 against 6 940):
keep `preload_ahead`, or use `l2` (8 159) if the autoload's share grows.

## Marker pins (feature `markers`)

Performance plan step 0.4, as in `poc-p4-eth` (its README, "Marker pins", has the full
rationale): pins that toggle at fixed points, for a logic analyser beside the sensor's STROBE.
Off by default; `cargo build --release --features markers` (also with
`--no-default-features`). The feature also turns the sensor's STROBE output on (0x3006 =
0x0C after each mode set-up, `camera::setup`).

| Signal | GPIO | 40-pin header | Driven by | Toggles when |
|---|---|---|---|---|
| datagram queued | GPIO26 | pin 29 | core 0, dedicated-IO channel 0 (`markers::DATAGRAM`) | `net::chunk::send` has handed a datagram to the EMAC's TX ring (`eth::tx_queue`): every bench datagram and every camera-stream chunk on the direct path |
| core 1 strip | GPIO20 | pin 35 | core 1, channel 0 (`espion_core1::MARKER_STRIP`) | `Service::BenchVision`: core 1's reader has handed out a strip, before the detector |
| core 1 frame | GPIO21 | pin 34 | core 1, channel 1 (`MARKER_FRAME`) | `Service::BenchVision`: the frame's blobs are final |
| GND | | pins 28, 33 | | |
| STROBE | | not on the board: the camera module's STROBE back pad | OV9281 | Once per frame |

- Pins named in `src/markers.rs`; header numbering, why these pins are free (each wired only
  P4 to header on the Waveshare schematic; none of camera, RMII, UART, strapping, microSD,
  audio, USB or JTAG) and STROBE's place and level: `poc-p4-eth`'s README.
- Toggles, not pulses: each level lasts until the next event (about 122 us between datagrams
  at the wire rate, about 10 us at the closest when the ring has room [INFERENCE: one
  `direct_cyc` of about 4 200 cycles]), so any analyser from 1 MS/s sees every edge; count
  every edge, rising or falling, as one event.
- Figures to read: STROBE to datagram edges in `Service::Stream` (exposure to the frame's
  chunks on the wire queue); in `BenchVision`, STROBE to core 1's frame edge (exposure to
  centroids). Frame done to datagram queued is not available in one firmware today: neither
  PoC sends the detector's result on the network, so the datagram marker times bench
  datagrams or camera chunks, never centroids.

**Boot self-check** (`espion_core1::markers::self_check`, pads read back through `GPIO_IN`,
no wire): `Service::BenchVision`, the host's `vision --job detect --autoload 1` then `tx
--source internal`, 2026-11-11, `/tmp/p4perf-logs/Markers/stream-bv-on2.log`: in 1.000 s
8189 datagram edges (the host received 8186 datagrams/s), 1561 core-1 strip edges, 120
frame edges, none merged.

**Cost**, one boot each, same session, `tools/netbench.py tx --source internal --seconds 20`
(`first/stream-bv-off.host.txt`, `first/stream-bv-on.host.txt`; the self-check's second falls
in the first run):

| | without `markers` | with `markers` |
|---|---|---|
| `direct_cyc` per datagram, core 1 detecting (two runs) | 4213.5 / 4211.4 | 4246.2 / 4243.0 |
| `direct_cyc` per datagram, core 1 idle | 3986.8 | 4017.2 |
| Core 1 detect per frame, mean / max | 758 / 787 us | 758 / 787 us |
| Host rate, datagrams/s | 8185 to 8188 | 8186 to 8188 |

One toggle measures 10 to 11 cycles back to back (`esp32p4-perf` README); the datagram path
grows by 30 to 33 cycles (0.8 %), of which `direct_hdr_cyc` and `direct_copy_cyc` (work
before the toggle) take 17 [INFERENCE: code placement]. Between boots of the same build
`direct_cyc` moves by about 30 cycles too (3959 to 3987 without the feature, core 1 idle), so
the difference is at the noise between boots. Without the feature the TX and net tasks, core
1's `run` and the detector are instruction for instruction those of the build before the
change (llvm-objdump, `BenchVision` with and without default features).

Not done: no logic analyser or scope was connected; no latency, jitter or STROBE waveform
has been measured.

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

### Measured: step 2, the Ethernet ring, 2026-11-11

Plan step 7.2. `net/eth.rs` replaces esp-hal's descriptor rings and its embassy-net driver
(esp-hal keeps the bring-up, the PHY and the MAC's speed and duplex; see
[The Ethernet driver](#the-ethernet-driver-esp-hal-122-ethernet-for-the-bring-up-this-crates-dma-rings)):

- 16 RX and 16 TX descriptors (32-byte enhanced format), each with a 1536-byte buffer, 50 176
  B in `.bss`. The CPU reads and writes the descriptors through the non-cacheable alias of
  internal RAM (+0x4000_0000, as `esp32p4-jpeg`); the DMA gets the ordinary addresses. Their
  lines are written back and invalidated once at the take-over and never used through the
  cache after.
- One cache call per frame, through `esp32p4-cache` (its cross-core lock): a TX buffer is
  written back after the stack wrote it; the lines of an RX frame the stack read are
  invalidated before the buffer goes back to the DMA, so no line is cached while the DMA
  writes it (all of them are invalidated at the take-over). A token request reads one or two
  descriptor words, no cache call.
- TX threshold mode (64 bytes) from the start, CIC 0, smoltcp computes UDP and ICMPv6.
- TX completion interrupt (`TDES0` IC) on every 8th frame only. The runner waits for room only
  when all 16 descriptors are in flight, so two frames of them ask for it. RX interrupt on
  every frame, as before. The driver registers its waker only when it returns no token.
- RX: the frames the DMA or the checksum engine flagged, partial ones and ones of an
  impossible length go back to the DMA unread (as esp-hal's checks).

Same session, same flags: the step 1 build (`7e303f2`) against this one; logs
`s2-base-bench.*`, `s2-bench.*`, `s2-base-stream.*`, `s2-stream.*` in
`/tmp/p4perf-logs/StreamSend/`. Boot log: `own DMA rings: 16 RX + 16 TX descriptors (50176
B), TX completion interrupt every 8 frames`; RAM 202 304 B static, 379 264 B left for the main
stack (network DMA 53 376 B: the rings and esp-hal's one-descriptor storage). Bench, 20 s per
source:

| Source, driver | Datagrams | Host MB/s (datagrams/s) | Lost, dup., reordered, bad | CPU busy (`busy_all`) | `copy_cyc` | TX task | Runner | `tx_stack_cyc` | `tx_driver_cyc` | Token requests per datagram x `token_cyc` | EMAC interrupts (cycles each) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Internal, esp-hal | 163 780 | 11.795 (8187) | 0, 0, 0, 0 | 65.5 % (70.2 %) | 1 349 | 3 798 | 27 143 | 10 136 | 2 074 | 4.95 x 1 155 | 163 759 (867) |
| Internal, own ring | 163 773 | 11.795 (8187) | 0, 0, 0, 0 | 36.5 % (37.1 %) | 1 328 | 2 946 | 14 736 | 9 772 | 943 | 1.51 x 159 | 20 473 (853) |
| PSRAM, esp-hal | 162 775 | 11.799 (8136) | 0, 0, 0, 0 | 72.0 % (76.8 %) | 4 554 | 7 065 | 27 287 | 10 222 | 2 084 | 4.97 x 1 157 | 162 756 (848) |
| PSRAM, own ring | 162 774 | 11.799 (8136) | 0, 0, 0, 0 | 45.6 % (46.2 %) | 4 527 | 6 328 | 15 776 | 9 859 | 957 | 1.98 x 194 | 20 348 (848) |
| Camera, esp-hal | 162 708 | 11.801 (8137) | 0, 0, 0, 0 | 73.2 % (78.0 %) | 4 928 | 7 516 | 27 400 | 10 268 | 2 092 | 4.97 x 1 154 | 162 689 (864) |
| Camera, own ring | 162 705 | 11.801 (8137) | 0, 0, 0, 0 | 45.9 % (46.6 %) | 4 892 | 6 685 | 15 637 | 9 871 | 957 | 1.86 x 190 | 20 341 (837) |

Host to board, 1452-byte datagrams unpaced for 20 s, every byte checked on the board: esp-hal
162 611 sent, 162 611 received; own ring 162 599 sent, 162 599 received (8130/s, 11.802 MB/s);
0 bad datagrams or bytes, 0 lost, 0 late, EMAC missed frames 0 in both.

Stream (as in [step 1](#measured-step-1-the-datagram-copy-through-pie-2026-11-11), `jrx`, 10 s
windows):

| Run, driver | Host | Core 0 busy (`busy_all`) | net task | send task | `tx_stack_cyc` | `tx_driver_cyc` | Token requests per datagram x `token_cyc` | Wake paths |
|---|---|---|---|---|---|---|---|---|
| Raw 1280x800 RAW8 (link), esp-hal | 89.99 Mbit/s, 10.98 fps | 85.0 % (87.2 %) | 32 401 | 8 965 | 18 093 | 2 135 | 3.65 x 1 197 | 34 972 |
| Raw 1280x800 RAW8 (link), own ring | 89.99 Mbit/s, 10.98 fps | 63.7 % (64.2 %) | 23 145 | 8 048 | 17 583 | 979 | 1.68 x 184 | 7 245 |
| JPEG q90, 15.5x, 153.98 Hz (link), esp-hal | 92.71 Mbit/s, 79.9 fps | 91.9 % (93.5 %) | 31 410 | 10 847 | 17 360 | 2 132 | 3.57 x 1 205 | 26 697 |
| JPEG q90, 15.5x, 153.98 Hz (link), own ring | 92.79 Mbit/s, 74.9 fps | 71.8 % (72.3 %) | 22 654 | 10 231 | 17 073 | 977 | 1.70 x 188 | 6 639 |
| JPEG q90, 2x, 120 Hz, esp-hal | 13.55 Mbit/s, 120.06 fps | 11.1 % (11.7 %) | 25 272 | 7 573 | 15 798 | 2 125 | 1.96 x 1 302 | 9 550 |
| JPEG q90, 2x, 120 Hz, own ring | 13.62 Mbit/s, 120.05 fps | 9.4 % (9.7 %) | 19 914 | 7 436 | 15 084 | 990 | 1.32 x 143 | 3 643 |

Every stream run: 0 incomplete or bad frames, 0 capture errors, 0 EMAC missed frames, request
round trips 1.34-1.38 ms (2x) and 4.6-4.9 ms (link full) as before. `poc-viewer`'s board test,
sections `patterns` and `raw10` (every byte of the bars pattern and of RAW8 from RAW10
checked), against this build: 0 failed checks (`s2-stream.board.txt`).

- **Kept.** Net and send tasks together: 41 366 to 31 193 cycles per datagram in the raw
  stream (-25 %), 42 257 to 32 885 with link-bound JPEG (-22 %), 32 845 to 27 350 at
  13.6 Mbit/s (-17 %). The runner alone: 27 100-27 400 to 14 700-15 800 in the bench.
- Where it went: the token requests (4 to 5 per datagram at 1 150 cycles, a ROM cache call
  each, to 1.3-2 at 140-190), the driver's part of a frame (2 100 to 950-990: a buffer
  write-back, no descriptor write-back), and 8 times fewer EMAC interrupts and runner wakes
  (1 000 a second at the wire rate instead of 8 100).
- smoltcp's `tx_stack` is unchanged (its copy and checksum; step 3).

### Measured: step 3, chunk datagrams beside smoltcp, 2026-11-11

Plan steps 7.3 and 7.4. `net/chunk.rs` sends the stream's chunks and the bench's datagrams
straight onto the TX ring of `net/eth.rs`, beside smoltcp, which keeps answers, neighbour
discovery, ping and the RX path:

- **Peer.** smoltcp does not expose its neighbour cache: the driver records the source
  address and MAC of every IPv6 frame it receives (4 entries, most recent first). The board
  only sends to the source of a request it received; until a frame from it came, datagrams
  go through the socket as before.
- **One copy.** The headers (Ethernet, IPv6, UDP: 62 bytes, written per datagram), the head
  (the chunk's header from `FrameChunk::encode_header`, the bench's 16 bytes) and a PIE copy of
  the body go into a TX descriptor's own buffer in internal RAM; one write-back, one
  descriptor. Before: PIE copied into smoltcp's ring, smoltcp copied into the DMA buffer and
  summed the datagram.
- **Checksum.** The pseudo-header as scalar words, the UDP header, head and copied body by
  `Pie::sum16_be` from internal RAM; a computed 0 goes out as 0xFFFF. The bench service
  checks it against smoltcp's `UdpPacket::fill_checksum` and `verify_checksum` at boot, on
  4096 random datagrams (heads 0-40 bytes, bodies 0-1440 bytes at any alignment, random,
  all-0xFF and all-zero): 0 failed (`s5-bench.log`).
- **Sharing the ring.** A datagram waits until 4 descriptors more than its own are free, so
  smoltcp always finds room; the waiter is woken by the TX completion interrupt (every 8th
  frame; a waiter needs more than 11 frames in flight, so one of them asks for it).
- **TX errors.** The driver now reads the status the DMA wrote back into each TX descriptor
  before it reuses it, and counts errors and underflows (`tx_errors`, `tx_underflows` in the
  bench's report, `emac_tx_errors`, `emac_tx_underflows` in the stream's `R`).

Same session, same flags: the step 2 build (`64d4b17`) against this one; logs
`s5-base-bench.*`, `s5-bench.*`, `s5-base-stream.*`, `s5-stream.*` in
`/tmp/p4perf-logs/StreamSend/`. Bench, 20 s per source; "net + TX" is the runner and the TX
task, cycles per datagram:

| Source, build | Datagrams | Host MB/s (datagrams/s) | Lost, dup., reordered, bad | CPU busy (`busy_all`) | Runner | TX task | Net + TX | `direct_cyc` (headers and sum) | Token requests per datagram x `token_cyc` |
|---|---|---|---|---|---|---|---|---|---|
| Internal, step 2 | 163 772 | 11.795 (8187) | 0, 0, 0, 0 | 36.5 % (37.1 %) | 14 731 | 2 946 | 17 677 | — | 1.51 x 158 |
| Internal, step 3 | 163 767 | 11.797 (8188) | 0, 0, 0, 0 | 13.0 % (13.7 %) | 246 | 6 071 | 6 317 (-64 %) | 3 889 (1 773) | 0.13 x 209 |
| PSRAM, step 2 | 162 770 | 11.799 (8136) | 0, 0, 0, 0 | 45.6 % (46.2 %) | 15 775 | 6 331 | 22 106 | — | 1.98 x 194 |
| PSRAM, step 3 | 162 760 | 11.801 (8137) | 0, 0, 0, 0 | 19.7 % (20.4 %) | 279 | 9 336 | 9 615 (-57 %) | 7 146 (1 796) | 0.13 x 233 |
| Camera, step 2 | 162 707 | 11.801 (8138) | 0, 0, 0, 0 | 45.9 % (46.6 %) | 15 639 | 6 686 | 22 325 | — | 1.86 x 190 |
| Camera, step 3 | 162 686 | 11.800 (8137) | 0, 0, 0, 0 | 21.0 % (21.7 %) | 310 | 9 940 | 10 250 (-54 %) | 7 609 (1 813) | 0.13 x 272 |

Host to board, unpaced 20 s, every byte checked: 162 609 sent, 162 609 received, 0 bad, 0
lost; EMAC missed frames, TX errors and underflows 0 in every bench run.

Stream (`jrx`, 10 s windows; net and send are the tasks' cycles per datagram; JPEG link-bound
ran twice, the second figures in brackets):

| Run, build | Host | Core 0 busy (`busy_all`) | net task | send task | Net + send | `direct_cyc` (headers and sum) |
|---|---|---|---|---|---|---|
| Raw 1280x800 RAW8 (link), step 2 | 90.00 Mbit/s, 10.99 fps | 63.7 % (64.2 %) | 23 138 | 8 038 | 31 176 | — |
| Raw 1280x800 RAW8 (link), step 3 | 89.96 Mbit/s, 10.98 fps | 24.4 % (25.2 %) | 605 | 10 903 | 11 508 (-63 %) | 8 110 (1 206) |
| JPEG q90, 15.5x, 153.98 Hz (link), step 2 | 92.69 (92.78) Mbit/s | 71.3 % (71.8 %) | 22 587 | 10 165 | 32 752 (32 789) | — |
| JPEG q90, 15.5x, 153.98 Hz (link), step 3 | 92.57 (92.48) Mbit/s | 31.0 % (32.0 %) | 525 | 12 231 | 12 756 (12 744), -61 % | 8 936 (1 195) |
| JPEG q90, 2x, 120 Hz, step 2 | 13.55 Mbit/s, 120.06 fps | 9.4 % (9.6 %) | 20 123 | 7 499 | 27 622 | — |
| JPEG q90, 2x, 120 Hz, step 3 | 13.62 Mbit/s, 120.06 fps | 4.0 % (4.2 %) | 385 | 10 090 | 10 475 (-62 %) | 7 452 (1 140) |

Every stream run: 0 incomplete or bad frames, 0 lost polls, 0 capture errors, 0 EMAC missed
frames, TX errors or underflows; request round trips 1.36 ms (2x) and 4.9-5.1 ms (link full;
4.6-4.9 ms before). Link-bound JPEG came out at 74.0 and 73.5 fps against 79.3 and 76.3: the
same Mbit/s, larger files (156 kB a file against 145 kB, from `encoded_bytes`) [INFERENCE:
the noise at gain 15.5x grows as the sensor warms, and this build ran after the baseline;
the baseline's own second run was slower too]. `poc-viewer`'s whole board test against this
build: 0 failed checks (`s5-stream.board.txt`), 0 EMAC TX errors in its reports.

- **Kept.** About 11 000 to 20 000 cycles a datagram less in every case; at the wire rate
  core 0 is 24-31 % busy instead of 64-71 %. smoltcp's TX path (17 000 cycles a datagram with
  the driver's part and the token requests) is off the stream's datagrams.
- The send task is now the cost: `chunk::send` takes 8 100-8 900 cycles a chunk from PSRAM,
  of which 1 200 the headers and the sum (from internal RAM); the rest is mostly the PIE copy
  reading PSRAM [INFERENCE: from `direct_cyc` less `direct_hdr_cyc`; the copy itself is not
  timed apart]. More runner wakes (10 700 against 7 200 in 10 s raw): the send task now waits
  for room on the ring, woken by the completion interrupt.

**Tried and dropped: the body sent from where it lies** (plan 7.3's zero-copy: the headers in
the first descriptor's buffer, a second descriptor pointing the EMAC's DMA at the bytes in
PSRAM; 32 TX descriptors; logs `s3-*`, `s4-*`). The EMAC's DMA reads PSRAM: the bench's
datagrams built whole in PSRAM and sent from there, 162 173 datagrams in 20 s, every byte
right (`s3-bench.psramdma.txt`). It was not kept:

- No CPU gain over the copy: the checksum then read the body from PSRAM, and cost what the
  copy costs (`direct_hdr_cyc` 5 600-7 000 against 1 100-1 800). Net + send per datagram, same
  session as its own step 2 baseline (`s3-*`): raw 11 316, link-bound JPEG 12 946, 2x 10 385;
  bench net + TX: internal 6 840, PSRAM 10 367, camera 11 590.
- A body must stay unchanged until the DMA has read it, up to 16 frames after it was queued.
  Without a wait, the board test's 1280x64 RAW8 at 1000 Hz had 66 incomplete frames
  (`s3-stream.board.txt`) [INFERENCE: the receiver wrote the next frame into the buffer
  before the last chunks went out, and the host dropped them on their checksum]. Waiting for
  the last chunk's completion before giving the buffer back fixed that
  (`s4-stream.board.txt`) and cost 0.4 % of the camera bench's rate.
- TX underflows: with the JPEG encoder working in PSRAM, the EMAC's DMA fell behind the
  wire. Link-bound JPEG at 153.98 Hz: 3 underflows in 10 s (`emac_tx_underflows`), 2
  incomplete frames, and one failed board-test check (`auto off, the scene at gain 15.5x,
  longest exposure: 1 incomplete frames`, `s4-stream.board.txt`). A 256-byte TX threshold
  (code 3 instead of 64 bytes) to ride it out: the host received no stream at all
  (`s5-ttc256-stream.*`, `jrx`: `no window` in all four runs); not pursued.

### Measured: where core 0's time goes per frame, 2026-11-11

The start of plan steps 7.5, 7.6 and 6.2. The step 3 tree (`d157059`) with counters added
(feature `accounting`): the test hooks' `P` reply has the JPEG encode's first poll (the
driver's set-up: header, write-back, invalidate of the output, descriptors, start), its
wake-up after the end (from the end of the 2D-DMA RX handler that saw the file's end to the
start of the encode future's last poll) and that last poll (the driver's end work: descriptor
read, invalidate of the file); `N` has the PIE copy of a datagram's body alone
(`direct_copy_cyc`). This measurement build also timed the capture task's whole-frame
invalidate (the driver's `wait_frame` split in two, the invalidate made and timed in the
capture task; not kept, see below). Logs `s1-b0.*` in `/tmp/p4perf-logs/StreamFrames/`; `jrx`
(its copy there also asks for `I` and `P`), 10 s windows after 2 s of warm-up. Per frame:
over the CSI DMA handler's calls.

| Run | Host | Core 0 busy (`busy_all`) | Capture task per frame (of it: invalidate), share | CSI DMA handler per frame | Encode task per frame (driver set-up / end work), share | Encode wake-up, mean (longest) | JPEG + 2D-DMA RX handlers per frame | Send task per datagram (body copy) |
|---|---|---|---|---|---|---|---|---|
| Raw 1280x800 RAW8 120 Hz (link) | 89.93 Mbit/s, 10.98 fps | 25.8 % | 33 552 (2 192), 1.0 % | 1 190 | — | — | — | 11 253 (4 883) |
| JPEG q90, 15.5x, 153.98 Hz (link) | 92.50 Mbit/s, 76.6 fps | 30.3 % | 26 346 (1 765), 1.0 % | 1 166 | 35 182 (5 824 / 4 939), 1.3 % | 21 255 = 53 µs (2.6 ms) | 768 + 282 | 12 310 (5 684) |
| JPEG q90, 2x, binned 320x200 at 120 Hz | 1.15 Mbit/s, 120.03 fps | 1.1 % | 6 207 (1 456), 0.1 % | 1 266 | 10 211 (4 003 / 1 262), 0.3 % | 2 016 = 5 µs (20 µs) | 848 + 290 | 11 551 (2 984) |
| JPEG q90, 2x, binned 320x200 at 557.9 Hz | 5.20 Mbit/s, 557.71 fps | 5.0 % | 6 198 (1 457), 0.8 % | 1 263 | 10 015 (4 033 / 1 256), 1.3 % | 2 011 = 5 µs (5 µs) | 860 + 288 | 11 627 (2 948) |

Every run: 0 incomplete or bad frames at the host, 0 capture errors, 0 frames the encoder
could not take.

- **The send task is the cost**: 22-25 % of core 0 at the wire rate, of which the body copy
  from PSRAM is 4 900-5 700 cycles a datagram. That is plan step 6.2's target.
- **The whole-frame invalidate costs 1 450-2 200 cycles (4-5.5 µs)**, not the 17-31 µs of
  `poc-p4-eth`'s measurement: 0.03 % of core 0 at 154 fps, 0.2 % at 558 fps. A frame the
  CPU has just read costs more: 10 200 cycles (25 µs) for a raw frame after it was sent
  (`s1-b1.case1`).
- **The CSI DMA handler** (the buffer swap and the re-arm) costs 1 170-1 270 cycles a frame:
  0.05 % of core 0 at 154 fps, 0.18 % at 558 fps.
- **The capture and encode tasks cost 4 to 6 times more per frame while the link is busy**
  (26 000-34 000 and 35 000 cycles against 6 200 and 10 000 at 1 to 5 Mbit/s), though they do
  the same work [INFERENCE: their code runs from flash through the L2 cache, which the body
  copies' 11 MB/s of PSRAM reads keep evicting; not measured]. Still 1.0-1.3 % of core 0
  each.
- **The encode wake-up** waits behind the send task's polls when the link is busy: 53 µs on
  average against 5 µs, the longest 2.6 ms [INFERENCE: the longest fell while `jrx` read the
  reports at the window's end; no frame was lost to it, `enc_skipped` 0].

### Plan step 7.5: whole-frame invalidate and circular capture list, 2026-11-11, not kept

**(a) No invalidate for frames only the encoder reads: tried, dropped.** The receiver's
`wait_frame` was split into a wait that returns the frame without the invalidate and an
`into_frame` that invalidates it; the capture task invalidated only the raw frames it gave
to the send task (the CPU's PIE copy reads them), not JPEG frames (the encoder's DMA reads
memory, not the caches, and the CPU writes no frame buffer, so no dirty line can reach the
pixels) or frames it dropped. Lines an earlier raw frame left in the caches stay stale and
unread until the next raw frame of that buffer, whose invalidate comes after its DMA write
ended and before the first read, also after a switch from JPEG. Same session as the
baseline above (`s1-b1.*`):

| Run, build | Host | Core 0 busy (`busy_all`) | Capture task per frame, share | Invalidates in 10 s (cycles each) | Encode task per frame |
|---|---|---|---|---|---|
| Raw 1280x800 RAW8 (link), every frame | 89.93 Mbit/s | 25.8 % | 33 552, 1.0 % | 1 203 (2 192) | — |
| Raw 1280x800 RAW8 (link), sent frames only | 89.91 Mbit/s | 26.0 % | 38 976, 1.1 % | 110 (10 192) | — |
| JPEG q90, 15.5x, 153.98 Hz (link), every frame | 92.50 Mbit/s | 30.3 % | 26 346, 1.0 % | 1 543 (1 765) | 35 182 |
| JPEG q90, 15.5x, 153.98 Hz (link), none | 92.50 Mbit/s | 30.9 % | 32 994, 1.2 % | 0 | 37 814 |
| JPEG 320x200 at 120 Hz, every frame | 1.15 Mbit/s | 1.1 % | 6 207, 0.1 % | 1 195 (1 456) | 10 211 |
| JPEG 320x200 at 120 Hz, none | 1.15 Mbit/s | 1.1 % | 4 776, 0.1 % | 0 | 10 369 |
| JPEG 320x200 at 557.9 Hz, every frame | 5.20 Mbit/s | 5.0 % | 6 198, 0.8 % | 5 602 (1 457) | 10 015 |
| JPEG 320x200 at 557.9 Hz, none | 5.22 Mbit/s | 4.8 % | 4 730, 0.6 % | 0 | 10 131 |

It removes 1 450 cycles a frame where the system is quiet (-24 % of the capture task), and
nothing that shows while the link is busy, where the capture task's own cost varies by more
than that. At most 0.2 % of core 0 (558 fps), below the 1 % this phase set as the threshold
of a visible cost, so the driver keeps one invalidating `wait_frame`.

**(b) A circular DMA list over the frame buffers: not done.** The driver re-arms the channel
in the DMA handler at every frame end (one block per frame, `arm`: 8 register writes); the
handler costs 1 170-1 270 cycles a frame, 0.05 % of core 0 at 154 fps and 0.18 % at 558 fps.
A circular list would keep one interrupt per frame (the driver must know which buffer
completed) and save part of those cycles, below the same threshold. It would also fix the
order of the buffers: the stream holds a raw frame for the 90 ms its datagrams take (about 11
frame periods) and the encoder holds one while it works, which the driver's swap handles by
choosing a free buffer each time, and a fixed ring would write into the held one. No short
frame was seen (0 capture errors in every run of this phase).

### Plan step 7.6: the encode task's wake-up, 2026-11-11, not done

The JPEG driver's 2D-DMA RX handler records the end of the file and wakes the encode task
through its waker; the task then runs in the one thread-mode executor of core 0, after the
task being polled. The `P` reply times that wake-up (above) and the encode's polls. Sessions
1 and 3 (`s1-b0.*`, `s3-stream.case6`, `case7`), 1280x800 at 153.98 Hz, encode-only (`X1`)
for the case without the link:

| Run | Host | Encode µs (mean) | Encoder busy | Wake-up, mean (longest) | Driver set-up / end work, cycles | Frames the encoder could not take |
|---|---|---|---|---|---|---|
| JPEG q90, 15.5x (link), 151 kB files | 92.50 Mbit/s, 76.6 fps | 6 086 | 93.7 % | 21 255 = 53 µs (2.6 ms) | 5 824 / 4 939 | 0 |
| JPEG q100, 2x (link), 362 kB files | 92.43 Mbit/s, 31.9 fps | 6 410 | 98.6 % | 21 174 = 53 µs (1.6 ms) | 6 291 / 6 439 | 0 |
| The same, encode-only | — | 6 284 | 96.8 % | 1 865 = 4.7 µs (5 µs) | 3 997 / 1 789 | 0 |
| JPEG q90, 2x, 320x200 at 557.9 Hz | 5.20 Mbit/s | 401 | 22.4 % | 2 011 = 5 µs (5 µs) | 4 033 / 1 256 | 0 |

While the link is full the encode takes 126 µs (2 %) longer than without it; the wake-up
accounts for 48 µs of that and the slower driver set-up and end work (cache misses beside
the send task's PSRAM reads) for 22 µs [INFERENCE: the rest is the encoder's DMA sharing
PSRAM with the copies; the driver has no hardware timestamp]. Even at 98.6 % encoder busy no
frame was lost to it (0 in every run), and the wake-up costs no CPU cycles: it is time the
core spends on the send task's poll. A handler that starts the next encode would move the
driver's set-up (4 000-6 300 cycles: header copy, two cache calls, descriptors) into an
interrupt and change the encoder's ownership for at most 48 µs a frame. Not done.

### Measured: plan step 6.2, cache prefetch of the PSRAM reads, 2026-11-11

The CPU reads PSRAM in one place: the PIE copy of each datagram's body into a TX buffer
(`net/chunk.rs`), from a camera frame, a JPEG file or the bench's PSRAM buffer, and the
RAW10-to-RAW8 selection (`stream/send.rs`). `src/prefetch.rs` wraps those reads
(`prefetch::reading`) with one of these modes (the test hooks' `A<n>`, `netbench.py tx
--prefetch n`), through `esp32p4-cache`'s `prefetch` module:

| Mode | What |
|---|---|
| 0 `off` | none (the build before) |
| 1 `l1l2` | L1 data and L2 autoload, miss+hit trigger, ascending, one section over the bytes read (the spike's best setting), on for the copy only |
| 2 `l1`, 3 `l2` | either autoload alone, miss+hit |
| 4 `l1l2_hit` | both, hit trigger |
| 5 `preload` | manual L1 data preload of the bytes read, started just before the copy |
| 6 `preload_ahead` (kept, the default) | manual L1 data preload of the next chunk's bytes (`prefetch::ahead`), started as soon as a datagram is queued, so it loads while the task waits for room on the ring and builds the next head; the copy finds the lines in L1 |

**Why no stale line reaches the CPU** (the `prefetch` module docs have the full argument).
Every PSRAM buffer the CPU reads is invalidated after the DMA's last write to it (the
receiver's `wait_frame`, the encoder's end of file, the bench's flush before each pass), and
no DMA writes it again until the reader gives it back. Prefetch runs only over the buffer
being read: autoload is on only inside `reading` (off again, `DONE` waited for, before it
returns), and the preload ahead covers the next chunk of the same buffer; the reader calls
`prefetch::settle` (waits for the preload) before it gives the buffer back, and no
invalidate runs inside `reading` (all of them are made by tasks outside it, none by an
interrupt handler). So a prefetched line is always loaded after the DMA finished the buffer,
and dropped by the buffer's next invalidate before the CPU reads it again.

Bench, same build, the modes in turn, 10 s each (`s2-bench.*`, `s3-bench.*`); cycles per
datagram:

| Source | Mode | Datagrams | Lost, dup., reordered, bad | Busy (`busy_all`) | TX task | `direct_cyc` (body copy) |
|---|---|---|---|---|---|---|
| PSRAM | off | 81 381 | 0, 0, 0, 0 | 20.8 % | 9 520 | 7 234 (4 383) |
| PSRAM | l1l2 | 81 383 | 0, 0, 0, 0 | 17.4 % | 7 871 | 5 608 (2 722) |
| PSRAM | l1 | 81 383 | 0, 0, 0, 0 | 17.4 % | 7 866 | 5 598 (2 715) |
| PSRAM | l2 | 81 383 | 0, 0, 0, 0 | 17.5 % | 7 884 | 5 620 (2 744) |
| PSRAM | l1l2_hit | 81 374 | 0, 0, 0, 0 | 20.1 % | 9 159 | 6 887 (4 009) |
| PSRAM | preload | 81 383 | 0, 0, 0, 0 | 17.0 % | 7 643 | 5 377 (2 530) |
| PSRAM | preload_ahead (`s3`) | 81 383 | 0, 0, 0, 0 | 15.0 % | 6 691 | 4 382 (1 186) |
| Camera | off | 81 318 | 0, 0, 0, 0 | 22.2 % | 10 141 | 7 711 (4 845) |
| Camera | l1l2 | 81 316 | 0, 0, 0, 0 | 18.8 % | 8 476 | 6 051 (3 163) |
| Camera | l1 | 81 315 | 0, 0, 0, 0 | 18.9 % | 8 509 | 6 066 (3 171) |
| Camera | l2 | 81 310 | 0, 0, 0, 0 | 18.9 % | 8 527 | 6 102 (3 207) |
| Camera | l1l2_hit | 81 319 | 0, 0, 0, 0 | 21.5 % | 9 824 | 7 398 (4 507) |
| Camera | preload | 81 319 | 0, 0, 0, 0 | 18.4 % | 8 263 | 5 835 (2 969) |
| Camera | preload_ahead (`s3`) | 81 310 | 0, 0, 0, 0 | 16.2 % | 7 166 | 4 701 (1 200) |

- Autoload, either cache or both, takes the copy from 4 400-4 800 to 2 700-3 200 cycles: on
  a 1 409-byte chunk the two levels do not add up as they did over the spike's whole frame,
  and the hit trigger alone gains little, as in the spike.
- Preloading the chunk being copied gains about as much; preloading the next one while the
  task waits makes the copy from PSRAM as fast as from internal RAM (1 070-1 200 cycles
  against 1 156 for the internal source, `s4-bench.internal.p6`).

The kept build (`preload_ahead` the default, the preload started after the datagram is
queued), against `off` in the same session, 20 s each (`s4-bench.*`):

| Source, mode | Datagrams | Host MB/s | Lost, dup., reordered, bad | Busy (`busy_all`) | TX task | `direct_cyc` (body copy) |
|---|---|---|---|---|---|---|
| PSRAM, off | 162 757 | 11.801 | 0, 0, 0, 0 | 20.9 % | 9 570 | 7 266 (4 411) |
| PSRAM, preload_ahead | 162 759 | 11.801 | 0, 0, 0, 0 | 14.9 % | 6 642 | 3 934 (1 071) |
| Camera, off | 162 693 | 11.801 | 0, 0, 0, 0 | 22.3 % | 10 192 | 7 752 (4 878) |
| Camera, preload_ahead | 162 695 | 11.801 | 0, 0, 0, 0 | 15.8 % | 7 016 | 4 075 (1 182) |
| Internal, preload_ahead (no prefetch: internal RAM) | 163 770 | 11.796 | 0, 0, 0, 0 | 14.1 % | 6 261 | 3 965 (1 156) |

Stream (`jrx`, 10 s windows, `s5-stream.case1` to `case6`; send and net tasks per datagram;
the raw rate's 10.98 against 10.88 fps is one frame in the window, the datagrams are the
same, 79 436 and 79 469):

| Run, mode | Host | Core 0 busy (`busy_all`) | Send task | Net task | `direct_cyc` (body copy) |
|---|---|---|---|---|---|
| Raw 1280x800 RAW8 120 Hz (link), off | 89.94 Mbit/s | 26.2 % | 11 322 | 557 | 8 270 (4 890) |
| Raw 1280x800 RAW8 120 Hz (link), preload_ahead | 89.11 Mbit/s | 19.7 % | 8 013 | 543 | 4 507 (1 088) |
| JPEG q90, 15.5x, 153.98 Hz (link), off | 92.65 Mbit/s, 39.8 fps | 31.6 % | 12 327 | 537 | 9 267 (6 034) |
| JPEG q90, 15.5x, 153.98 Hz (link), preload_ahead | 92.67 Mbit/s, 39.7 fps | 24.4 % | 8 851 | 526 | 4 869 (1 410) |
| RAW10 sent as RAW8 (link), off | 89.88 Mbit/s | 48.5 % | 21 359 | 756 | 18 101 (selection 14 238) |
| RAW10 sent as RAW8 (link), preload_ahead | 89.13 Mbit/s | 41.9 % | 18 169 | 741 | 14 898 (selection 10 999) |

(RAW10 as RAW8 preloads the packed bytes of the chunk being reduced, not the next one: the
selection's reads are not the copy's.)

Every stream run: 0 incomplete or bad frames, 0 capture errors, 0 EMAC TX errors or
underflows.

**Content under live DMA** (`s5-stream.case7`, `case8`). The sensor's rolling-bars pattern
(crop 1280x800 RAW8 at 120 Hz) changes in every frame: a band of 8 rows moves down by 8 rows a
frame over two bars rows. `jrx check=rolling` checks every whole frame: each row equals one of
the 4 distinct rows of the first frame, at most 8 band rows, so a line of an older frame of the
same buffer shows as a wrong row. With `preload_ahead`, 180 s at the wire rate: 1 967 frames,
0 failed (1 910 with the band whole, 57 with it in the blanking), 0 incomplete, 1 429 365
datagrams; with `off`, 20 s: 219 frames, 0 failed. Then `poc-viewer`'s whole board test
against this build (`preload_ahead` on; the sections `patterns`, `raw10`, `switching`,
`agreement` check pixels of raw and JPEG frames and the switch between them): 0 failed
checks, 325 s (`s5-stream.board.txt`).

- **Kept.** At the wire rate core 0 goes from 26.2 to 19.7 % with raw frames and from 31.6 to
  24.4 % with link-bound JPEG (send task 11 300-12 300 to 8 000-8 900 cycles a datagram); the
  bench's camera source from 22.3 to 15.8 %. The copy from PSRAM costs what a copy from
  internal RAM costs.
- Not measured: the preload beside a second core's cache operations (core 1 is off), and
  the detector's whole-frame reads of plan 6.2 (they are in `poc-p4-eth`).

## The Ethernet driver (esp-hal 1.2.2 `ethernet` for the bring-up, this crate's DMA rings)

Read in the registry source and measured here. The first three needed workarounds in this
crate; they are defects to take upstream. Since 2026-11-11 ([step 2](#measured-step-2-the-ethernet-ring-2026-11-11))
esp-hal only brings the EMAC up (`Ethernet::new` in blocking mode: clocks, pads, DMA reset and
bus mode, MAC configuration and address, the PHY) and keeps the PHY and the MAC's speed and
duplex; `net/eth.rs` stops its DMA, points the descriptor list registers at its own rings,
binds its own `ETH_MAC` handler and starts the DMA again. Nothing of esp-hal's API blocked
that: its blocking driver binds no handler and leaves the DMA interrupts off, and the DMA
registers are reachable through the PAC.

1. **TX store-and-forward breaks frames on the P4.** `dma_start` sets `TX_STR_FWD` (and the
   checksum insertion needs it). With it, 56-byte pings passed, 400-byte ones failed with
   ICMPv6 checksum errors at the host, 1000 and 1452-byte ones were lost; a 10 s run left the
   board at 11.77 MB/s but the host received none of 81 680 datagrams (its NIC counted 16 320
   frames, the rest never arrived; UDP checksum errors on those), and afterwards even 56-byte
   pings failed. ESP-IDF disables TX store-and-forward on every chip
   (`emac_hal_init_dma_default`, threshold 64) and states a 256-byte RX FIFO for the P4
   [INFERENCE: the TX FIFO is smaller than a full frame too]. The take-over in `net/eth.rs`
   clears the bit, sets the 64-byte threshold and flushes the TX FIFO before it starts the DMA
   on its rings.
2. **No checksum insertion then.** esp-hal sets CIC 3 on every TX descriptor and tells
   smoltcp to compute no checksum. In threshold mode the EMAC inserts none: with smoltcp left
   at that, every ping failed with ICMPv6 checksum errors at the host and the board was
   unreachable (neighbour discovery too). `net/eth.rs` leaves CIC at 0 and gives smoltcp the
   UDP and ICMPv6 checksums on TX (part of `tx_stack`). In store-and-forward mode (before the
   fix) small frames did get correct inserted checksums. RX checksums stay with the EMAC: the
   driver drops frames its checksum engine flags in RDES4.
3. **The DMA-buffer check uses the wrong RAM range.** `InternalMemory::get_mut` asserts the
   address is in esp-metadata's "DRAM" range, 0x4FF40000..0x4FFADFC0, which assumes a 256 KiB
   L2 cache. With the 128 KiB the bootloader sets (`ESP_HAL_CONFIG_L2_CACHE_SIZE`, as
   `poc-p4-eth`), RAM starts at 0x4FF20000, the storage landed in `.bss` below 0x4FF40000 and
   the first `Ethernet::new` panicked (`assertion failed: is_valid_ram_address`). `dram_dma.x`
   adds a NOLOAD section before the stack at 0x4FF40000 or above; `net` zeroes the storage
   there before use (all-zero is the value `EthernetDmaStorage::new` builds). Only the
   one-descriptor storage `Ethernet::new` still needs (3 200 B) lives there now; the rings of
   `net/eth.rs` are in `.bss`, wherever the linker puts them.
4. **`GenericPhy` busy-polls.** Its `poll_link` reads BMSR twice and ANLPAR and calls
   `wake_by_ref` every time; embassy-net calls it at the end of every runner poll, so the
   runner never sleeps (measured above: 99.8 % busy). `net/ip101.rs` reads the PHY every
   500 ms, woken by a 500 ms task. The MDC divider at a 400 MHz CPU clock is /124 = 3.2 MHz,
   above the 2.5 MHz of IEEE 802.3 clause 22 (from the source); MDIO works on this board.
5. **RX cache maintenance works.** esp-hal invalidated each RX descriptor and buffer before
   reading; 60 s at the wire limit with every byte checked found no bad byte (below). The
   rings of `net/eth.rs` read and write the descriptors through the non-cacheable alias and
   invalidate the lines of a frame the stack read before the buffer goes back to the DMA (none
   is cached while the DMA owns it); 20 s at the wire limit with every byte checked, no bad
   byte ([step 2](#measured-step-2-the-ethernet-ring-2026-11-11)).
6. Costs of esp-hal's rings, gone with the own ones: every TX descriptor asked for a
   completion interrupt (one runner wake per frame), and each token request made one or two
   ROM cache calls (about 1 200 cycles).
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
- **CPU**: at the wire rate core 0 is 20-24 % busy (raw, link-bound JPEG), 8 500-9 400
  cycles a datagram since the preload of the next chunk
  ([plan step 6.2](#measured-plan-step-62-cache-prefetch-of-the-psram-reads-2026-11-11)):
  the copy (1 100-1 400), the headers and the checksum (1 200), the descriptor and the waits.
  The detector runs on a bare core 1 beside the wire rate in `Service::BenchVision`
  ([Co-load](#co-load-the-detector-on-core-1-beside-the-wire-rate-2026-11-11)); the stream
  service itself still runs on core 0 alone ([the two-core attempt](#the-two-core-attempt)).
  Sending the bytes from PSRAM without the copy failed on TX underflows (step 3).
- Not verified: an encoder timeout (never seen; the path is the same as the driver's errors),
  the encoder's codec and DMA errors on this firmware (only `OutputTooSmall` was provoked),
  quality below 20 on the camera, a scene in daylight (the room was dark for every run).
- Keep the threshold mode and the checksum capabilities of `net/eth.rs` while the P4's EMAC
  breaks frames in store-and-forward mode; size TX socket metadata rings as in item 7 of
  [The Ethernet driver](#the-ethernet-driver-esp-hal-122-ethernet-for-the-bring-up-this-crates-dma-rings)
  and give TX rings at least twice the largest datagram (smoltcp wants it contiguous).
