# esp32p4-cache

Cache maintenance and cache prefetch for the ESP32-P4, for DMA buffers that the CPU reads or
writes. `no_std`; it depends only on the `esp32p4` PAC (no esp-hal, esp-rtos or
`critical-section`), so a bare second core with no scheduler and no interrupts can call it.
RISC-V only (`riscv32imafc-unknown-none-elf`); it must run in machine mode.

## Maintenance

| Function | ROM call | Use |
|---|---|---|
| `invalidate(addr, len)` | `Cache_Invalidate_Addr` | Drop the cached lines of a buffer a DMA wrote, before reading it. Dirty lines are discarded |
| `write_back(addr, len)` | `Cache_WriteBack_Addr` | Write dirty lines back before a DMA reads the buffer |
| `writeback_invalidate(addr, len)` | both, in one hold of the lock | Before switching a buffer to the non-cacheable alias, or handing it to a DMA that writes it |
| `line_size(addr)` | `Cache_Get_L2_Cache_Line_Size` outside L2MEM | Alignment of start and length: 64 B in internal RAM, max(64, L2 line) in PSRAM |
| `in_l2mem(addr, len)`, `non_cacheable(addr)`, `L1_LINE` | none | Internal RAM test and its uncached alias (+0x4000_0000) |

Internal RAM (L2MEM, 0x4FF0_0000..0x4FFC_0000) is cached by the L1 data cache only, so the
operations there touch L1; PSRAM and flash go through L1 and L2, so they touch both.

The ROM functions are declared in the crate and get their addresses from the ROM linker
script (`esp32p4.rom.ld` of `esp-rom-sys`) that esp-hal adds to every firmware binary.

## The lock

Both HP cores share one set of cache-sync registers (`SYNC_CTRL`, `SYNC_MAP`, `SYNC_ADDR`,
`SYNC_SIZE`), and the ROM functions program them without a lock, so two operations started at
once from the two cores can garble each other. ESP-IDF serialises them with a cross-core
spinlock; this crate does the same.

- One atomic word, on a cache line of its own, is the lock. Every ROM call above runs while
  holding it, and only for the duration of that call.
- It is taken with machine interrupts masked on the calling core: `csrrci mstatus, 8` saves
  and clears `MIE`, `amoswap.w.aq` takes the word, the ROM function runs, a release store
  frees the word and `MIE` is set again if it was set before. If the word is busy, `MIE` is
  restored while the core waits and cleared again for the next try.
- Masking is what lets an interrupt handler call these functions too: a holder that its own
  core could interrupt would leave the handler spinning on a lock nobody can release.
- No `critical_section`: a bare second core must not share esp-hal's global critical section.

Hold time, from the board measurements in `firmware/poc-p4-eth/README.md`: a whole-frame
invalidate over L1 and L2 takes 17 to 31 us whatever the frame size; a strip invalidate
(80 or 40 KiB in L2MEM, L1 only) 1.9 to 2.3 us on average, 2.6 us at most, 9.7 us the first
time. `poc-p4-stream` measured its receiver's whole-frame invalidate at 1450 to 2200 cycles
(4 to 5.5 us), and 10 200 cycles (25 us) for a frame the CPU had just read
(`firmware/poc-p4-stream/README.md`, "Measured: where core 0's time goes per frame",
2026-11-11). Write-back was not measured. So interrupts on the calling core wait up to about
31 us behind a whole-frame invalidate and about 3 us behind a strip; the other core keeps its
interrupts, and a cache operation it starts meanwhile waits up to the same time.

**Rule:** every cache operation in this repository goes through this crate. Not covered are
esp-hal 1.2.2's own ROM cache calls (`soc/esp32p4/mod.rs`): the Ethernet driver on every TX
buffer and descriptor (`ethernet/dma.rs` through `dma/aligned.rs`), `interrupt::enable_direct`
(vector table write-back), the DMA buffer helpers (`dma/buffers/`, `dma/aligned.rs`), the SPI
master's DMA path, the DSI frame buffers, and at boot the PSRAM set-up and the flash
invalidate. A firmware that runs esp-hal's Ethernet driver on one core and calls this crate
on the other is still exposed. `poc-p4-stream` keeps esp-hal's driver for the bring-up only
and runs its own Ethernet DMA rings (`net/eth.rs`, since `64d4b17`), which call this crate.

## Prefetch

Module `prefetch`, plain register writes in the `CACHE` block (0x3FF1_0000) through the PAC:

| Function | Registers |
|---|---|
| `autoload_configure(level, &Autoload)` | `L1_DCACHE_AUTOLOAD_CTRL` +0x134 and `SCT0..3_ADDR/SIZE` +0x138..+0x154, or `L2_CACHE_AUTOLOAD_CTRL` +0x2B4 and +0x2B8..+0x2D4: up to four sections, trigger (miss, hit, miss+hit), order; leaves it off |
| `autoload_enable`, `autoload_disable`, `autoload_enabled` | `ENA` (bit 0); disable waits for `DONE` (bit 1) |
| `autoload_suspend` / `autoload_resume` | disable, then re-enable only if it was on |
| `l1d_preload(addr, len)`, `l1d_preload_done`, `l1d_preload_wait` | `L1_DCACHE_PRELOAD_ADDR/SIZE/CTRL` +0xD8..+0xE0, at most 16 383 B per preload |

Measured on the board on 2026-11-11 without any DMA running (the performance spike in
`poc-p4-eth`: whole 1280x800 RAW8 frames scanned by the scalar blob detector, 400 MHz):

| Setting | Frame in PSRAM, dark | PSRAM, 64 blobs | Strips in internal RAM, dark |
|---|---|---|---|
| None | 9.9 ms | 10.8 ms | 4.33 ms |
| L1 data autoload, miss+hit | 5.6 ms | 6.5 ms | 4.08 ms |
| L2 autoload, miss+hit | 5.8 ms | 6.6 ms | 4.33 ms |
| L1 data and L2 autoload, miss+hit | 4.6 ms | 5.4 ms | 4.08 ms |
| L1 data autoload, hit only | 7.1 ms | 7.9 ms | 4.08 ms |
| Manual L1 data preload, 12 rows (15 360 B) ahead | 4.4 ms | 5.3 ms | 4.19 ms |

Caveats:
- L2 autoload does nothing for internal RAM: the L2 cache does not cache L2MEM.
- The hit trigger alone is poor on PSRAM.
- A section that covers memory a DMA is writing can load lines before the DMA has written
  them, and an invalidate done before that load does not drop them. Limit sections to data
  already delivered, or suspend autoload around the invalidate. Both live uses below do the
  first.
- No lock: these registers are not the cache-sync registers [INFERENCE: running prefetch
  beside a cache-sync operation was not tested]. Each set exists once for the chip, so one
  context on one core owns the autoload set-up and the manual preloads; nothing here guards
  against a second owner.
- The ROM's own preload functions suspend autoload while they run; `l1d_preload` does not.
  The preload figures above were taken with autoload off.

### Under live DMA

Run on the board on 2026-11-11 in two places, each limited to a buffer the DMA had finished
and that was invalidated, with no prefetch in flight at that buffer's next invalidate:

- **Per-strip L1 data autoload** (`poc-p4-eth`, `strip_soak::AUTOLOAD`): miss+hit, ascending,
  one section over exactly the strip the detector holds, set before `push_rows_with` and
  disabled right after it (`autoload_disable` waits for `DONE`). Strip check with the sensor's
  random test pattern in four modes, 30 s each: 10 874 992 rows, 0 bad. Live strip soak,
  1280x800 RAW8 at 120 fps, 120 s: 14 406 frames, none dropped. In the bench the autoload
  takes the PIE dark frame from 985 to 753 us (-23.6 %) and the scalar one by 8.9 %
  (`firmware/poc-p4-eth/README.md`, "L1 data autoload on the strip path", "Live strip soak,
  final configuration").
- **Manual L1 data preload of the next chunk** (`poc-p4-stream`, `src/prefetch.rs`, mode
  `preload_ahead`): once a datagram is queued, the next datagram's bytes of the same PSRAM
  buffer (camera frame or JPEG file) are preloaded; the reader waits for the preload before
  it gives the buffer back, and no invalidate runs while a prefetch may be in flight. The
  body copy from PSRAM falls from 4400-4900 to 1070-1200 cycles per datagram. Rolling-bars
  check at the wire rate under live capture, 180 s: 1967 frames, 0 failed; `poc-viewer`'s
  board test: 0 failed checks (`firmware/poc-p4-stream/README.md`, "Measured: plan step 6.2,
  cache prefetch of the PSRAM reads"). The same bench ran the autoload modes (L1, L2 or both,
  one section over the bytes read, on for the copy only) over live camera frames too (copy
  2700-3200 cycles); the content checks ran with `preload_ahead` and with prefetch off only.

Not tested:

- Prefetch on one core while the other core runs cache-sync operations: core 1 was off in
  both uses.
- Two owners. The detector's per-strip autoload and the stream's preload use the one set of
  L1 data cache prefetch registers of the chip. If both ran in one firmware they would share
  it; nothing in this crate arbitrates, and the combination has not run. Open point.
- The L1 and L2 autoload over whole frames in PSRAM (the table above) under live capture.

## Build

`cargo build --release` and `cargo clippy --release` in this directory (the target is set in
`.cargo/config.toml`). `esp32p4-csi` and `esp32p4-jpeg` depend on it, so it is linked into
`cam-p4`, `poc-p4-eth` and `poc-p4-stream`; the two PoCs also call it directly (prefetch, and
the stream's Ethernet rings).
