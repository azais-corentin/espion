# espion-core1

The blob detector on the second HP core of the ESP32-P4, started bare: no scheduler, no
interrupts, no critical section and no logging on core 1 (performance plan step 5, shape B of
the two-core analysis). `no_std`, no heap. RISC-V only (`riscv32imafc-unknown-none-elf`).

Core 1 polls the strip reader of `esp32p4-csi` (`StripReader`), runs the detector of
`espion-vision` (or the pixel check) on each strip and publishes one result per frame through
a lock-free queue. Core 0 keeps the strip receiver's control half, the network and everything
else, and watches core 1 through atomics.

Used by `poc-p4-eth` (`RUN_CORE1`: bring-up, measurements, fault tests) and `poc-p4-stream`
(`Service::BenchVision`: core 1 detecting while core 0 sends at the wire rate).

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
  strip reader (`poll`, `release`), `esp32p4-cache` (invalidate, write-back, the L1 data
  autoload registers), the detector, PIE through core 1's own token, and with feature
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
| `start(cpu, det, scratch, skip) -> Option<&'static mut Core1>` | core 0, once | Starts core 1 (`CpuControl::start_app_core`, 16 KiB static stack) with the detector `det`, a blob scratch for frames whose result could not be queued, and the dark skip (`SkipChoice::Pie` or `Scalar`). The `AppCoreGuard` lives in a static and is never dropped (dropping it parks core 1, possibly inside the cache lock) |
| `Core1::hand_over(CsiStrips) -> Option<StripControl>` | core 0 | Splits the strip receiver, copies the `StripReader` into core 1's two-slot mailbox (core 1 uses it in place) and returns the control half. `Core1::can_post`, `Core1::wait_mailbox(timeout)` (async) before a second one |
| `Core1::set_run(&Run)` | core 0, strip path stopped | The next run: `Job::Detect` (width, RAW8/RAW10, threshold, late-strip limit, L1 data autoload over each strip, optional synthetic strip) or `Job::Check` (reference row checksums, row bytes, the L1 data autoload, the test holds) |
| `Core1::note_stop()` | core 0 | After every `StripControl::stop`: the check tells strips of a stopped run from bad ones |
| `Monitor` (`new`, `poll`, `second`, `end`) | core 0 | Every millisecond `poll`: `StripControl::service()` and the drain of the frame results. Once a second `second` (async): core 1's trap record and heartbeat, then the log lines (fps, detect mean and max, instructions per frame, late strips, worst strip start latency, lapped strips, abandoned frames, blobs, dropped results, core 0's time in `poll`, heartbeat; with feature `stats` the cache lock's operations and longest hold and wait per core), the executor getting a turn after each line. `end`: the run's totals |
| `log_summary`, `report_trap`, `report_dead` | core 0 | The strip path's counters at the end of a run (and the check's verdict); a trap or a stalled core 1 |
| `heartbeat`, `abandoned`, `trap`, `skip_used`, `CHECK`, `CHECK_MAX`, `HOLD_TIMES` | core 0 | Core 1's counters |
| `FrameResult`, `Core1::pop_with`, `Core1::dropped` | core 0 | One frame's blobs (up to 256), detector counters, cycles and instructions, late strips and latencies; read in place |
| `row_sum` | both | The row checksum the check compares; core 0 computes the reference with it |
| `spsc` | both | Single-producer single-consumer queue on two atomic indices, overwrite-never (a full queue counts a drop), items filled and read in place |
| `markers::Marker::new(name, pin, core, channel)` | core 0, feature `markers` | Routes dedicated-IO channel `channel` of `core` (either core) to `pin` through the GPIO matrix (`OutputSignal::CPU_GPIO_n`, core 0's channels are signals 214 to 221, core 1's 222 to 229), with the pad's input path and its pull-down on. The driving core enables the channel itself (core 1: its entry, for `MARKER_STRIP` and `MARKER_FRAME`) |
| `markers::self_check(&mut [Marker])` (async) | core 0, feature `markers` | Logs the cost of one toggle, listens on every marker pad (GPIO interrupt on any edge, bound on core 0 only), waits for a run to toggle them (every marker, or 5 s after the first edge), counts each pad's level changes read back from `GPIO_IN` for 1 s, then waits until the markers rest for 20 ms (between runs: its log lines hold core 0 for about 3 ms) to stop listening and log the counts |
| `MARKER_STRIP`, `MARKER_FRAME` | both | Core 1's channels: toggled when the reader has handed out a strip of a detector run (before the detector) and when a frame's blobs are final (result published, or dropped on a full queue) |

Core 1's loop: take a new reader from the mailbox if core 0 posted one, answer a clock
request (`Core1::clock_offset`), poll; on a strip run the job and release it; between empty
polls wait 10 us (polls share the L2MEM bus with the DMA). Its heartbeat counts the turns.

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
