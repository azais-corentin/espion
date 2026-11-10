//! JPEG frames ([`Encoding::Jpeg`](espion_proto::cam::Encoding::Jpeg)): the header check the
//! protocol asks for (one baseline, 8-bit, greyscale JPEG file of the frame's size, from SOI to
//! EOI) and decoding to RAW8 with `zune-jpeg` (pure Rust, SIMD on x86).

use std::fmt;

use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions},
};

/// What a JPEG file's header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The start-of-frame marker's second byte: `0xC0` baseline.
    pub sof: u8,
    /// Sample precision, bits.
    pub precision: u8,
    /// Width in pixels.
    pub width: u16,
    /// Height in lines.
    pub height: u16,
    /// Colour components: 1 greyscale.
    pub components: u8,
}

/// Why a frame is not a JPEG the viewer shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JpegError {
    /// No SOI at the start or no EOI at the end.
    Framing,
    /// The segments end before a start of frame and a start of scan.
    Truncated,
    /// Not baseline 8-bit greyscale.
    NotBaselineGrey(Header),
    /// Another size than the frame's.
    Size {
        /// The header's size.
        header: (u16, u16),
        /// The frame's size.
        frame: (u16, u16),
    },
    /// The decoder refused the data.
    Decode(String),
}

impl fmt::Display for JpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framing => f.write_str("no SOI at the start or no EOI at the end"),
            Self::Truncated => f.write_str("headers end before the start of frame and of scan"),
            Self::NotBaselineGrey(h) => write!(
                f,
                "not baseline 8-bit greyscale: SOF 0x{:02X}, {} bits, {} components",
                h.sof, h.precision, h.components
            ),
            Self::Size { header, frame } => write!(
                f,
                "picture {}x{}, frame {}x{}",
                header.0, header.1, frame.0, frame.1
            ),
            Self::Decode(e) => write!(f, "decoder: {e}"),
        }
    }
}

impl std::error::Error for JpegError {}

/// Reads the markers from SOI to the start of scan: the start of frame's values. Checks SOI
/// and EOI.
pub fn header(data: &[u8]) -> Result<Header, JpegError> {
    if data.len() < 4 || data[..2] != [0xFF, 0xD8] || data[data.len() - 2..] != [0xFF, 0xD9] {
        return Err(JpegError::Framing);
    }
    let mut at = 2;
    let mut sof = None;
    loop {
        // Fill bytes (0xFF) may precede a marker.
        while data.get(at) == Some(&0xFF) && data.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        let (Some(&0xFF), Some(&m)) = (data.get(at), data.get(at + 1)) else {
            return Err(JpegError::Truncated);
        };
        at += 2;
        if m == 0x01 || (0xD0..=0xD7).contains(&m) {
            continue;
        }
        let len = match data.get(at..at + 2) {
            Some(l) => u16::from_be_bytes([l[0], l[1]]) as usize,
            None => return Err(JpegError::Truncated),
        };
        let Some(seg) = data.get(at + 2..at + len.max(2)) else {
            return Err(JpegError::Truncated);
        };
        let is_sof = matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if seg.len() < 6 {
                return Err(JpegError::Truncated);
            }
            sof = Some(Header {
                sof: m,
                precision: seg[0],
                height: u16::from_be_bytes([seg[1], seg[2]]),
                width: u16::from_be_bytes([seg[3], seg[4]]),
                components: seg[5],
            });
        }
        if m == 0xDA {
            return sof.ok_or(JpegError::Truncated);
        }
        at += len;
    }
}

/// Checks that `data` is one baseline 8-bit greyscale JPEG of `width` x `height` and decodes
/// it into `out` (resized to `width * height`, RAW8 rows top to bottom).
pub fn decode(data: &[u8], width: u16, height: u16, out: &mut Vec<u8>) -> Result<(), JpegError> {
    let h = header(data)?;
    if (h.sof, h.precision, h.components) != (0xC0, 8, 1) {
        return Err(JpegError::NotBaselineGrey(h));
    }
    if (h.width, h.height) != (width, height) {
        return Err(JpegError::Size {
            header: (h.width, h.height),
            frame: (width, height),
        });
    }
    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .jpeg_set_out_colorspace(ColorSpace::Luma);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), options);
    out.resize(width as usize * height as usize, 0);
    d.decode_into(out)
        .map_err(|e| JpegError::Decode(format!("{e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(w: u16, h: u16, q: u8) -> (Vec<u8>, Vec<u8>) {
        let raw: Vec<u8> = (0..w as usize * h as usize)
            .map(|i| ((i % w as usize) / 4 + (i / w as usize) / 2) as u8)
            .collect();
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, q)
            .encode(&raw, w, h, jpeg_encoder::ColorType::Luma)
            .unwrap();
        (raw, out)
    }

    #[test]
    fn decodes_a_greyscale_baseline_file() {
        let (raw, file) = encode(320, 200, 95);
        let h = header(&file).unwrap();
        assert_eq!(
            (h.sof, h.precision, h.width, h.height, h.components),
            (0xC0, 8, 320, 200, 1)
        );
        let mut out = Vec::new();
        decode(&file, 320, 200, &mut out).unwrap();
        assert_eq!(out.len(), raw.len());
        let worst = out
            .iter()
            .zip(&raw)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 4, "worst error {worst}");
    }

    #[test]
    fn refuses_other_files() {
        let (_, file) = encode(64, 64, 90);
        let mut out = Vec::new();
        assert_eq!(
            decode(&file, 64, 72, &mut out),
            Err(JpegError::Size {
                header: (64, 64),
                frame: (64, 72)
            })
        );
        assert_eq!(
            decode(&file[..file.len() - 1], 64, 64, &mut out),
            Err(JpegError::Framing)
        );
        assert_eq!(
            decode(&file[1..], 64, 64, &mut out),
            Err(JpegError::Framing)
        );
        // Cut after the headers but with an EOI: the decoder refuses it.
        let mut cut = file[..file.len() / 2].to_vec();
        cut.extend_from_slice(&[0xFF, 0xD9]);
        assert!(decode(&cut, 64, 64, &mut out).is_err());
        // Progressive is not baseline.
        let mut prog = Vec::new();
        let raw = vec![128u8; 64 * 64];
        let mut e = jpeg_encoder::Encoder::new(&mut prog, 90);
        e.set_progressive(true);
        e.encode(&raw, 64, 64, jpeg_encoder::ColorType::Luma)
            .unwrap();
        assert!(matches!(
            decode(&prog, 64, 64, &mut out),
            Err(JpegError::NotBaselineGrey(Header { sof: 0xC2, .. }))
        ));
    }
}
