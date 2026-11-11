# esp32p4-csi

MIPI CSI-2 receiver for the ESP32-P4 (`no_std`, on the `esp32p4` PAC, written in esp-hal
style), for the OV9281 in RAW8 or RAW10 over two lanes. Two drivers share the hardware, one
at a time:

- `Csi`: whole frames into a few frame buffers (PSRAM or internal RAM), one DMA block per
  frame.
- `CsiStrips`: every frame as a sequence of strips of a fixed number of rows in a small ring
  of internal-RAM buffers, so a row-incremental consumer (the blob detector) works on a frame
  while the sensor still sends it. The frame-boundary method, stall recovery and measured
  interrupt cost are in the `strips` module docs.

Cache maintenance goes through `firmware/esp32p4-cache` (one cross-core lock for every cache
operation).

## Strips on two cores: `CsiStrips::split`

Run on the board on 2026-11-11 with the reader on a bare core 1 (`poc-p4-eth`,
`RUN_CORE1`); results under "Board checks" below. Items 7's network part and 8's wire-rate
load are left for the co-load step.

`split(self)` turns a stopped (or running: it stops it) `CsiStrips` into two halves:

| Half | Where | What |
|---|---|---|
| `StripControl<'d>` | the core that created the driver (core 0) | `start`, `stop`, `set_config`, `is_running`, `stats`, `strips_lapped`, `reset_stats`, `debug_status`, `phy_status`, and `service()`: restarts a halted channel (stall or DMA error) once the reader has caught up; call it about every millisecond |
| `StripReader` (`Send`) | moved once to the bare second core (core 1) | `poll() -> Option<StripRef<'_>>`, never waits; `is_running()` |
| `StripRef<'r>` | core 1 | derefs to the strip's bytes; `frame()`, `first_row()`, `rows()`, `is_last()`, `queued_at_cycles()`; `release() -> bool` (also run by its drop) |

Interrupts after the split: the per-strip `BLOCK_TFR_DONE` signal is masked, so core 0 takes
no interrupt per strip. The DMA error and stall interrupt, the CSI host and the bridge error
interrupts stay bound on core 0.

### How the reader finds finished strips

It polls each list item's `LLP_STATUS[62]` (`BLOCK_TFR_DONE`, written last by the DMA's
write-back, TRM figure 7.5-6) through the non-cacheable L2MEM alias, in ring order, starting
at the slot it expects next. It clears the bit when it makes the item valid again, before the
DMA can reuse it. A set bit stays set until the reader clears it (no miss); the DMA sets it
again only after writing that item's next block (no double report). The channel registers
(`LLP0`, `DAR0`) were not used: they tell where the DMA is, not which blocks finished since
the last look, and only core 0 touches channel registers.

Each block is classified exactly as the interrupt path does it (frame end = short block,
probation frame after damage). Damage comes from the bridge's raw interrupt status, read by
the reader, and from a loss counter the core-0 error handlers raise. As on one core, a CSI
host error that core 0 has not handled yet can let one damaged strip through.

### Rules for the reader's core

- Every function the reader reaches is free of `critical_section`, `esp_println`/`log`,
  esp-hal driver calls and esp-rtos/embassy primitives, and allocates nothing. It uses
  atomics, volatile accesses to the list items, one read of the bridge's raw interrupt status,
  `mcycle`, and `esp32p4-cache`.
- The one interrupt-mask operation it reaches is inside `esp32p4-cache`'s lock: `MIE` cleared
  for the hold and restored as it was, so nothing changes on a core that runs with interrupts
  off.
- esp-hal's `critical_section` on this chip is a single lock shared by both cores. A bare core
  that panics or is parked while holding it freezes the other core at its next critical
  section. So the reader's core must never take it, and the code around `poll` must follow
  the same rules.
- `poll` writes list items only while it is marked busy. Poll at a fraction of the strip
  period: polling shares the L2MEM bus with the DMA.
- Checked in a `poc-p4-eth` release build with a probe calling `poll`, `release` and the
  drop (2026-11-11, not committed): the reader reaches `StripReader::poll`,
  `StripReader::finish`, `Tracker::complete` and the ROM's `memset`,
  `Cache_Get_L2_Cache_Line_Size` and `Cache_Invalidate_Addr`; no indirect call, no panic
  path, no reference to `CRITICAL_SECTION`, `SCHEDULER` or `esp_println::LOCK`. Its only
  CSR accesses are `mcycle` and the cache lock's `MIE` clear and restore.

### A reader that falls behind

The reader gives each item back to the DMA as soon as it takes the block, not when the
consumer is done. With N strips in the ring and T the time the DMA takes for one strip
(0.59 ms measured at 1280x800, 120 fps: 64 lines of 9.1 us):

| The consumer keeps one strip for | Effect |
|---|---|
| less than (N - 1) T | nothing lost |
| (N - 1) T to about N T (the DMA comes round to the strip being read) | `release()` returns `false`; counted in `StripControl::strips_lapped()` and `frames_truncated`; the rest of that frame is tracked but not delivered; delivery resumes at the next frame start, no probation frame (the DMA never stopped, so the blocks stay aligned) |
| longer (the DMA reaches an item the reader has not taken yet) | the DMA halts; core 0 counts a stall; once every item is valid again, `service()` restarts the ISP and bridge and resumes the channel; the reader resynchronises with a probation frame, as on one core |

Lap detection: strip `k` is overwritten only after the DMA finished item `k - 1` once more,
whose done bit the reader cleared before it took `k`. So `release` reports a lap if `k - 1`
shows done. This needs the write-back of `k - 1` to become visible before the DMA's first
data write into `k`; the TRM does not say, so it was measured. On the board (1280x800, N = 4,
log `TwoCore/i-faults.log`), times after the reader took `k`, minimum to maximum over the
holds: item `k + 1` done at 571 to 582 us, `k + 2` at 1153 to 1164 us, `k + 3` (= `k - 1`)
at 1735 to 1746 us, the first 16 bytes of `k` changed at 1764 to 1775 us, `k` done again at
2318 to 2328 us. A strip released right after a hold of 1.70 ms released `true`, of 1.75 ms
`false` with its data unchanged, of 1.80 ms and more `false` with its data changed; from 2.35
ms each hold also stalled the DMA. In every one of the 1178 holds where the bytes changed,
the done bit of `k - 1`, read right after the changed bytes, was set (order errors 0; the
hold loop reads four done bits and 16 bytes per turn, a turn of a few microseconds
[INFERENCE: not timed]). So the lap check fires about 20 us before the first write into
`k`; this is a measurement on this chip and these timings, not a guarantee from the TRM.

The consumer must treat what it computed from a frame as valid only if every strip of the
frame released `true`.

### Stop and restart while the reader polls

- A run number, odd while running, even while stopped, +1 at every start and stop, never
  reused. The reader marks itself busy for each whole poll and checks the run number first.
- `stop` makes the run number even before it stops the bridge and the channel.
- `start` waits (at most 10 ms, else `StripError::ReaderBusy`) until the reader is not busy,
  rewrites every item, publishes the geometry and the new odd run number, then enables the
  channel.
- Both sides store then load, all `SeqCst` (a Dekker handshake): either `start` sees the busy
  flag and waits, or the poll sees the new run number and leaves the items alone.
- `release` checks the run number after the consumer's reads. Any DMA write of a later run
  follows a change of the run number, so an unchanged one means the strip is from the current
  run. `start` does not wait for held strips: they fail their release check.
- The drop of `StripControl` stops capture and makes the reader inert (instance number
  cleared, then the same bounded wait).

Shared state is atomics only; each field names its single writer in `src/strips/split.rs`.

### Board checks (2026-11-11)

Reader on core 1 (`poc-p4-eth`, `RUN_CORE1`, plans in `src/core1_soak.rs`), logs in
`/tmp/p4perf-logs/TwoCore/`.

1. Call-graph walk of the core-1 runner from its entry: **pass**. No reference to
   `CRITICAL_SECTION`, `SCHEDULER`, `esp_println::LOCK` or `raise`; the only ROM cache
   calls (`Cache_Invalidate_Addr`, `Cache_WriteBack_Addr`, `Cache_Get_L2_Cache_Line_Size`)
   are inside `esp32p4_cache`. Details in the `poc-p4-eth` README.
2. Pixel check with the reader on core 1, all four 120 fps modes, 30 s each (not 10 min;
   `c-check.log`): **pass**. 1280x800: 46 787 strips, 2 879 200 rows, 0 bad, 3599 of 3599
   frames good; 1280x720: 2 591 280 rows, 640x480: 1 728 000, 640x400 bin2: 1 439 600, all
   0 bad; stalls, length, DMA errors and laps 0. 10 minutes at 1280x800: with core 0
   contention, item 8; without (`g3-long.log`): 936 377 strips,
   57 623 200 rows, 0 bad, 72 029 of 72 029 frames good, stalls, laps and `isr_runs` 0,
   strip start latency at most 51 us, `invalidate_cycles_max` 8508 (21 us).
3. Interrupt count: **pass**. `isr_runs` 0 in every clean run (`a-bringup.log`,
   `c-check.log`); in the fault runs it equals the stall count.
4. Lap test (`d-faults.log`, `d2-faults.log`, `i-faults.log`): **pass**. 5433 holds from
   0.5 to 100 ms: changed and released `true` 0. With the release right after the hold
   (`i-faults.log`, 1681 holds from 1.5 ms): order errors 0, unchanged and released `false`
   only for the 1.75 ms holds (79), the window between the lap flag and the first write.
   Timeline above; delivery resumed with the next frame; `dma_stalls` 0 below about 2.33 ms.
5. Stall test (holds of 1, 5, 30, 100 ms every 20 frames, `d2-faults.log`): **pass**. A stall
   per hold of 5 ms or more (27, 24, 18), `service()` resumed each time, the next good frame
   ended at most 11.2, 32.1, 32.1 and 28.8 ms after the hold, rows bad 0, the guard after
   the ring and the items intact after each case.
6. 1000 stop/start cycles while the reader polls (`h3-cycles.log`): **pass**. ReaderBusy 0,
   other start errors 0, strips of a stopped run released `true` 0 (`false` 772), rows bad
   0, frames good 1200 of 1200, every run delivered a good frame: 33.55 ms mean, 33.56 ms
   max after `start` (stream-on 1 ms after it), `start` at most 34 us. Each cycle turns the
   stream off and stops at once, then waits 20 ms. A restart with the stream left on gives
   no data at all (`h2-cycles.log`: no block, PHY clock lane neither in stop state nor in
   HS): `stop` resets the D-PHY and `start` must come before stream-on, as documented.
7. Drop the control half while the reader polls: **pass** (`c-check.log`): list items
   unchanged over 50 ms, no strip delivered, core 1's heartbeat still moving.
8. Cache contention (`f3-contention.log`): **pass** for the cache-operation part; the network
   load is left for the co-load step. Core 0 ran cache operations through `esp32p4-cache` in
   a tight loop in thread mode and from a 10 kHz timer interrupt (internal 64 B to 4 KiB,
   PSRAM 64 B to a whole frame), about 444 000 lock operations per second, while core 1
   polled. Pixel check, 10 min: 936 377 strips, 57 623 200 rows, 0 bad, 72 029 of 72 029
   frames good, stalls, laps and `isr_runs` 0, `invalidate_cycles_max` 3956 (9.9 us), strip
   start latency at most 14 us. Detector live, 2 min: 14 404 frames, 0 dropped or lapped,
   detect mean 936 us (845 without contention). Lock maxima in the `esp32p4-cache` README.

## Build

`cargo build --release` and `cargo clippy --release` in this directory (the target is set in
`.cargo/config.toml`). It is linked into `cam-p4`, `poc-p4-eth` and `poc-p4-stream`.
