# poc-p4-stream

A throwaway proof of concept on the Waveshare ESP32-P4-ETH dev board (the board of
[`poc-p4-eth`](../poc-p4-eth/README.md)): the OV9281 camera streamed live over Ethernet to
[`poc-viewer`](../../host/poc-viewer/README.md), with every camera setting taken from it (the
camera stream of `espion-proto`, `docs/protocol.md`), on Ethernet and IPv6 with esp-hal's EMAC
driver and embassy-net. Not part of the product.

The `Stream` service (the default) streams raw frames; the `Bench` service (the network test
it was built on) stays selectable.

- Addressing: IPv6 only, the fixed link-local address `fe80::4553:9281/64`, neighbour discovery
  by smoltcp; no IPv4, no DHCP, no SLAAC. The MAC is the chip's factory MAC from eFuse
  (`80:f1:b2:da:e4:67` on the bench board): Espressif's OUI, unique per chip, so several boards
  on one switch never collide; the P4 has no radio that uses it too.
- The host always talks first (its firewall drops unsolicited UDP): the board sends only to the
  address and port a request came from.
- Status: the camera streams at the 100 Mbit/s wire rate in every mode the `ov9281` driver
  accepts, the viewer's board test passes against it, and a 10-minute soak with mode changes,
  viewer restarts and malformed datagrams ran without a crash or stall (see
  [The camera stream](#the-camera-stream)). The network alone sends and receives at the wire
  limit with every byte checked (see [Measured results](#measured-results)). esp-hal's
  Ethernet driver needs three workarounds on the P4 (see [The Ethernet driver](#the-ethernet-driver)).

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
python3 firmware/poc-p4-stream/tools/streamdiag.py report   # then report
```

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

1. Switches UART0 to 2 Mbaud, prints the chip, brings up PSRAM (250 MHz).
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
the socket buffer; from the driver wrapper (`net/device.rs`), per frame, the cycles in smoltcp's
part of the TX and RX tokens (`tx_stack_cyc`: headers, the payload copy into the DMA buffer and
the UDP checksum) and in the driver's part (`tx_driver_cyc`: descriptor, cache write-back, poll
demand), and the token requests (`token_cyc` each); from the executor's trace hooks
(`src/cpu.rs`), the share of time the executor was busy and each task's cycles per datagram;
and the EMAC's DMA status, missed-frame counter and MAC debug register.

| File | What |
|---|---|
| `src/main.rs` | Boot: log baud, chip, PSRAM, the network, the service selected by `SERVICE`, the RAM report |
| `src/net/mod.rs` | The network module: pins and their sources, PHY reset, EMAC driver, TX threshold mode, the DMA storage in its linker section, embassy-net with the static IPv6 address, the runner and link tasks, EMAC register snapshot |
| `src/net/ip101.rs` | IP101 PHY driver (`Phy` trait): link polled every 500 ms, speed and duplex from ANAR and ANLPAR, the last report for the log |
| `src/net/device.rs` | `embassy-net-driver` wrapper around esp-hal's driver: per-frame cycle counts, and the checksum capabilities (UDP and ICMPv6 computed by smoltcp) |
| `src/cpu.rs` | Cycle counter and per-task CPU time from the executor's `trace` hooks |
| `src/camera.rs` | OV9281 and CSI receiver: set-up, any mode's start (lanes checked in their stop state before stream-on), controls, stop, the newest frame, the fault switch of the stream's error-path test |
| `src/stream/mod.rs` | The camera stream service: the shared stream state, the control task (requests, all-or-nothing `SetCamera`, repeats), the hand-off to the capture and send tasks |
| `src/stream/check.rs` | The `SetCamera` checks in the protocol's order, as the viewer's fake camera makes them |
| `src/stream/capture.rs` | The capture task: frames, counters, the stream rules, mode changes and recovery |
| `src/stream/send.rs` | The send task: answers and frame chunks, encoded straight into the socket's ring |
| `src/stream/diag.rs` | The stream's test hooks on UDP 5010: CPU report, fault injection, stall, log marker |
| `tools/streamdiag.py` | Host side of the stream's test hooks |
| `src/bench.rs` | The test service: TX runs from internal RAM, PSRAM or the camera, the RX check, the reports |
| `dram_dma.x` | Linker section for the Ethernet DMA storage at 0x4FF40000 or above (see below) |
| `tools/netbench.py` | Host side of the test service |

## The camera stream

`src/stream/`: the protocol of `espion_proto::cam` (`docs/protocol.md`, "Camera stream",
version 2) on UDP 5009, behaving as the viewer's fake camera (`host/poc-viewer/src/fake.rs`,
the reference) does, with the real sensor.

- **Requests.** `GetState` and `Poll` answer the full state; `SetStream` checks the encoding
  against the format; `SetCamera` checks, in the protocol's order, the `ov9281` builder (size,
  alignment, window, rate), the receiver's geometry (`esp32p4-csi`'s rules), the encoding
  against the format, exposure (1..=the mode's longest) and gain (16..=248), and applies nothing
  unless all pass; the answer is the state or the error with its values. Start-up settings are
  the fake's: 1280x800 RAW8 crop, 120 Hz +- 0.5, exposure 200 lines, gain 2x, stream on, RAW8, no
  limit. Every valid request makes its source the destination and restarts the 3 s stream
  timeout; anything else on the port is counted and dropped. A repeat of the last request
  (same number, op and source, which the host sends while it waits) gets that request's result
  and is not executed again.
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
  counted, and goes out only while a host polls and the stream is on: one in flight (a frame
  completing meanwhile counts as skipped, as do frames the receiver overwrote before the task
  took them), `max_fps` honoured (frames held back by it are not counted), the chunks in order
  on the 1409-byte grid to the last request's source. RAW10 goes out packed, or as RAW8 (the
  first 4 bytes of every 5, taken as each chunk is copied). Capture errors count short frames,
  the receiver's DMA, CSI host and bridge error interrupts, frames the receiver dropped for lack
  of a buffer, and frames that did not come within two periods (the camera is then restarted).
- **Where it runs.** All on core 0, in the one embassy executor, with the network: control,
  capture, send, diag, the runner and the link task; the EMAC and CSI interrupts are on core 0
  too. The send task encodes each chunk straight into the socket's TX ring (`send_to_with`),
  the one CPU copy of the pixels; smoltcp copies the datagram into the DMA buffer with its
  checksum pass. Core 1 stays off (below: the two-core attempt hung).
- **RAM** (boot log): 182 272 B of internal RAM static, of which the service's sockets and
  buffers are 22 740 B (camera socket: 8 x 1452 B TX ring, 4 KiB RX ring; the hooks' socket);
  399 296 B left for the main stack. PSRAM: the four frame buffers, 5 144 576 B.

### Measured

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
though: about 45 000 cycles per datagram on one core. The copies were not the limit, so none
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

## For the next step (JPEG frames)

- **The hook**: `stream::capture`'s `Capture::frame` decides that a frame goes out and hands a
  `stream::Job` to the send task (`JOB`, one in flight). The send task sends the first `len`
  bytes of the job's buffer with the job's `encoding`, `width` and `height`, never assuming
  width x height, chunk by chunk straight into the socket's TX ring: one CPU copy from the
  buffer to the wire. An encoded frame goes in its own buffer in place of the captured
  `esp32p4_csi::Frame` (today the job holds the frame itself, which keeps the receiver buffer
  until the send ends; with 4 buffers the receiver still has two to write).
- **Encoding a frame**: the capture task would, instead of handing the frame on, keep it
  (16-byte aligned, straight from the receiver's PSRAM buffer), `encode` it into a PSRAM output
  buffer (128-byte aligned) and hand that buffer on with its length; two output buffers in
  turn, so the one being sent is never written. The encoder's interrupts go to the core that
  creates it: core 0 here, like everything else.
- **CPU**: at the wire rate core 0 is 91-97 % busy, almost all of it per datagram (about
  46 000 cycles: smoltcp, the driver, the chunk copy). A 45 kB JPEG at 154 fps is about 4 800
  datagrams/s [INFERENCE: computed], about 60 % of the wire rate's datagrams, so about 55 % of
  core 0 [INFERENCE] plus the encoder's own work (hardware, async).
- **Protocol and viewer** need a new `Encoding` variant (a version bump, `docs/protocol.md`),
  its `frame_bytes` no longer fixed by the size (`FrameChunk::well_formed` checks
  `frame_len == encoding.frame_bytes(width, height)` today, so a JPEG chunk would be refused),
  and a decoder in the viewer's display path.
- **Two cores**: see [The two-core attempt](#the-two-core-attempt) before moving anything to
  core 1.
- Keep `net/device.rs`'s checksum override and `net::tx_threshold_mode` until esp-hal fixes the
  driver; size TX socket metadata rings as in item 7 of [The Ethernet driver](#the-ethernet-driver)
  and give TX rings at least twice the largest datagram (smoltcp wants it contiguous).
