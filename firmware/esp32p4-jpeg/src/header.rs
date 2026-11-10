//! The JPEG file header the CPU writes in front of the codec's entropy-coded data, and the
//! quantisation table the codec is programmed with.
//!
//! Ported from ESP-IDF `esp_driver_jpeg/jpeg_emit_marker.c` and `jpeg_param.c` (Apache-2.0)
//! for one greyscale component: SOI, APP0 (JFIF 1.01), DQT (table 0), SOF0 (baseline, 8-bit,
//! one component 1x1 on table 0), DHT (the luminance DC and AC tables of ITU-T T.81 Annex K.3,
//! which the hardware encoder uses), COM (padding), SOS. The codec adds the entropy-coded
//! data, its stuffing bytes and EOI.
//!
//! The COM segment pads the header to [`HEADER_LEN`] bytes, a multiple of the largest cache
//! line (128 B), so the DMA's part of the output starts on a cache line and invalidating it
//! never touches the header the CPU wrote (ESP-IDF `emit_com_marker` does the same).

/// Header length: 328 bytes of markers plus the COM padding to a multiple of 128.
pub const HEADER_LEN: usize = 384;

/// Largest cache line on the P4 (L1 64 B; L2 64 or 128 B as the bootloader sets it).
pub const MAX_LINE: usize = 128;

const _: () = assert!(HEADER_LEN.is_multiple_of(MAX_LINE));

/// Zig-zag order: `ZIGZAG[k]` is the natural (row-major) index of the k-th coefficient
/// (ITU-T T.81 figure A.6; ESP-IDF `zigzag_arr`).
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Luminance quantisation table of ITU-T T.81 Annex K.1 (table K.1), natural order.
const LUMA_QUANT: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Luminance DC Huffman table (T.81 table K.3): code counts per length 1..=16, then values.
const DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Luminance AC Huffman table (T.81 table K.5).
const AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
const AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// The quantisation table for `quality` (1..=100) by the IJG rule (libjpeg
/// `jpeg_quality_scaling` and `jpeg_add_quant_table` with `force_baseline`; ESP-IDF
/// `compute_quant_table`): scale 5000 / q below 50, else 200 - 2 q; each entry
/// `(base * scale + 50) / 100` clamped to 1..=255. Natural order.
pub fn quant_table(quality: u8) -> [u8; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut t = [0u8; 64];
    for (out, &base) in t.iter_mut().zip(LUMA_QUANT.iter()) {
        *out = ((u32::from(base) * scale + 50) / 100).clamp(1, 255) as u8;
    }
    t
}

/// Writes the header for a `width` x `height` greyscale picture quantised with `quant`
/// (natural order) into `out`.
pub fn build(out: &mut [u8; HEADER_LEN], width: u16, height: u16, quant: &[u8; 64]) {
    let mut w = Writer { buf: out, len: 0 };
    // SOI
    w.marker(0xD8);
    // APP0: JFIF 1.01, no density unit, 1:1, no thumbnail.
    w.marker(0xE0);
    w.word(16);
    w.bytes(b"JFIF\0");
    w.bytes(&[1, 1, 0]);
    w.word(1);
    w.word(1);
    w.bytes(&[0, 0]);
    // DQT: table 0, 8-bit, in zig-zag order.
    w.marker(0xDB);
    w.word(67);
    w.byte(0);
    for &k in &ZIGZAG {
        w.byte(quant[k as usize]);
    }
    // SOF0: 8-bit precision, height, width, 1 component: id 1, 1x1, table 0.
    w.marker(0xC0);
    w.word(11);
    w.byte(8);
    w.word(height);
    w.word(width);
    w.bytes(&[1, 1, 0x11, 0]);
    // DHT: DC table 0, AC table 0.
    w.dht(0x00, &DC_BITS, &DC_VALUES);
    w.dht(0x10, &AC_BITS, &AC_VALUES);
    // COM: zero padding up to the SOS.
    const SOS_LEN: usize = 10;
    let pad = HEADER_LEN - w.len - 4 - SOS_LEN;
    w.marker(0xFE);
    w.word(pad as u16 + 2);
    for _ in 0..pad {
        w.byte(0);
    }
    // SOS: 1 component, id 1, DC table 0 / AC table 0, spectral 0..63, no approximation.
    w.marker(0xDA);
    w.word(8);
    w.bytes(&[1, 1, 0x00, 0, 63, 0]);
    debug_assert_eq!(w.len, HEADER_LEN);
}

struct Writer<'a> {
    buf: &'a mut [u8; HEADER_LEN],
    len: usize,
}

impl Writer<'_> {
    fn byte(&mut self, b: u8) {
        self.buf[self.len] = b;
        self.len += 1;
    }
    fn bytes(&mut self, b: &[u8]) {
        self.buf[self.len..self.len + b.len()].copy_from_slice(b);
        self.len += b.len();
    }
    fn word(&mut self, v: u16) {
        self.bytes(&v.to_be_bytes());
    }
    fn marker(&mut self, m: u8) {
        self.bytes(&[0xFF, m]);
    }
    fn dht(&mut self, class_id: u8, bits: &[u8; 16], values: &[u8]) {
        self.marker(0xC4);
        self.word((2 + 1 + 16 + values.len()) as u16);
        self.byte(class_id);
        self.bytes(bits);
        self.bytes(values);
    }
}
