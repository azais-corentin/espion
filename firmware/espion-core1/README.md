# espion-core1

The blob detector on the second HP core of the ESP32-P4, started bare: no scheduler, no
interrupts, no critical section and no logging on core 1 (performance plan step 5, shape B of
the two-core analysis). `no_std`, no heap. RISC-V only (`riscv32imafc-unknown-none-elf`).

Core 1 polls the strip reader of `esp32p4-csi` (`StripReader`) and runs one job on each
strip: the detector of `espion-vision`, the pixel check, or the stream job, which also copies
each strip into a frame buffer in PSRAM (`esp32p4_csi::DmaCopy`), feeds the copied rows to
the hardware JPEG encoder while the frame comes in (`esp32p4_jpeg::live`), and measures the
frame's blobs once it is assembled. It publishes one result per frame through a lock-free
queue.
Core 0 keeps the strip receiver's control half, the network and everything else, and watches
core 1 through atomics.

Used by `poc-p4-eth` (`RUN_CORE1`: bring-up, measurements, fault tests) and `poc-p4-stream`
(`Service::BenchVision`: core 1 detecting on strips while core 0 sends at the wire rate;
`Service::Stream`: the stream job on every frame of the modes it runs on strips; that crate's
README, "Detection on core 1").

## Rules for code on core 1

Everything reachable from core 1's entry (`runner::entry`, run by esp-hal's
`start_core1_run`) must keep to these:

- **No `critical_section`.** esp-hal's critical section on this chip is one spinlock shared
  by both cores: a core 1 that traps or is parked while holding it freezes core 0 at its next
  critical section.
- **No `log`, `esp-println`, esp-rtos, embassy or esp-hal driver call.** The logger takes a
  lock with core 0 and prints at 2 Mbaud with interrupts masked; esp-rtos and embassy-time
  take the scheduler lock; esp-hal's `SoftwareInterrupt::raise` spins on a bit the other core
  may already have cleared (the two-core analysis, C1).
- **No interrupts.** The entry clears `mstatus.MIE` once; no peripheral interrupt is bound to
  core 1 (every driver is created on core 0).
- **Cache maintenance only through `esp32p4-cache`**, whose cross-core lock serialises the
  ROM's cache-sync calls of both cores.
- **Allowed:** atomics, `mcycle`/`minstret` and the event counters (`esp32p4-perf`), the
  strip reader (`poll`, `release`), the copier (`DmaCopy::start`, `poll`, `wait`, `abort`,
  `is_busy`: register accesses only), `esp32p4-cache` (the strip's invalidate, the frame
  buffer's invalidate, the write-back of a synthetic strip, the L1 data autoload registers),
  the detector, the background sample (`espion_vision::auto`) and the refined measure
  (`espion_vision::refine`), the live JPEG encode's input side (`esp32p4_jpeg::live`:
  `begin`, `Feeding::append`, `abandon`: atomics, descriptor writes through the non-cacheable
  alias, 2D-DMA and JPEG register writes), PIE through core 1's own token, and with feature
  `markers` core 1's dedicated-IO CSRs (`esp32p4_perf::marker`: `cpu_gpio_oen` 0x803 once at
  the entry, `cpu_gpio_out` 0x805 per marker).
- **Traps** go to the crate's own vector (`core1_trap`, in RAM, no stack): it stores
  `mcause`, `mepc` and `mtval` and waits for ever; core 0 reports them. esp-hal's default
  path would `panic!` and print from core 1.
- **PIE:** core 1 enables its own token at its entry (`Pie::enable`: per-core flag, per-hart
  CSR 0x7F2). A `Pie` is neither `Send` nor `Sync`, so core 0's token cannot reach core 1.
- **Check every build:** walk the call graph from the entry (below); it must reach no
  reference to `CRITICAL_SECTION`, `SCHEDULER`, `esp_println`, `raise`, `log::`, esp-rtos or
  embassy, no indirect call, and no ROM cache call outside `esp32p4_cache`.

## API

| Item | Side | What |
|---|---|---|
| `start(cpu, det, scratch, skip, copy) -> Option<&'static mut Core1>` | core 0, once | Paints core 1's 16 KiB static stack, then starts core 1 (`CpuControl::start_app_core`) with the detector `det`, a blob scratch for frames whose result could not be queued, the dark skip (`SkipChoice::Pie` or `Scalar`) and `copy`, the `DmaCopy` of the stream job (`None`: that job copies nothing). The `AppCoreGuard` lives in a static and is never dropped (dropping it parks core 1, possibly inside the cache lock) |
| `Core1::hand_over(CsiStrips) -> Option<StripControl>` | core 0 | Splits the strip receiver, copies the `StripReader` into core 1's two-slot mailbox (`Core1::post`; core 1 uses it in place) and returns the control half. `Core1::can_post`, `Core1::wait_mailbox(timeout)` (async) before a second one |
| `Core1::set_run(&Run)` | core 0, strip path stopped | The next run: `Job::Detect` (width, RAW8/RAW10, threshold, late-strip limit, L1 data autoload over each strip, optional synthetic strip) or `Job::Check` (reference row checksums, row bytes, the L1 data autoload, the test holds) |
| `Core1::set_stream(&StreamRun)` | core 0, strip path stopped | The next run is the stream job: width, height, format, row bytes, rows of a full strip, the frame buffers (`FrameBuffers`: base, stride, count up to `MAX_FRAME_BUFFERS` = 8, bytes per frame), late-strip limit (one strip period), `budget_cycles`, the L1 data autoload, a run number echoed in the results |
| `Core1::set_detect(&StreamDetect)` | core 0, any time | The stream job's detection settings, taken at each frame's first strip: on or off, threshold (clamped to the format), area bounds, most blobs measured (up to `MAX_SPOTS` = 128), a serial echoed in the results |
| `give_frame(index)` | core 0 | Puts a frame buffer in the stream job's free set: each buffer once at the start, then each one a result handed over, once core 0 is done with it. A buffer the live JPEG encode still reads goes back once that encode ended too |
| `release_encoder(index)`, `live_frame() -> LiveFrame` | core 0 | The live JPEG encode's hold on the frame buffer core 1 bound to it, ended once the encode ended or was cancelled; the bound frame: run, frame number, buffer, and in core 1's cycles its first and last feed and its capture (the last strip's bound) |
| `Core1::abort_stream() -> u32`, `Core1::stream_idle(req)` | core 0 | Asks core 1 to drop what the stream job holds (below); `stream_idle` says it has: no buffer held, no copy running |
| `Core1::note_stop()` | core 0 | After every `StripControl::stop`: the check tells strips of a stopped run from bad ones |
| `Core1::clock_offset(max_cycles)` | core 0 | Core 1's `mcycle` against core 0's, best of 16 requests; `age(then)`: cycles since a core-1 `mcycle` reading, as of core 1's last loop turn |
| `Core1::stack_unused()` | core 0 | Bytes of core 1's stack never written since `start` painted it |
| `Monitor` (`new`, `poll`, `second`, `end`) | core 0 | Every millisecond `poll`: `StripControl::service()` and the drain of the frame results. Once a second `second` (async): core 1's trap record and heartbeat, then the log lines (fps, detect mean and max, instructions per frame, late strips, worst strip start latency, lapped strips, abandoned frames, blobs, dropped results, core 0's time in `poll`, heartbeat; with feature `stats` the cache lock's operations and longest hold and wait per core), the executor getting a turn after each line. `end`: the run's totals. Used with `set_run`'s jobs |
| `log_summary`, `log_check_second2`, `report_trap`, `report_dead` | core 0 | The strip path's counters at the end of a run (and the check's verdict); a trap or a stalled core 1 |
| `health(&mut last_heartbeat) -> Health` | core 0, about once a second | Core 1's trap record, then whether its heartbeat moved since the last call; a trap or a stall is logged (`Alive`, `Trapped`, `Stalled`) |
| `heartbeat`, `abandoned`, `copy_counts`, `trap`, `skip_used`, `CHECK`, `CHECK_MAX`, `HOLD_TIMES` | core 0 | Core 1's counters; `copy_counts`: stream frames not copied (no free buffer) and failed copies |
| `FrameResult`, `Core1::pop_with`, `Core1::dropped` | core 0 | One frame's blobs (up to `MAX_BLOBS` = 256), detector counters, cycles and instructions, late strips and latencies, threshold and format; for the stream job also the run number, the frame buffer handed over (`NO_BUFFER` if not copied), `detected`, `cut`, the serial, the background (`Option<Background>`), each kept blob's refined measure (`spots`, `spot_blob`), the refine cycles (the frame end's invalidate and every blob's measure, over all its loop turns), per strip the hold, the detector's time and the wait for the copy, the lead (core 1's start on the first strip to the last strip's bound, 0 if later), the last strip's hold, whether the live JPEG encode got every row (`jpeg`) and its longest feed after a strip (`feed_max`); read in place |
| `row_sum` | both | The row checksum the check compares; core 0 computes the reference with it |
| `spsc` | both | Single-producer single-consumer queue on two atomic indices, overwrite-never (a full queue counts a drop), items filled and read in place (`push_with`, `pop_with`); a slot can be reserved, filled over several steps and published later (`reserve`, `reserved`, `commit`) or given up (`cancel`) |
| `markers::Marker::new(name, pin, core, channel)` | core 0, feature `markers` | Routes dedicated-IO channel `channel` of `core` (either core) to `pin` through the GPIO matrix (`OutputSignal::CPU_GPIO_n`, core 0's channels are signals 214 to 221, core 1's 222 to 229), with the pad's input path and its pull-down on. The driving core enables the channel itself (core 1: its entry, for `MARKER_STRIP` and `MARKER_FRAME`) |
| `markers::self_check(&mut [Marker])` (async) | core 0, feature `markers` | Logs the cost of one toggle, listens on every marker pad (GPIO interrupt on any edge, bound on core 0 only), waits for a run to toggle them (every marker, or 5 s after the first edge), counts each pad's level changes read back from `GPIO_IN` for 1 s, then waits until the markers rest for 20 ms (between runs: its log lines hold core 0 for about 3 ms) to stop listening and log the counts |
| `MARKER_STRIP`, `MARKER_FRAME` | both | Core 1's channels: toggled when the reader has handed out a strip of a detector or stream run (before the detector) and when a frame's blobs are final (result published, or dropped on a full queue) |

Core 1's loop: take a new reader from the mailbox if core 0 posted one (the frame in work is
dropped), answer a clock request, handle an abort request, poll; on a strip run the job and
release the strip; without one, measure the next blob of a stream frame waiting for its
measure, or wait 10 us (polls share the L2MEM bus with the DMA). Its heartbeat counts the
turns.

### The stream job

- **First strip of a frame** (row 0): the detection settings of `set_detect`, a buffer
  claimed from the free set (none free, or no copier: the frame is detected but not copied,
  `NO_BUFFER`), the detector and the background sample begun. A frame still open is
  abandoned.
- **Strip age** counts from a lower bound of the end of the strip's block (`strip_bound`):
  the reader's (`StripRef::queued_at_cycles`), or, if later, the previous strip's bound plus
  this strip's rows at 15/16 of the line time (from the late-strip limit). Time the strip lay
  done before core 1 took it counts against its budget.
- **Each strip:** the copy of the strip to its rows in the frame buffer started first (one DMA
  block). With detection on, the background sample over the whole strip, then the detector in
  one bounded call (`push_rows_until`, `espion-vision` README) while the copy runs: it stops
  at a row boundary once the strip's age passes `budget_cycles` less the detector's longest
  step, the larger of one worst-case row (width x 144 cycles) and one skip over the strip (1
  cycle a byte). Stopped short, detection stops for the rest of the frame; the frame is still
  copied, the background sample still takes its other strips (so the result has the whole
  frame's background, for core 0's automatic threshold) and its result says `cut`. Once the
  strip is past half its budget the detector's stop check also polls the copy. Then the wait
  for the copy (at most 4 ms; a failed copy counts in `copy_counts` and the frame goes out as
  not copied) and the release.
- **JPEG feed**, after the release (never inside the hold): if core 0 armed a live encode
  (`esp32p4_jpeg::Encoder::arm`) and the frame has its buffer and no failed copy, core 1
  binds the encode to the frame at whichever strip it is on (`live::begin`; a frame of
  another size or RAW10 is given up at once) and appends every whole 8-row band copied so
  far, then each strip's; the last strip appends the rest of the frame. One band slot is kept
  for the end: with one left, bands wait for the last strip. While the encode is bound its
  buffer has two holders, core 0's result and the encode: the second to let go puts it back
  in the free set (`give_frame`, `release_encoder`).
- **Release:** a strip whose copy ended while it was still intact (`StripRef::intact`) is
  released with `release_copied`. If the DMA lapped it after that (`Released::Lapped`), the
  frame goes on, without detection for the rest of it (`cut`). Any other strip released
  `false` (lapped, or the run stopped) abandons the frame and its buffer goes back; a live
  JPEG encode bound to it is abandoned too (`live::abandon`), and core 0 counts the frame as
  skipped by the encoder.
- **Last strip:** a result slot reserved in the queue (a full queue: counted as dropped, the
  buffer goes back), the detector's `finish`, the background, the frame buffer's cache lines
  invalidated. If the frame was detected and has blobs, the kept ones (area bounds, at most
  the settings' limit) are measured on the frame (`espion_vision::refine`), one per loop turn
  between strips; the result is published after the last, else at once. Publishing hands the
  buffer to core 0.
- **A frame still being measured when the next frame ends** is published first, with the
  blobs measured so far: measuring the rest there would hold the new frame's strips past
  their budget. Core 0 counts the rest as dropped.
- **Abort** (`abort_stream`, and a new reader): between two strips or two blobs core 1 waits
  for the copy in flight (at most 4 ms), drops the frame being assembled and the one being
  measured (its reserved slot given up, a live encode bound to it abandoned), gives their
  buffers back, then acknowledges.

The L1 data autoload registers exist once for the chip (the L1 data cache is shared): while a
run has the autoload on, core 1 owns them and the firmware on core 0 must not write them.
Core 1 sets one section over the strip it reads and turns it off (waiting until no fetch is
in flight) before it releases the strip.

## Features

`stats`: forwards `esp32p4-cache`'s `stats` (per-core counts and maxima of the cache lock),
and the monitor logs them.

`markers`: core 1's entry enables its channels `MARKER_STRIP` and `MARKER_FRAME`
(`esp32p4_perf::marker::enable`) and its loop toggles them (`csrr`, `xor`, `csrw` of 0x805:
no memory access, no lock); the module `markers` (core 0) routes channels to pads and checks
them at boot. Without the feature the runner's code is unchanged (`mark` is empty). Pins,
wiring and capture: the binaries' READMEs, "Marker pins".

## Build and test

```sh
cargo build --release && cargo clippy --release     # in this directory (target in .cargo/config.toml)
cargo build --release --features markers && cargo clippy --release --features markers
rustc --edition 2024 --test src/spsc.rs -o /tmp/spsc-test && /tmp/spsc-test   # host tests of the queue
```

The call-graph walk: disassemble the firmware ELF with `llvm-objdump`, follow the direct
calls and tail jumps from `start_core1_run::<espion_core1::runner::start::{closure#0}>`, and
list every reachable function, CSR access, ROM call, data reference and panic call site (the
script of 2026-11-11 is `/tmp/p4perf-logs/TwoCore/walk.py`, not in the repository).

## Verified

- Host: `spsc`'s 4 tests pass (`rustc --test`, 2026-11-11).
- `poc-p4-eth`, `RUN_CORE1 = true`, after the move (2026-11-11,
  `/tmp/p4perf-logs/CoLoad/eth-bringup.log`): four 120 fps modes, 3600 frames each, nothing
  dropped or lapped, detect 855 / 865 us at 1280x800 as before the move (849 / 860). Walk
  (`eth-bringup.walk`): 24 reachable functions with the panic leaves, no banned reference,
  31 panic call sites.
- `poc-p4-stream`, `Service::BenchVision` (2026-11-11, `/tmp/p4perf-logs/CoLoad/co1.log`,
  `co2.log`): beside the bench at the wire rate, 0 late, lapped or dropped; 10 min of pixel
  check under PSRAM TX, 0 bad rows of 57 824 000. Walk of that build (`co1.walk`): 24
  reachable functions, no banned reference, 36 panic call sites. Figures in that crate's
  README, "Co-load".
- Earlier checks of the same code in `poc-p4-eth` (trap, stalled heartbeat, laps, stalls,
  stop/start cycles, cache contention): `firmware/poc-p4-eth/README.md`, "Detector on core
  1"; not repeated after the move.
- Feature `markers` (2026-11-11, `/tmp/p4perf-logs/Markers/`): the walk of `poc-p4-eth`
  (`RUN_CORE1`, `eth-core1-on2.walk`) and of `poc-p4-stream` (`Service::BenchVision`,
  `stream-bv-on2.walk`) with the feature on: 24 reachable functions each, no banned
  reference, 31 and 36 panic call sites as before; the only new CSR accesses are `csrc 0x803`
  at the entry and `csrr`/`csrw 0x805` in the two `run` instances. With the feature off, the
  runner's `run` instances, `start_core1_run`, `core1_trap` and the detector are
  instruction-for-instruction those of the build before the change (llvm-objdump of both
  ELFs). On the board, the self-check read back 1561 edges of `MARKER_STRIP` and 120 of
  `MARKER_FRAME` in 1 s at 1280x800 RAW8 120 fps, none merged (`eth-core1-on2.log`); detect
  mean 754 us with and without the feature (max 763 and 756 us). Figures in the binaries'
  READMEs, "Marker pins".
- `FrameResult::lead` and `last_hold` (2026-11-21, `/tmp/p4perf-logs/DetectTiming/`): the
  walk of `poc-p4-stream`'s release build (`stream-new.walk`; `stream-markers-new.walk` with
  feature `markers`) against the build before the change (`stream-base.walk`,
  `stream-markers-base.walk`): 37 reachable functions each, no banned reference, no indirect
  call, the same functions, data references, CSR accesses and ROM calls; 72 and 73 panic call
  sites, one fewer than before in both. Not run on the board.
- The live JPEG feed (2026-11-21, `/tmp/p4perf-logs/LiveJpeg/`): the walk of
  `poc-p4-stream`'s release build (`stream-new.walk`; `stream-markers-new.walk` with feature
  `markers`): 38 reachable functions each, the one new being `runner::feed_jpeg` (data:
  `esp32p4_jpeg::live`'s state and tag, the run's `LIVE` record and merged globals; one
  `mcycle` read; the 2D-DMA and JPEG registers by address), no banned reference, no indirect
  call, no new ROM call; 74 panic call sites in both, two and one more than before (a slice
  index in the `run` instances). On the board: every live encode the same bytes as the frame
  encoded whole afterwards, 200 frames in each of six strip modes (`poc-p4-stream` README,
  "Encoding during the readout").
