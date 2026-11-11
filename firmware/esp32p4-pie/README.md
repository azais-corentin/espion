# esp32p4-pie

PIE kernels for the ESP32-P4 HP cores on stable Rust. PIE (Xespv 2.2) is the chip's
128-bit SIMD extension: eight 128-bit `q` registers with byte, half-word and word lanes,
loads and stores with post-increment, and horizontal reductions such as "largest byte".

- `no_std`, no heap. The encoders, the scalar references, the case generators and the
  head/middle/tail splits (run with scalar stand-ins for the vector cores) build and test
  on the host; everything that executes PIE or CSR instructions is built for
  `target_arch = "riscv32"` only.
- Chips at revision 3.0 or later only. Older chips implement PIE 2.1, whose encodings
  differ; `Pie::enable` reads the revision from eFuse (the fields esp-hal 1.2.2 reads,
  through the `esp32p4` PAC) and refuses them.

## Usage

```rust
let pie = esp32p4_pie::Pie::enable()?;          // this core, this thread
assert_eq!(pie.self_test(1, 10_000), 0);       // optional checks at boot
assert_eq!(pie.self_test10(1, 10_000), 0);
assert_eq!(pie.self_test_copy(1, 10_000), 0);
assert_eq!(pie.self_test_sum(1, 10_000), 0);
let o = pie.first_bright(row, threshold);       // o == row.len(): no byte >= threshold
let o = pie.first_bright10(raw10_row, t >> 2);  // the same over RAW10 pixels
pie.copy(&mut datagram[HEADER..], chunk);       // as copy_from_slice, any alignment
let mut s = pie.sum16_be(payload) + pseudo_header_sum;
while s > 0xFFFF { s = (s & 0xFFFF) + (s >> 16); }
let checksum = !s as u16;                       // the UDP checksum (0 is sent as 0xFFFF)
```

Each kernel has a scalar reference with the same contract that builds and runs on any
target, and a self-test that compares the kernel with its reference on the chip:

| kernel | contract | reference | self-test |
|---|---|---|---|
| `first_bright(bytes, t)` | `o <= len`; no byte of `bytes[..o]` is `>= t`; `o == len` or a byte of `bytes[o..min(o + 64, len)]` is | `first_bright_scalar` (64-byte chunks) | `self_test` |
| `first_bright10(bytes, t)` | RAW10 from a group boundary (bytes 0..=3 of each 5-byte group are pixels, byte 4 is not); `o` a multiple of 5 or `len`; no pixel byte of `bytes[..o]` is `>= t`; `o == len` or a pixel byte of `bytes[o..min(o + 80, len)]` is | `first_bright10_scalar` (80-byte chunks) | `self_test10` |
| `copy(dst, src)` | `dst` equals `src` byte for byte; panics if the lengths differ | `copy_from_slice` | `self_test_copy` |
| `sum16_be(bytes)` | the one's-complement sum of `bytes` as big-endian 16-bit words (odd last byte = high byte), fully folded: in `0..=0xFFFF`, equal to the plain word sum modulo 0xFFFF, 0 only for all-zero bytes | `sum16_be_scalar` | `self_test_sum` |

None of `first_bright10`, `copy` and `sum16_be` has run on the board yet: their
self-tests and timing are still to be done there.

All kernels take any alignment and length and never write outside their slices. They
read nothing outside them either, with one exception: `copy` with a misaligned source
reads the whole aligned 16-byte words that hold the first and the last source byte (see
below). A self-test returns the number of failed cases (0 = pass); its slices sit between
guard bytes chosen so that a read or write outside the slice changes the result, at every
start alignment 0..16 in turn (`self_test_copy`: every pair of source and destination
alignments).

## Rule for using PIE

Nothing in the Rust stack saves PIE state: not the trap entry (riscv-rt, esp-riscv-rt),
not the esp-rtos thread switch. So:

- One PIE-using thread per core, pinned to it. `Pie` is neither `Send` nor `Sync`, and
  `Pie::enable` refuses a second token on the same core.
- Never in an interrupt handler or an interrupt-priority executor.
- Kernels keep no vector state between calls; the `q` registers are caller-clobbered.

`Pie::enable` writes `STATE` = 1 (INITIAL) to `mext_pie_status` (CSR 0x7F2) and leaves the
hardware-loop CSR alone. Dropping the token writes 0 (OFF).

## How instructions are emitted

rustc's LLVM does not know the PIE mnemonics. `src/encode.rs` builds each 32-bit word with a
`const fn` (bit layouts from the `*_2P2` definitions in Espressif's LLVM fork), and the kernel
emits it with `.insn 4, {w}` and a `const` operand. The encoders take typed registers: `Q`
for `q0`-`q7`, `Ar` for the scalar registers both PIE and `asm!` accept (a0-a5, s8-s11,
t3-t6; PIE only addresses x8-x15 and x24-x31, and `asm!` refuses s0 and s1). Immediates out
of range and a fused load into the result register fail the build.

To check the words of a build, use Espressif's binutils (crosstool-NG `riscv32-esp-elf`):

```sh
riscv32-esp-elf-objdump-xespv2p2 -d -M max target/riscv32imafc-unknown-none-elf/release/deps/libesp32p4_pie-*.rlib
```

Standard objdump (and `llvm-objdump`) mis-decodes them: opcodes with low bits `0011111`
look like the start of a 48-bit instruction. The unit test in `src/encode.rs` compares every
encoder with words from `riscv32-esp-elf-as -march=rv32imafcb_zicsr_zifencei_xesploop_xespv`.

## Kernels

| kernel | what | measured on the board |
|---|---|---|
| `first_bright` | first 64-byte block holding a byte `>= threshold` | 0.29 cycles/pixel (L1), 0.41 (cold internal RAM), 1280-byte rows |
| scalar dark skip (detector today) | same, 32-bit words | 1.27 cycles/pixel (L1), 1.35 (cold internal RAM) |
| `first_bright10` | first 80-byte RAW10 pass (64 pixels) holding a pixel byte `>= threshold` | not run yet |
| `copy` | `copy_from_slice` at any alignment | not run yet (the spike's aligned loop: 0.49 cycles/byte; ROM `memcpy` 1.71 aligned, 9.06 when source and destination differ mod 4) |
| `sum16_be` | Internet checksum partial sum, big-endian 16-bit words | not run yet |

The `first_bright` figures were measured in the spike on 2026-11-11 (Waveshare
ESP32-P4-ETH, rev 3.2, 400 MHz) with the software-pipelined 64-byte loop that
`first_bright` uses. That spike version read up to 32 bytes past the end of the row; this
one finishes the last block without the read-ahead, which has not been timed on the board
yet.

`first_bright` uses 16-byte-aligned loads, so the bytes before the first 16-byte boundary
and after the last one are scanned one at a time.

`first_bright10` has not run on the board yet. Per 80 bytes (lcm of the 5-byte group and
the 16-byte vector) it does five loads, five ANDs with constant masks that clear the
low-bit bytes, four unsigned maxima and one horizontal maximum. The masks are constants
because every pass starts on a group boundary: the first 0 to 15 groups (up to 75 bytes)
are scanned one group at a time until the data reaches a 16-byte boundary, and the tail
of fewer than 80 bytes is scanned the same way. A 1600-byte row (1280 pixels) that
starts on a 16-byte boundary is 20 vector passes with no scalar part.

`copy` has not run on the board yet. It copies up to 15 bytes one at a time until the
destination reaches a 16-byte boundary, so every vector store is aligned, then 16 bytes
per load and store and the last 0 to 15 bytes one at a time. With the source aligned like
the destination the loop is the spike's (four loads and four stores per 64 bytes, 0.49
cycles per byte measured for 1436 bytes in L1). Otherwise it loads the aligned words that
hold the source, each once, and joins each two neighbours with `esp.src.q`, the byte
shift set by `esp.movx.w.sar.bytes`. Every PIE load and store is at an aligned address,
so the result does not depend on the `CFG` misaligned-access modes (`CFG` reads 1 at
reset on this silicon, against the documented 0); esp-dl's `dl_esp32p4_memcpy.S` instead
loads with `esp.ld.128.usar.ip` at the misaligned address, whose data depends on that
mode. The first and the last word read may extend up to 15 bytes past the source slice;
each holds at least one source byte and an aligned 16-byte word never crosses a cache
line or a page, so the read cannot fault, and the bytes outside the slice are shifted out
and never stored.

`sum16_be` has not run on the board yet. The bytes up to the first 16-byte boundary and
after the last one are summed one at a time; the aligned middle goes through
`esp.vmulas.u16.xacc` against a vector of ones, so the 40-bit accumulator `XACC` adds
the eight little-endian 16-bit lanes of each 16 bytes, with the load of the next 16 bytes
fused into each accumulate (as esp-dsp's `dspi_dotprod_u16_arp4.S`). The middle goes in
pieces of at most 64 KiB: 4096 x 8 x 0xFFFF < 2^31, so `XACC` never saturates and its
low 32 bits hold the whole piece. Little-endian lanes are byte-swapped words, and a byte
swap of a 16-bit word is a multiplication by 256 modulo 0xFFFF, so the big-endian sum is
the folded lane sum byte-swapped once (or as is when the middle starts at an odd offset);
the proof is in `src/split.rs` and the host tests check it, the 64 KiB all-0xFF case
included.
