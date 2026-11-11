# esp32p4-pie

PIE kernels for the ESP32-P4 HP cores on stable Rust. PIE (Xespv 2.2) is the chip's
128-bit SIMD extension: eight 128-bit `q` registers with byte, half-word and word lanes,
loads and stores with post-increment, and horizontal reductions such as "largest byte".

- `no_std`, no heap. The encoders and the scalar reference build and test on the host;
  everything that executes PIE or CSR instructions is built for `target_arch = "riscv32"`
  only.
- Chips at revision 3.0 or later only. Older chips implement PIE 2.1, whose encodings
  differ; `Pie::enable` reads the revision from eFuse (the fields esp-hal 1.2.2 reads,
  through the `esp32p4` PAC) and refuses them.

## Usage

```rust
let pie = esp32p4_pie::Pie::enable()?;          // this core, this thread
assert_eq!(pie.self_test(1, 10_000), 0);       // optional checks at boot
assert_eq!(pie.self_test10(1, 10_000), 0);
let o = pie.first_bright(row, threshold);       // o == row.len(): no byte >= threshold
let o = pie.first_bright10(raw10_row, t >> 2);  // the same over RAW10 pixels
```

Each kernel has a scalar reference with the same contract that builds and runs on any
target, and a self-test that compares the kernel with its reference on the chip:

| kernel | contract | reference | self-test |
|---|---|---|---|
| `first_bright(bytes, t)` | `o <= len`; no byte of `bytes[..o]` is `>= t`; `o == len` or a byte of `bytes[o..min(o + 64, len)]` is | `first_bright_scalar` (64-byte chunks) | `self_test` |
| `first_bright10(bytes, t)` | RAW10 from a group boundary (bytes 0..=3 of each 5-byte group are pixels, byte 4 is not); `o` a multiple of 5 or `len`; no pixel byte of `bytes[..o]` is `>= t`; `o == len` or a pixel byte of `bytes[o..min(o + 80, len)]` is | `first_bright10_scalar` (80-byte chunks) | `self_test10` |

All kernels take any alignment and length and never read outside their slices. A
self-test returns the number of failed cases (0 = pass); its rows sit between guard bytes
chosen so that a read or write outside the slice changes the result, at every start
alignment 0..16 in turn.

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

| kernel | what | cycles per pixel, 1280-byte rows |
|---|---|---|
| `first_bright` | first 64-byte block holding a byte `>= threshold` | 0.29 (L1), 0.41 (cold internal RAM) |
| scalar dark skip (detector today) | same, 32-bit words | 1.27 (L1), 1.35 (cold internal RAM) |
| `first_bright10` | first 80-byte RAW10 pass (64 pixels) holding a pixel byte `>= threshold` | not measured |

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
