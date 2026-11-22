# espion-vision

Marker blob detector for the espion camera: it finds the bright blobs in RAW8 or packed
MIPI CSI-2 RAW10 mono frames and returns, per blob, the thresholded intensity-weighted
centroid in 1/32 pixel, the area and the peak value; `refine` then measures each blob on the
frame (centre in 1/256 pixel without the threshold's bias, width and flux), and `track` gives
each measured blob an ID that it keeps from frame to frame for as long as it is the same
marker (`Tracker::update` with the frame's capture time; constant-velocity prediction, gated
nearest-first association, coasting through missed frames, markers merged into one blob
kept apart; integer arithmetic, about 8 KB for `DefaultTracker`'s 96 tracks; the module's
documentation has the rules, `docs/protocol.md` "Identities" what a host can rely on). It
also holds `synth`, the deterministic synthetic frame generator that the tests and the board
benchmark (`firmware/poc-p4-eth`) use.

- `no_std`, no heap, no dependency by default: it builds and tests on the host.
- Fixed capacities as const generics: `Detector<RUNS, COMPS, BLOBS>`. `DefaultDetector` is
  `Detector<256, 256, 256>` (256 runs per row, 256 live components, 256 blobs per frame).
- `Detector::new` is a `const fn`, so the detector can live in a static (`DefaultDetector`
  takes about 24 KB, too much for most task stacks).

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

`Detector::set_split(Some(Split { background, min_area }))` splits the blobs of the frames
begun next by their peaks. The frame is also cut at up to 5 higher thresholds, each twice as
far above `background` as the one before. A blob that holds two or more blobs of the next
threshold that count as peaks (peak at least 5/4 of that threshold above the background, at
least 3 pixels and `min_area`) is replaced by them, each cut the same way in turn. Two
markers whose light touches come out as two blobs when the dip between them is below 2/5 of
the lower peak, and never when it is above 4/5. Each blob's centroid, area, peak and
`sigma` are those of its own component. The higher thresholds read a row's bright runs
again, at most the row's pixels in all: a run past that counts whole for the next
threshold, so in a row with that much light peaks may stay together. Their runs share the
row's run capacity with the frame's own.

`Blob::sigma` (1/32 pixel) is the blob's size as a Gaussian's: `sigma² = light / (2π ×
height)`, with the light the sum of its pixels above the background and the height its
peak above its threshold (`spot_sigma`). For a Gaussian spot it is the spot's sigma
whatever the threshold cut off.

The dark skip is a trait, `Skip`, passed to each call (`push_rows_with`,
`process_frame_with`, `process_frame_raw10_with`) rather than stored in the detector, so a
vector skip can hold a per-core token that is not `Send` while the detector lives in a
static. `Scalar` is the portable skip and the reference; the calls without `_with` use it.
Any skip that keeps the contract in `skip.rs` gives the same blobs.

`push_rows_until(skip, rows, first_row, stop)` is `push_rows_with` that can stop early, at a
row boundary, for a caller with a deadline (core 1 must give each strip back to the DMA in
time). It asks `stop()` before each row it scans and before each jump of the skip over dark
rows, returns at the first `true`, and says how many rows it did (those the skip passed
included). Between two questions it does at most one row (scan, labelling, end of row) or one
skip jump over at most the rest of `rows`, so the work after the last `false` is bounded by
one row's worst case (every other pixel bright) or one skip over `rows`, whatever the pixels.
Pushing the remaining rows afterwards continues exactly where it stopped (bit-identical blobs
and `Stats`); rows never pushed count as dark, as any gap does. With a `stop` that never
answers `true` it is `push_rows_with`, which is built on it.

## Automatic threshold

`auto` picks the threshold of the next frame from the current one, integer math only:

- `sample_raw8` / `sample_raw10` add every 4th pixel (`x % 4 == 0`) of every 16th row
  (`row % 16 == 0`) to a `Histogram` (256 bins, 8-bit units; RAW10 on the top 8 bits).
  Strips of whole rows give the same histogram as the whole frame; pass each strip's first
  frame row. A 1280 x 800 frame gives 16 000 samples.
- `Background::from_histogram` gives the median and `sigma = p84 - p50`, a robust spread
  taken on the bright side; the few marker samples barely move either.
- `threshold_raw8(bg)` is `median + max(6 * sigma, 12)` in `1..=255`: the noise floor.
  `threshold_raw10` is 4 times it (`4..=1020`), which selects the same pixels on their top
  8 bits. It does not follow the brightest blob: a marker's peak depends on its size, and
  a threshold at half the brightest peak loses every marker less than half as bright
  (`firmware/poc-p4-stream/README.md`, "Measured: the noise-floor threshold and the split
  by peaks"). The split by peaks (`set_split` with `bg.median`) keeps a bright marker's
  skirt from joining its neighbours instead.

## Refinement

The detector's centroid weighs only the pixels above the threshold: it is pulled towards
the pixel centres (0.07 to 0.15 px) and jumps when a pixel crosses the threshold. The
threshold only finds the blobs; `refine_raw8` / `refine_raw10` measure each one on the
frame and return a `Spot`: centre in 1/256 pixel (pixel centres at integer coordinates),
`width` (the spot's sigma, 1/256 pixel) and `flux` (sum of value − background over the
window, frame units). Integer math only, so the board and the host agree bit for bit.

1. Window sigma `s` from the blob: `Blob::sigma`, at least 0.75 px. Past 4 px the spot is
   measured on bins of `2^k × 2^k` pixels, the smallest `k`
   (at most 5) that brings `s / 2^k` to 4 or below: each bin the rounded mean of its pixels
   (RAW8 with 2 fraction bits), the rest in bins, the result scaled back to pixels. A
   window too narrow for the spot sees only its top: the width blows up (step 4) and the
   centre follows the top's unevenness. The square of half-size `ceil(4s) + 1` around the
   blob's centroid (at most 35 × 35, pixels or bins) is read from the frame once into a
   stack buffer; the rest works on it.
2. Background: the median of the pixels on the perimeter of that square, selected with
   two 32-bin counts (no sort).
3. From the blob's centroid, iterated Gaussian-weighted centroid over the square of
   half-size `ceil(4s)`: weight `(value − background) × exp(−d² / 2s²)`, not clipped. Each
   step moves twice the weighted mean offset (at most 1 px): one step lands on a Gaussian
   spot of width `s`, and the centre of any symmetric spot is the fixed point whatever `s`.
   At most 8 steps, until a step is below 1/512 px.
4. `width`, from the last step's pass (its estimate is within 1/512 px of the result):
   the Gaussian-weighted second moment `m = Σ w d² / Σ w` (same weights `w`, unclipped,
   x and y averaged). A Gaussian spot of sigma σ under the window gives
   `m = σ² s² / (σ² + s²)`, so `σ² = m s² / (s² − m)`, less the 1/12 px² of the pixel's own
   width (binned: the bin's). For any other profile it is the sigma of the Gaussian with the
   same weighted moment. `m ≤ 0` gives 0, `m ≥ s²` (far wider than the window) or a width
   past 255 px `u16::MAX`. `flux`: the unweighted sum of value − background over that
   window, at least 0.

Cost: the weights are separable (`gx(dx) · gy(dy)`, built once per step from an
`exp` table), so a step is one pass over the window with four `i32` multiply-adds per
pixel (15 RV32 instructions in the loop), plus about 45 instructions per column and 90 per
row; divisions are 32-bit (two per step); no `i128`, no 64-bit division, no sort. At
σ 1.5 px: a 15 × 15 window, 2.7 steps, about 900 pixel reads per blob (the 17 × 17 buffer
once, the window per step), estimated at about 21 000 instructions per blob on the P4. A
binned spot's buffer reads each of its pixels once (about 15 × the blob's area), the steps
stay within the 35 × 35 bins.

Every window is clipped to the frame (a spot within 4 widths of the edge may be biased
towards the inside), to the rows given, and to the buffer (an estimate that moves more than
1 px from the blob's centroid loses the far columns). Noise-free spots measured by the
tests, at 64 sub-pixel phases each: Gaussian σ 1 to 4 px within 1/256 px (the output
step; thresholded centroid up to 0.16 px) and `width` within 0.4 %; uniform discs and a
saturated Gaussian within 0.012 px (`width`: 1.39 to 1.46 px for a disc of radius 2.5,
2.24 to 2.27 for radius 4, 2.35 to 2.38 for a Gaussian of σ 2 clipped at 255). With 2 DN
read noise and shot noise at σ 1.5 px, peak 220: 0.021 px rms per axis against 0.071 for
the thresholded centroid, `width` 1.500 px on average with 0.015 px rms error. About 2.7
steps on average for Gaussian spots (at most 4), windows of 13 to 17 px square at σ 1.5.
Uniform discs of radius 6 to 24 px (bins of 2 to 8 px) within 0.5 % of the radius, `width`
1.12 to 1.14 × `R / 2` (that of radius 4 at full resolution: 1.12 to 1.14).

## Cargo features

| Feature | What |
|---|---|
| `pie` | `impl Skip for esp32p4_pie::Pie` (crate `firmware/esp32p4-pie`): the PIE dark skip of the ESP32-P4, 16 bytes per load. The impl is compiled for `target_arch = "riscv32"` only; on the host the feature builds the dependency's encoders and adds nothing, so `cargo test --features pie` runs the same tests. The token comes from `Pie::enable()` on the core that runs the detector; pass `&pie` to the `_with` calls |
| `ram` | The hot functions (`push_rows_with` and `push_rows_until` with the strip loop, `Scalar`'s skip methods, `link`, `end_row`, `emit`, `finish`, the centroid division; every function of `refine`) in `.rwtext`, which esp-hal's linker script places in internal RAM; with `pie`, the PIE kernels too (`esp32p4-pie/ram`). No effect on the host. Every linked instance of the generic functions moves (one strip loop per `stop` closure type): about 9.3 KB for the Pie path, 12.7 KB for `Scalar` (measured before `push_rows_until`). Measured on the ESP32-P4 in `firmware/poc-p4-eth/README.md`, "Hot code in RAM and Zb/Zcb" |

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

`cargo test` runs the detector's unit tests, the `synth`, `auto` and `refine` tests, the
skip's tests against a byte-wise statement of its contract, and a randomised equivalence test
(`src/equivalence.rs`): fixed seeds, widths 4, 36, 64, 72, 320, 640, 1280 and 1288,
heights 1 to 24, 0 to 64 blobs of radius 1 to 12 anywhere in the frame, thresholds over
the whole range, RAW8 and RAW10, random buffer alignments and random strip splits, and
skips that answer as `Scalar` does, at the start of the window-sized block holding the
bright pixel (as a vector skip does) or as early as the contract allows; each frame is
compared with an 8-connected flood fill (`src/reference.rs`) blob for blob and in order.
The same strips also go through `push_rows_until`, stopped after a random number of
answers and resumed until done, and must give the unbounded `Stats` and blobs. On rows of
1280 pixels where every other pixel is bright (shifted by one each row), `push_rows_until`
must do exactly one row per `false` (two questions before the first row) and resume to the
same result.

## Measured

The timings on the ESP32-P4 (synthetic and live frames, whole frames in PSRAM and strips in
internal RAM) are in `firmware/poc-p4-eth/README.md`.
