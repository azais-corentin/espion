# espion-vision

Marker blob detector for the espion camera: it finds the bright blobs in RAW8 or packed
MIPI CSI-2 RAW10 mono frames and returns, per blob, the intensity-weighted centroid in
1/32 pixel, the area and the peak value. It also holds `synth`, the deterministic synthetic
frame generator that the tests and the board benchmark (`firmware/poc-p4-eth`) use.

- `no_std`, no heap, no dependency by default: it builds and tests on the host.
- Fixed capacities as const generics: `Detector<RUNS, COMPS, BLOBS>`. `DefaultDetector` is
  `Detector<256, 256, 256>` (256 runs per row, 256 live components, 256 blobs per frame).
- `Detector::new` is a `const fn`, so the detector can live in a static (`DefaultDetector`
  takes about 19.5 KB, too much for most task stacks).

## Usage

```rust
use espion_vision::{Blob, DefaultDetector};

fn detect(det: &mut DefaultDetector, frame: &[u8], width: u16) -> usize {
    let mut out = [Blob::default(); 256];
    // Whole frame at once:
    let n = det.process_frame(frame, width, 128, &mut out);
    // Or strips of whole rows as they arrive (same blobs, same order):
    det.begin_frame(width, 128);
    for (i, strip) in frame.chunks(64 * width as usize).enumerate() {
        det.push_rows(strip, (i * 64) as u16);
    }
    assert_eq!(det.finish(&mut out), n);
    n
}
```

RAW10 frames use `begin_frame_raw10` / `process_frame_raw10` with a 10-bit threshold;
`raw10` holds the packing.

The dark skip is a trait, `Skip`, passed to each call (`push_rows_with`,
`process_frame_with`, `process_frame_raw10_with`) rather than stored in the detector, so a
vector skip can hold a per-core token that is not `Send` while the detector lives in a
static. `Scalar` is the portable skip and the reference; the calls without `_with` use it.
Any skip that keeps the contract in `skip.rs` gives the same blobs.

## Cargo features

| Feature | What |
|---|---|
| `pie` | `impl Skip for esp32p4_pie::Pie` (crate `firmware/esp32p4-pie`): the PIE dark skip of the ESP32-P4, 16 bytes per load. The impl is compiled for `target_arch = "riscv32"` only; on the host the feature builds the dependency's encoders and adds nothing, so `cargo test --features pie` runs the same tests. The token comes from `Pie::enable()` on the core that runs the detector; pass `&pie` to the `_with` calls |
| `ram` | The hot functions (`push_rows_with` with the strip loop, `Scalar`'s skip methods, `link`, `end_row`, `emit`, `finish`, the centroid division) in `.rwtext`, which esp-hal's linker script places in internal RAM; with `pie`, the PIE kernels too (`esp32p4-pie/ram`). No effect on the host. Every linked instance of the generic functions moves: about 9.3 KB for the Pie path, 12.7 KB for `Scalar`. Measured on the ESP32-P4 in `firmware/poc-p4-eth/README.md`, "Hot code in RAM and Zb/Zcb" |

## Algorithm

- A pixel is foreground when its value is `>= threshold`. Foreground pixels are grouped into
  8-connected components: each row is scanned into runs of foreground pixels, each run is
  linked to the runs it touches in the row above, and touching components are merged with
  union-find. A component whose runs stop is finished at once and its slot reused, so only
  the components crossing the current row are held.
- With no run open, the scan asks the skip where the next bright pixel can be: an offset
  with no bright pixel before it and one within the next 64 bytes (RAW10: 80 bytes, 64
  pixels), unless the row ends. From there it reads exactly (a SWAR test on aligned words
  that tells whether any byte of a `u32` is `>= threshold`, then pixel by pixel in the
  words that pass) until a dark word with no run open once a bright pixel or the window
  was passed, then asks the skip again. `Scalar` tests 64 bytes at a time on the OR of
  their 16 words, then each word of a block that passes (the OR of dark bytes can pass).
  RAW10 runs the same test on the top 8 bits (low-bit bytes masked out), 80 bytes at a
  time, and the scan tests each group of four pixels exactly.
- While no component crosses the row above, dark rows change nothing but the row counter,
  so the skip runs once over the rest of the strip (the rows of one `push_rows` call are
  contiguous) and row by row work resumes in the row of its answer, from the answer. A
  dark strip costs only the skip; there is no work per dark row.
- One `push_rows` call runs one strip loop, built per pixel format and threshold class (no
  format test per row), with the scan state in locals and the two run buffers swapped by
  reference. A row with no run, under a row with no run, skips the labelling.
- Blobs come out in the order in which their last row was seen, the same for a whole frame
  and for any split into strips.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
# For the ESP32-P4:
cargo build --release --target riscv32imafc-unknown-none-elf
cargo build --release --target riscv32imafc-unknown-none-elf --features pie
```

`cargo test` runs the detector's unit tests, the `synth` tests, the skip's tests against
a byte-wise statement of its contract, and a randomised equivalence test
(`src/equivalence.rs`): fixed seeds, widths 4, 36, 64, 72, 320, 640, 1280 and 1288,
heights 1 to 24, 0 to 64 blobs of radius 1 to 12 anywhere in the frame, thresholds over
the whole range, RAW8 and RAW10, random buffer alignments and random strip splits, and
skips that answer as `Scalar` does, at the start of the window-sized block holding the
bright pixel (as a vector skip does) or as early as the contract allows; each frame is
compared with an 8-connected flood fill (`src/reference.rs`) blob for blob and in order.

## Measured

The timings on the ESP32-P4 (synthetic and live frames, whole frames in PSRAM and strips in
internal RAM) are in `firmware/poc-p4-eth/README.md`.
