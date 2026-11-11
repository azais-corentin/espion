# esp32p4-perf

Performance and cache counters of the ESP32-P4 HP cores, for profiling firmware on the
board. `no_std`, no dependencies, no locks: it works from either core, including a bare
second core with no RTOS. RISC-V only (`riscv32imafc-unknown-none-elf`); it must run in
machine mode.

## Counters

| Field | Source | Meaning |
|---|---|---|
| `cycles` | `mcycle` | Core clock cycles (400 MHz on the board). |
| `instructions` | `minstret` | Retired instructions. |
| `branches` | `mhpmcounter9`, event 7 | Retired conditional branches (not jumps or calls). |
| `mispredicts` | `mhpmcounter8`, event 6 | Conditional-branch mispredictions. |
| `stores` | `mhpmcounter13`, event 0xB | Retired store instructions. |
| `l1i_hit`, `l1i_miss` | `CACHE` +0x180/+0x184 (core 0), +0x190/+0x194 (core 1) | L1 instruction cache of the calling core. |
| `l1d_hit`, `l1d_miss` | `CACHE` +0x1C0/+0x1C4 (core 0), +0x1D4/+0x1D8 (core 1) | L1 data cache of the calling core. |
| `l2i_hit`, `l2i_miss` | `CACHE` +0x300/+0x304 plus +0x310/+0x314 | L2 instruction side, bus 0 + bus 1. |
| `l2d_hit`, `l2d_miss` | `CACHE` +0x340/+0x344 plus +0x354/+0x358 | L2 data side, bus 0 + bus 1. |

`CACHE` is 0x3FF1_0000. The offsets are those of the `esp32p4` PAC 0.3.0.

`init()` writes the event selectors (`mhpmevent8/9/13` at CSR 0x328/0x329/0x32D), clears the
`mcountinhibit` bits, enables the cache counters (`+0x17C` for L1, `+0x2FC` for L2) and clears
the calling core's L1 counters. Call it once on each core that measures.

## Caveats

- The event counters count nothing until `init()` runs on that core: their selectors are 0
  at boot.
- Every counter is per core and includes interrupt time on that core. Nothing saves or pauses
  them on an interrupt or a context switch (esp-rtos does not), so a window that an interrupt
  hits also counts the handler.
- The L1 hit counters count accesses: one per load or store that hits, whatever its width (a
  128-bit PIE load counts once). Measured: 1959 hits for 1920 word loads of a warm buffer.
- The miss counters do **not** count cache lines. They count miss events, including retries
  while a line fill is pending. Measured on cold reads: about 10 L1 data misses per 64-byte
  line from internal RAM, about 148 per line from PSRAM, and about 76 L2 data misses per line
  from PSRAM. Use them to compare runs of the same code, not to count bytes moved.
- The L2 counters are shared by both cores. `init()` does not clear them, so a call on one
  core leaves a window open on the other intact.
- The L1 counter enables live in one register for both cores. `init()` writes all of them on
  every call (no read-modify-write that could race with the other core); it clears only the
  calling core's L1 counters.
- All counters are 32 bits wide; deltas use wrapping subtraction, so a window is correct as
  long as no counter advances by 2^32 inside it. `mcycle` wraps after 10.7 s at 400 MHz.
  `cycles64()` reads the full 64-bit cycle count.
- The closing reads count themselves: the five `csrr` of the CSR counters fall inside the
  window. The slower cache reads stay outside it (`Sample::now` reads them before the CSRs,
  `Sample::end` after). Measure an empty window to get the fixed overhead.

## Usage

```rust
esp32p4_perf::init();

let start = esp32p4_perf::Sample::now();
detector.push_rows(rows);
let d = start.elapsed(); // = Sample::end().since(&start)
log::info!("push_rows: {d}");
// Prints: push_rows: cyc=<n> ins=<n> ipc=<i>.<iii> br=<n> mis=<n> st=<n> l1i_hit=<n> ... l2d_miss=<n>

// Inside a hot loop, read only the five CSR counters:
let a = esp32p4_perf::CpuSample::now();
step();
let cpu = esp32p4_perf::CpuSample::now().since(&a);
```

## Cycles per pixel

Divide the cycle delta by the pixels the window processed. Keep it in integers with a scale
factor, for example thousandths of a cycle:

```rust
let pixels = u64::from(width) * u64::from(rows);
let milli_cycles_per_px = u64::from(d.cpu.cycles) * 1000 / pixels;
log::info!("{}.{:03} cycles/px", milli_cycles_per_px / 1000, milli_cycles_per_px % 1000);
```

The same division on `d.cpu.instructions` gives instructions per pixel; their ratio is the
IPC (`d.ipc_milli()`). Low IPC with few instructions per pixel points at memory stalls; high
instruction count per pixel points at the code. Time in microseconds is `cycles / 400` at
400 MHz.
