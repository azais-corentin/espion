# espion-vision

Marker blob detector for the espion camera: it finds the bright blobs in RAW8 or packed
MIPI CSI-2 RAW10 mono frames and returns, per blob, the intensity-weighted centroid in
1/32 pixel, the area and the peak value. It also holds `synth`, the deterministic synthetic
frame generator that the tests and the board benchmark (`firmware/poc-p4-eth`) use.

- `no_std`, no heap, no dependency: it builds and tests on the host.
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

## Algorithm

- A pixel is foreground when its value is `>= threshold`. Foreground pixels are grouped into
  8-connected components: each row is scanned into runs of foreground pixels, each run is
  linked to the runs it touches in the row above, and touching components are merged with
  union-find. A component whose runs stop is finished at once and its slot reused, so only
  the components crossing the current row are held.
- The scan skips dark pixels 32 at a time: a SWAR test tells whether any byte of a `u32` is
  `>= threshold`; it runs on the OR of eight aligned words, then on each word of a chunk that
  passes, and only words that pass are looked at pixel by pixel. RAW10 runs the same test on
  the top 8 bits of 32 pixels (ten words, low-bit bytes masked out), then exactly on each
  group of four pixels.
- Blobs come out in the order in which their last row was seen, the same for a whole frame
  and for any split into strips.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
# For the ESP32-P4:
cargo build --release --target riscv32imafc-unknown-none-elf
```

`cargo test` runs the detector's unit tests, the `synth` tests and a randomised equivalence
test (`src/equivalence.rs`): fixed seeds, widths 64, 320, 640 and 1280, heights 1 to 24, 0 to
64 blobs of radius 1 to 12 anywhere in the frame, thresholds over the whole range, RAW8 and
RAW10, random buffer alignments and random strip splits, each frame compared with an
8-connected flood fill (`src/reference.rs`) blob for blob and in order.

## Measured

The timings on the ESP32-P4 (synthetic and live frames, whole frames in PSRAM and strips in
internal RAM) are in `firmware/poc-p4-eth/README.md`.
