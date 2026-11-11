//! The stream settings as the window edits them, and what is sent of them.
//!
//! With automatic quality the board chooses the JPEG quality, so the quality the user sets
//! applies only with automatic quality off. On the wire `jpeg_quality` is then the ceiling of
//! the automatic quality (`espion_proto::cam::auto_quality`): the viewer sends
//! [`AUTO_QUALITY_CEILING`] there and keeps the user's quality apart, for when automatic quality
//! is turned off again.

use espion_proto::cam::{Encoding, StreamSettings};

/// The JPEG quality automatic quality may reach: the board's start-up quality. Above it each
/// step makes the files 30 to 100 % larger.
pub const AUTO_QUALITY_CEILING: u8 = 90;

/// The stream controls of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamEdit {
    /// Send frames.
    pub enabled: bool,
    /// Pixel encoding on the wire.
    pub encoding: Encoding,
    /// Most frames per second (0: no limit).
    pub max_fps: u16,
    /// The JPEG quality the user set: sent with automatic quality off, kept while it is on.
    pub manual_quality: u8,
    /// Automatic JPEG quality: the board chooses, up to [`AUTO_QUALITY_CEILING`].
    pub auto_quality: bool,
}

impl StreamEdit {
    /// The controls showing `s`, its quality as the manual quality.
    pub const fn new(s: StreamSettings) -> Self {
        Self {
            enabled: s.enabled,
            encoding: s.encoding,
            max_fps: s.max_fps,
            manual_quality: s.jpeg_quality,
            auto_quality: s.auto_quality,
        }
    }

    /// What is sent: the manual quality with automatic quality off, the ceiling with it on.
    pub const fn settings(&self) -> StreamSettings {
        StreamSettings {
            enabled: self.enabled,
            encoding: self.encoding,
            max_fps: self.max_fps,
            jpeg_quality: if self.auto_quality {
                AUTO_QUALITY_CEILING
            } else {
                self.manual_quality
            },
            auto_quality: self.auto_quality,
        }
    }

    /// Takes the board's stream settings into the controls. Its quality is a manual quality only
    /// with automatic quality off; with it on, it is the ceiling and the manual quality stays.
    pub const fn adopt(&mut self, s: StreamSettings) {
        self.enabled = s.enabled;
        self.encoding = s.encoding;
        self.max_fps = s.max_fps;
        self.auto_quality = s.auto_quality;
        if !s.auto_quality {
            self.manual_quality = s.jpeg_quality;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual(q: u8) -> StreamEdit {
        StreamEdit::new(StreamSettings {
            enabled: true,
            encoding: Encoding::Jpeg,
            max_fps: 0,
            jpeg_quality: q,
            auto_quality: false,
        })
    }

    #[test]
    fn automatic_sends_the_ceiling_whatever_the_manual_quality() {
        for q in [1, 50, 90, 100] {
            let s = StreamEdit {
                auto_quality: true,
                ..manual(q)
            }
            .settings();
            assert_eq!(
                (s.jpeg_quality, s.auto_quality),
                (AUTO_QUALITY_CEILING, true)
            );
        }
    }

    #[test]
    fn the_manual_quality_survives_automatic_on_and_off() {
        let mut e = manual(1);
        assert_eq!(e.settings().jpeg_quality, 1);
        e.auto_quality = true;
        assert_eq!(e.settings().jpeg_quality, AUTO_QUALITY_CEILING);
        e.auto_quality = false;
        assert_eq!(e.settings(), manual(1).settings());
    }

    #[test]
    fn a_reported_ceiling_is_not_a_manual_quality() {
        let mut e = manual(1);
        e.adopt(StreamSettings {
            enabled: true,
            encoding: Encoding::Raw8,
            max_fps: 30,
            jpeg_quality: AUTO_QUALITY_CEILING,
            auto_quality: true,
        });
        assert_eq!(
            e,
            StreamEdit {
                encoding: Encoding::Raw8,
                max_fps: 30,
                auto_quality: true,
                ..manual(1)
            }
        );
        // A board with automatic quality off reports a manual quality.
        e.adopt(manual(40).settings());
        assert_eq!(e, manual(40));
    }

    #[test]
    fn switching_the_encoding_keeps_automatic_quality_at_the_ceiling() {
        let mut e = StreamEdit {
            auto_quality: true,
            ..manual(1)
        };
        for encoding in [Encoding::Raw8, Encoding::Raw10, Encoding::Jpeg] {
            e.encoding = encoding;
            let s = e.settings();
            assert_eq!(
                (s.encoding, s.jpeg_quality, s.auto_quality),
                (encoding, AUTO_QUALITY_CEILING, true)
            );
        }
    }
}
