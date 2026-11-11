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

**Not run on the board yet** (2026-11-11). Built and checked by reading and in the release
ELF only; the board checks are listed below.

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
(about 0.64 ms at 1280x800, 120 fps):

| The consumer keeps one strip for | Effect |
|---|---|
| less than (N - 1) T | nothing lost |
| (N - 1) T to about N T (the DMA comes round to the strip being read) | `release()` returns `false`; counted in `StripControl::strips_lapped()` and `frames_truncated`; the rest of that frame is tracked but not delivered; delivery resumes at the next frame start, no probation frame (the DMA never stopped, so the blocks stay aligned) |
| longer (the DMA reaches an item the reader has not taken yet) | the DMA halts; core 0 counts a stall; once every item is valid again, `service()` restarts the ISP and bridge and resumes the channel; the reader resynchronises with a probation frame, as on one core |

Lap detection: strip `k` is overwritten only after the DMA finished item `k - 1` once more,
whose done bit the reader cleared before it took `k`. So `release` reports a lap if `k - 1`
shows done. [INFERENCE] This assumes the write-back of `k - 1` becomes visible before the
DMA's first data write into `k`; the TRM does not say.

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

### Board checks still to do

1. Build the core-1 runner (performance plan 5.3) and run the `llvm-objdump` call-graph walk
   from its entry closure: no reference to `CRITICAL_SECTION`, `SCHEDULER`,
   `esp_println::LOCK`, `SoftwareInterrupt::raise`, and no ROM cache call outside
   `esp32p4_cache`.
2. `poc-p4-eth` strip check (`StripCheck`: row checksums of the sensor's random test pattern
   from a whole-frame capture) with the reader on core 1, in all four 120 fps modes, 10 min
   each: every row of every strip released `true` matches, `frames_complete` equals the
   frames the consumer finished, `length_errors = dma_stalls = strips_lapped() = 0`,
   `dma_errors = 0`.
3. Interrupt count: the DMA interrupt runs only on errors and stalls (`isr_runs` stays 0 in
   a clean soak).
4. Lap test: keep strips for times stepped from (N - 2) T to N T, with row checksums taken
   when the strip is handed out and again just before `release`. Every strip whose checksum
   changed must release `false` (this tests the [INFERENCE] above), and the number of
   `false` releases with unchanged checksums shows how conservative the check is. Delivery
   resumes at the next frame start; `dma_stalls` stays 0 below N T.
5. Stall test: keep one strip (or stop polling) for 1 to 100 ms, longer than N T, so the DMA
   halts; core 0 counts a stall, `service()` resumes, the next
   whole frame arrives within the 25 to 33 ms measured on one core after the reader polls
   again, and the guard areas after the ring and around the items stay unwritten.
6. 1000 `stop`/`start` cycles while the reader polls: no `ReaderBusy`, no strip of an old
   run released `true`, every run delivers again after two frames.
7. Drop the control half while the reader polls: the reader returns `None` from then on and
   writes no item (guard check on the items).
8. Cache contention: core 0 streaming at the wire rate while core 1 polls; strip invalidate
   cycles (`invalidate_cycles_max`) and no corrupt row.

## Build

`cargo build --release` and `cargo clippy --release` in this directory (the target is set in
`.cargo/config.toml`). It is linked into `cam-p4`, `poc-p4-eth` and `poc-p4-stream`.
