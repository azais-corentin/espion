//! Camera stream messages: round trips at their boundaries and rejection of foreign datagrams.

use espion_proto::{
    Error, HEADER_LEN, MAGIC, VERSION,
    cam::{
        AUTO_QUALITY_MIN, AUTO_QUALITY_ROOM_PCT, CHUNK_MAX, CamError, CameraSettings, Counters,
        EncoderState, Encoding, Format, FrameChunk, FrameTiming, JPEG_ALIGN, JPEG_FRAME_MAX,
        JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, JpegRefusal, MAX_DATAGRAM, Message, ModeInfo, Op,
        Origin, Readout, Reply, Request, Response, Span, State, StreamSettings, TestPattern,
        auto_quality, chunk_count, ijg_scale,
    },
    kind,
};

fn camera(max: bool) -> CameraSettings {
    if max {
        CameraSettings {
            format: Format::Raw10,
            width: u16::MAX,
            height: u16::MAX,
            readout: Readout::Binned,
            origin: Some(Origin {
                x: u16::MAX,
                y: u16::MAX,
            }),
            rate_mhz: u32::MAX,
            tolerance_mhz: u32::MAX,
            exposure_lines: u16::MAX,
            gain_sixteenths: u8::MAX,
            test_pattern: TestPattern::Random,
        }
    } else {
        CameraSettings {
            format: Format::Raw8,
            width: 0,
            height: 0,
            readout: Readout::Crop,
            origin: None,
            rate_mhz: 0,
            tolerance_mhz: 0,
            exposure_lines: 0,
            gain_sixteenths: 0,
            test_pattern: TestPattern::Off,
        }
    }
}

fn state(max: bool) -> State {
    let m = |v: u32| if max { v } else { 0 };
    State {
        camera: camera(max),
        stream: StreamSettings {
            enabled: max,
            encoding: if max { Encoding::Jpeg } else { Encoding::Raw8 },
            max_fps: m(u16::MAX as u32) as u16,
            jpeg_quality: m(u8::MAX as u32) as u8,
            auto_quality: max,
        },
        mode: ModeInfo {
            sensor_clock_mhz: m(u8::MAX as u32) as u8,
            exceeds_datasheet_clock: max,
            hts: m(u16::MAX as u32) as u16,
            vts: m(u16::MAX as u32) as u16,
            psv: max,
            frame_rate_mhz: m(u32::MAX),
            rate_error_mhz: if max { i32::MIN } else { 0 },
            max_exposure_lines: m(u16::MAX as u32) as u16,
            line_ns: m(u32::MAX),
            frame_bytes: m(u32::MAX),
        },
        generation: m(u32::MAX),
        counters: Counters {
            frames_captured: m(u32::MAX),
            frames_sent: m(u32::MAX),
            frames_skipped: m(u32::MAX),
            capture_errors: m(u32::MAX),
            datagrams_sent: m(u32::MAX),
        },
        encoder: EncoderState {
            quality: m(u8::MAX as u32) as u8,
            frame_len: m(u32::MAX),
            encode_us: m(u32::MAX),
            errors: m(u32::MAX),
            frames_skipped: m(u32::MAX),
        },
        uptime_us: if max { u64::MAX } else { 0 },
        timing: max.then_some(FrameTiming {
            frame: u32::MAX,
            encode: Some(Span {
                start_us: u32::MAX,
                end_us: u32::MAX,
            }),
            send: Span {
                start_us: u32::MAX,
                end_us: u32::MAX,
            },
        }),
    }
}

fn every_error() -> [CamError; 16] {
    [
        CamError::Size {
            max_width: 1280,
            max_height: u16::MAX,
        },
        CamError::Alignment {
            width_multiple: 64,
            height_multiple: u8::MAX,
        },
        CamError::Window,
        CamError::TooFast { max_mhz: u32::MAX },
        CamError::TooSlow { min_mhz: 419 },
        CamError::Tolerance {
            nearest_mhz: 120_000,
        },
        CamError::Receiver,
        CamError::SensorBus,
        CamError::Exposure {
            max_lines: u16::MAX,
        },
        CamError::Gain { min: 16, max: 248 },
        CamError::Encoding {
            format: Format::Raw8,
            encoding: Encoding::Raw10,
        },
        CamError::Encoding {
            format: Format::Raw10,
            encoding: Encoding::Jpeg,
        },
        CamError::Jpeg {
            reason: JpegRefusal::Format,
            format: Format::Raw10,
            width: 1280,
            height: 800,
        },
        CamError::Jpeg {
            reason: JpegRefusal::Size,
            format: Format::Raw8,
            width: u16::MAX,
            height: u16::MAX,
        },
        CamError::Quality { min: 0, max: 0 },
        CamError::Quality {
            min: JPEG_QUALITY_MIN,
            max: JPEG_QUALITY_MAX,
        },
    ]
}

/// Encodes into a datagram-sized buffer, checks the header, decodes and compares.
fn round_trip(msg: Message<'_>) -> usize {
    let mut buf = [0u8; MAX_DATAGRAM];
    let len = msg
        .encode(&mut buf)
        .expect("encodes into one datagram")
        .len();
    assert_eq!(buf[..2], MAGIC);
    assert_eq!(buf[2], VERSION);
    assert_eq!(Message::decode(&buf[..len]), Ok(msg));
    len
}

#[test]
fn requests_round_trip() {
    for seq in [0, 1, u32::MAX] {
        for op in [
            Op::GetState,
            Op::Poll,
            Op::SetCamera(camera(false)),
            Op::SetCamera(camera(true)),
            Op::SetStream(StreamSettings {
                enabled: false,
                encoding: Encoding::Raw8,
                max_fps: 0,
                jpeg_quality: 0,
                auto_quality: false,
            }),
            Op::SetStream(StreamSettings {
                enabled: true,
                encoding: Encoding::Raw10,
                max_fps: u16::MAX,
                jpeg_quality: u8::MAX,
                auto_quality: true,
            }),
            Op::SetStream(StreamSettings {
                enabled: true,
                encoding: Encoding::Jpeg,
                max_fps: 120,
                jpeg_quality: 90,
                auto_quality: true,
            }),
        ] {
            round_trip(Message::Request(Request { seq, op }));
        }
    }
}

#[test]
fn every_pattern_and_readout_round_trips() {
    for test_pattern in [
        TestPattern::Off,
        TestPattern::Bars,
        TestPattern::RollingBars,
        TestPattern::GreySquares,
        TestPattern::BlackWhiteSquares,
        TestPattern::Random,
    ] {
        for readout in [Readout::Crop, Readout::Binned] {
            let c = CameraSettings {
                test_pattern,
                readout,
                ..camera(true)
            };
            round_trip(Message::Request(Request {
                seq: 7,
                op: Op::SetCamera(c),
            }));
        }
    }
}

#[test]
fn states_round_trip_and_fit_a_datagram() {
    for max in [false, true] {
        let len = round_trip(Message::Response(Response {
            seq: u32::MAX,
            reply: Reply::State(state(max)),
        }));
        assert!(len <= MAX_DATAGRAM);
    }
}

#[test]
fn every_error_round_trips() {
    for e in every_error() {
        round_trip(Message::Response(Response {
            seq: 3,
            reply: Reply::Error(e),
        }));
    }
}

/// A chunk whose every header field takes its largest varint, at a valid place in a valid
/// frame (RAW8 65535x65535: the frame length needs 5 bytes too).
fn largest_chunk(data: &[u8]) -> FrameChunk<'_> {
    let frame_len = Encoding::Raw8.frame_bytes(u16::MAX, u16::MAX).unwrap() as u32;
    let offset = (frame_len / CHUNK_MAX as u32 - 1) * CHUNK_MAX as u32;
    assert!(offset >= 1 << 28, "offset needs 5 varint bytes");
    FrameChunk {
        frame: u32::MAX,
        generation: u32::MAX,
        encoding: Encoding::Raw8,
        width: u16::MAX,
        height: u16::MAX,
        timestamp_us: u64::MAX,
        frame_len,
        offset,
        data,
    }
}

#[test]
fn largest_chunk_exactly_fills_the_datagram() {
    let data: Vec<u8> = (0..CHUNK_MAX).map(|i| (i * 7) as u8).collect();
    let len = round_trip(Message::Chunk(largest_chunk(&data)));
    assert_eq!(len, MAX_DATAGRAM);
    assert_eq!(MAX_DATAGRAM, 1452);
}

#[test]
fn chunk_longer_than_the_maximum_is_refused() {
    let data = vec![0u8; CHUNK_MAX + 1];
    let mut buf = [0u8; 2 * MAX_DATAGRAM];
    assert_eq!(
        largest_chunk(&data).encode(&mut buf),
        Err(Error::ChunkTooLong)
    );
}

/// The chunk's bytes as serde derives them from `FrameChunk` (the decoder's view), after the
/// header: `encode`, and `encode_header` with the data put after it, give exactly these.
#[test]
fn header_and_data_written_apart_equal_the_derived_encoding() {
    let data: Vec<u8> = (0..CHUNK_MAX).map(|i| (i * 13 + 5) as u8).collect();
    let small = FrameChunk {
        frame: 3,
        generation: 1,
        encoding: Encoding::Jpeg,
        width: 1280,
        height: 800,
        timestamp_us: 123_456,
        frame_len: 50_000,
        offset: CHUNK_MAX as u32,
        data: &data,
    };
    for len in [0, 1, 2, 3, 4, 127, 128, 129, 1000, CHUNK_MAX] {
        for chunk in [
            largest_chunk(&data[..len]),
            FrameChunk {
                data: &data[..len],
                ..small
            },
        ] {
            let mut derived = [0u8; MAX_DATAGRAM];
            derived[..HEADER_LEN].copy_from_slice(&[
                MAGIC[0],
                MAGIC[1],
                VERSION,
                kind::FRAME_CHUNK,
            ]);
            let body = postcard::to_slice(&chunk, &mut derived[HEADER_LEN..])
                .unwrap()
                .len();
            let derived = &derived[..HEADER_LEN + body];

            let mut whole = [0u8; MAX_DATAGRAM];
            assert_eq!(
                chunk.encode(&mut whole).unwrap(),
                derived,
                "encode, {len} bytes"
            );

            let mut apart = [0xA5u8; MAX_DATAGRAM];
            let n = chunk.encode_header(&mut apart).unwrap();
            apart[n..n + len].copy_from_slice(chunk.data);
            assert_eq!(&apart[..n + len], derived, "encode_header, {len} bytes");
        }
    }
}

#[test]
fn header_without_room_for_the_data_is_refused() {
    let data = [7u8; 100];
    let chunk = largest_chunk(&data);
    let mut buf = [0u8; MAX_DATAGRAM];
    let n = chunk.encode_header(&mut buf).unwrap();
    assert_eq!(chunk.encode_header(&mut buf[..n + 100]), Ok(n));
    assert_eq!(
        chunk.encode_header(&mut buf[..n + 99]),
        Err(Error::BufferFull)
    );
    let long = [0u8; CHUNK_MAX + 1];
    assert_eq!(
        largest_chunk(&long).encode_header(&mut buf),
        Err(Error::ChunkTooLong)
    );
}

#[test]
fn a_frame_cut_into_chunks_round_trips() {
    // RAW10 1280x800 and RAW8 640x400: whole chunks and a short last one.
    for (encoding, width, height) in [(Encoding::Raw10, 1280, 800), (Encoding::Raw8, 640, 400)] {
        let frame_len = encoding.frame_bytes(width, height).unwrap() as u32;
        let frame: Vec<u8> = (0..frame_len).map(|i| (i % 251) as u8).collect();
        let mut rebuilt = vec![0u8; frame.len()];
        let n = chunk_count(frame_len);
        assert!(
            !frame_len.is_multiple_of(CHUNK_MAX as u32),
            "the last chunk is short"
        );
        for (i, data) in frame.chunks(CHUNK_MAX).enumerate() {
            let chunk = FrameChunk {
                frame: 41,
                generation: 2,
                encoding,
                width,
                height,
                timestamp_us: 1_000_000,
                frame_len,
                offset: (i * CHUNK_MAX) as u32,
                data,
            };
            let mut buf = [0u8; MAX_DATAGRAM];
            let dgram = chunk.encode(&mut buf).unwrap();
            let Ok(Message::Chunk(c)) = Message::decode(dgram) else {
                panic!("chunk {i} does not decode");
            };
            assert_eq!(c.index(), i as u32);
            rebuilt[c.offset as usize..][..c.data.len()].copy_from_slice(c.data);
            assert!(i < n as usize);
        }
        assert_eq!(rebuilt, frame);
    }
}

#[test]
fn chunks_off_the_grid_are_rejected() {
    let (w, h) = (640u16, 400u16);
    let frame_len = Encoding::Raw8.frame_bytes(w, h).unwrap() as u32;
    let data = vec![1u8; CHUNK_MAX];
    let good = FrameChunk {
        frame: 1,
        generation: 1,
        encoding: Encoding::Raw8,
        width: w,
        height: h,
        timestamp_us: 5,
        frame_len,
        offset: 0,
        data: &data,
    };
    let last_len = frame_len as usize % CHUNK_MAX;
    let last_offset = frame_len - last_len as u32;
    let bad = [
        FrameChunk { offset: 1, ..good },
        FrameChunk {
            data: &data[..CHUNK_MAX - 1],
            ..good
        },
        FrameChunk {
            frame_len: frame_len + 1,
            ..good
        },
        FrameChunk {
            encoding: Encoding::Raw10,
            ..good
        },
        FrameChunk {
            offset: last_offset,
            ..good
        },
        FrameChunk {
            offset: last_offset + CHUNK_MAX as u32,
            data: &data[..0],
            ..good
        },
    ];
    let mut buf = [0u8; MAX_DATAGRAM];
    assert!(Message::decode(good.encode(&mut buf).unwrap()).is_ok());
    let last = FrameChunk {
        offset: last_offset,
        data: &data[..last_len],
        ..good
    };
    assert!(Message::decode(last.encode(&mut buf).unwrap()).is_ok());
    for c in bad {
        let dgram = c.encode(&mut buf).unwrap();
        assert_eq!(Message::decode(dgram), Err(Error::ChunkLayout), "{c:?}");
    }
}

/// Chunk `index` of a JPEG frame of `frame_len` bytes at 1280x800, its data cut from `file`.
fn jpeg_chunk(file: &[u8], frame_len: u32, index: u32) -> FrameChunk<'_> {
    let offset = index * CHUNK_MAX as u32;
    let end = (offset + CHUNK_MAX as u32).min(frame_len);
    FrameChunk {
        frame: 9,
        generation: 4,
        encoding: Encoding::Jpeg,
        width: 1280,
        height: 800,
        timestamp_us: 77,
        frame_len,
        offset,
        data: &file[offset as usize..end as usize],
    }
}

#[test]
fn a_jpeg_frame_of_the_largest_length_round_trips() {
    // Every length from one byte to JPEG_FRAME_MAX is accepted; each frame's chunks rebuild it.
    let file: Vec<u8> = (0..JPEG_FRAME_MAX).map(|i| (i % 253) as u8).collect();
    for frame_len in [
        1,
        2,
        CHUNK_MAX as u32,
        CHUNK_MAX as u32 + 1,
        45_123,
        JPEG_FRAME_MAX,
    ] {
        let mut rebuilt = vec![0u8; frame_len as usize];
        for i in 0..chunk_count(frame_len) {
            let c = jpeg_chunk(&file, frame_len, i);
            let mut buf = [0u8; MAX_DATAGRAM];
            let dgram = c.encode(&mut buf).unwrap();
            assert!(dgram.len() <= MAX_DATAGRAM);
            let Ok(Message::Chunk(back)) = Message::decode(dgram) else {
                panic!("JPEG chunk {i} of {frame_len} does not decode");
            };
            assert_eq!(back, c);
            rebuilt[back.offset as usize..][..back.data.len()].copy_from_slice(back.data);
        }
        assert_eq!(rebuilt, file[..frame_len as usize]);
    }
}

#[test]
fn a_jpeg_frame_one_byte_too_long_is_rejected() {
    let file = vec![0xA5u8; JPEG_FRAME_MAX as usize + 1];
    let mut buf = [0u8; MAX_DATAGRAM];
    for i in [0, chunk_count(JPEG_FRAME_MAX + 1) - 1] {
        let c = jpeg_chunk(&file, JPEG_FRAME_MAX + 1, i);
        let dgram = c.encode(&mut buf).unwrap();
        assert_eq!(Message::decode(dgram), Err(Error::ChunkLayout), "chunk {i}");
    }
}

#[test]
fn jpeg_chunks_that_disagree_with_their_frame_are_rejected() {
    let frame_len = 45_123u32;
    let file = vec![7u8; frame_len as usize];
    let good = jpeg_chunk(&file, frame_len, 3);
    let last = jpeg_chunk(&file, frame_len, chunk_count(frame_len) - 1);
    let mut buf = [0u8; MAX_DATAGRAM];
    assert!(Message::decode(good.encode(&mut buf).unwrap()).is_ok());
    assert!(Message::decode(last.encode(&mut buf).unwrap()).is_ok());
    let bad = [
        // Length disagrees with the offset and frame_len: short in the middle, long at the end.
        FrameChunk {
            data: &file[..CHUNK_MAX - 1],
            ..good
        },
        FrameChunk {
            data: &file[..last.data.len() + 1],
            ..last
        },
        FrameChunk {
            data: &file[..last.data.len() - 1],
            ..last
        },
        // Offset off the grid, or past the end.
        FrameChunk {
            offset: good.offset + 1,
            ..good
        },
        FrameChunk {
            offset: chunk_count(frame_len) * CHUNK_MAX as u32,
            data: &file[..0],
            ..good
        },
        // An empty frame, and sizes JPEG cannot have.
        FrameChunk {
            frame_len: 0,
            offset: 0,
            data: &file[..0],
            ..good
        },
        FrameChunk {
            width: 1284,
            ..good
        },
        FrameChunk {
            height: 802,
            ..good
        },
        FrameChunk { width: 0, ..good },
    ];
    for c in bad {
        let dgram = c.encode(&mut buf).unwrap();
        assert_eq!(Message::decode(dgram), Err(Error::ChunkLayout), "{c:?}");
    }
}

#[test]
fn frame_lengths_by_encoding() {
    assert_eq!(Encoding::Raw8.frame_bytes(1280, 800), Some(1_024_000));
    assert_eq!(Encoding::Raw10.frame_bytes(1280, 800), Some(1_280_000));
    assert_eq!(Encoding::Jpeg.frame_bytes(1280, 800), None);
    assert_eq!(Encoding::Jpeg.line_bytes(1280), None);
    assert!(Encoding::Jpeg.frame_len_ok(1280, 800, 1));
    assert!(Encoding::Jpeg.frame_len_ok(8, 8, JPEG_FRAME_MAX));
    assert!(!Encoding::Jpeg.frame_len_ok(1280, 800, JPEG_FRAME_MAX + 1));
    assert!(!Encoding::Jpeg.frame_len_ok(1280, 800, 0));
    assert!(Encoding::Raw8.frame_len_ok(1280, 800, 1_024_000));
    assert!(!Encoding::Raw8.frame_len_ok(1280, 800, 45_000));
}

#[test]
fn encodings_are_checked_against_the_mode() {
    assert_eq!(Encoding::Raw8.check(Format::Raw10, 1280, 800), Ok(()));
    assert_eq!(Encoding::Raw10.check(Format::Raw10, 1280, 800), Ok(()));
    assert_eq!(
        Encoding::Raw10.check(Format::Raw8, 1280, 800),
        Err(CamError::Encoding {
            format: Format::Raw8,
            encoding: Encoding::Raw10
        })
    );
    assert_eq!(Encoding::Jpeg.check(Format::Raw8, 1280, 800), Ok(()));
    assert_eq!(Encoding::Jpeg.check(Format::Raw8, 128, 8), Ok(()));
    // The format is checked first.
    assert_eq!(
        Encoding::Jpeg.check(Format::Raw10, 1280, 802),
        Err(CamError::Jpeg {
            reason: JpegRefusal::Format,
            format: Format::Raw10,
            width: 1280,
            height: 802
        })
    );
    for (w, h) in [(1280, 802), (1284, 800), (1288, 4)] {
        assert_eq!(
            Encoding::Jpeg.check(Format::Raw8, w, h),
            Err(CamError::Jpeg {
                reason: JpegRefusal::Size,
                format: Format::Raw8,
                width: w,
                height: h
            })
        );
    }
    assert_eq!(JPEG_ALIGN, 8);
    let shown = CamError::Jpeg {
        reason: JpegRefusal::Size,
        format: Format::Raw8,
        width: 1280,
        height: 802,
    }
    .to_string();
    assert!(
        shown.contains("1280x802") && shown.contains("multiples of 8"),
        "{shown}"
    );
    let shown = CamError::Jpeg {
        reason: JpegRefusal::Format,
        format: Format::Raw10,
        width: 1280,
        height: 800,
    }
    .to_string();
    assert!(shown.contains("RAW8") && shown.contains("Raw10"), "{shown}");
}

#[test]
fn automatic_quality_steps() {
    // Down by an eighth (at least 2) while frames are skipped, to the floor and never up.
    assert_eq!(auto_quality(90, 90, true, 100), 79);
    assert_eq!(auto_quality(90, 90, true, 0), 79);
    assert_eq!(auto_quality(16, 90, true, 100), 14);
    assert_eq!(auto_quality(11, 90, true, 100), AUTO_QUALITY_MIN);
    assert_eq!(
        auto_quality(AUTO_QUALITY_MIN, 90, true, 100),
        AUTO_QUALITY_MIN
    );
    assert_eq!(auto_quality(5, 5, true, 100), 5);
    assert_eq!(auto_quality(4, 50, true, 100), 4);
    // Up by 1 towards the set quality only with room on the link for the step's growth
    // (busy * S(q) < 80 * S(q + 1)); hold otherwise.
    assert_eq!(auto_quality(30, 90, false, 76), 31); // S 166 -> 161: below 77.6 %
    assert_eq!(auto_quality(30, 90, false, 78), 30);
    assert_eq!(auto_quality(79, 90, false, 0), 80);
    assert_eq!(auto_quality(79, 90, false, 76), 80); // S 42 -> 40: below 76.2 %
    assert_eq!(auto_quality(79, 90, false, 77), 79);
    assert_eq!(auto_quality(79, 90, false, AUTO_QUALITY_ROOM_PCT), 79);
    assert_eq!(auto_quality(79, 90, false, 100), 79);
    assert_eq!(auto_quality(89, 100, false, 72), 90); // S 22 -> 20: below 72.7 %
    assert_eq!(auto_quality(89, 100, false, 73), 89);
    assert_eq!(auto_quality(98, 100, false, 39), 99); // S 4 -> 2: below 40 %
    assert_eq!(auto_quality(98, 100, false, 40), 98);
    assert_eq!(auto_quality(99, 100, false, 39), 100); // S 2 -> 1
    assert_eq!(auto_quality(90, 90, false, 0), 90);
    assert_eq!(
        (ijg_scale(0), ijg_scale(1), ijg_scale(10), ijg_scale(49)),
        (5000, 5000, 500, 102)
    );
    assert_eq!(
        (ijg_scale(50), ijg_scale(90), ijg_scale(99), ijg_scale(100)),
        (100, 20, 2, 1)
    );
    // Never above the set quality.
    assert_eq!(auto_quality(95, 90, false, 0), 90);
    assert_eq!(auto_quality(95, 90, true, 100), 90);
    // From 90 to the floor and back.
    let mut q = 90;
    let mut down = 0;
    while q > AUTO_QUALITY_MIN {
        q = auto_quality(q, 90, true, 100);
        down += 1;
    }
    assert_eq!(down, 18);
    let mut up = 0;
    while q < 90 {
        q = auto_quality(q, 90, false, 50);
        up += 1;
    }
    assert_eq!(up, 80);
}

#[test]
fn stream_settings_are_checked_against_the_mode() {
    let s = StreamSettings {
        enabled: true,
        encoding: Encoding::Jpeg,
        max_fps: 0,
        jpeg_quality: 90,
        auto_quality: true,
    };
    assert_eq!(s.check(Format::Raw8, 1280, 800), Ok(()));
    let quality = Err(CamError::Quality {
        min: JPEG_QUALITY_MIN,
        max: JPEG_QUALITY_MAX,
    });
    for q in [0, 101, u8::MAX] {
        let bad = StreamSettings {
            jpeg_quality: q,
            ..s
        };
        assert_eq!(bad.check(Format::Raw8, 1280, 800), quality, "{q}");
        // Checked whatever the encoding.
        let raw = StreamSettings {
            encoding: Encoding::Raw8,
            ..bad
        };
        assert_eq!(raw.check(Format::Raw8, 1280, 800), quality, "{q}");
    }
    for q in [JPEG_QUALITY_MIN, JPEG_QUALITY_MAX] {
        let ok = StreamSettings {
            jpeg_quality: q,
            ..s
        };
        assert_eq!(ok.check(Format::Raw8, 1280, 800), Ok(()));
    }
    // The encoding first.
    let both = StreamSettings {
        jpeg_quality: 0,
        ..s
    };
    assert_eq!(
        both.check(Format::Raw10, 1280, 800),
        Err(CamError::Jpeg {
            reason: JpegRefusal::Format,
            format: Format::Raw10,
            width: 1280,
            height: 800
        })
    );
}

fn poll_datagram() -> Vec<u8> {
    let mut buf = [0u8; MAX_DATAGRAM];
    Request {
        seq: 9,
        op: Op::Poll,
    }
    .encode(&mut buf)
    .unwrap()
    .to_vec()
}

#[test]
fn wrong_magic_version_or_kind_is_rejected() {
    let good = poll_datagram();
    assert!(Message::decode(&good).is_ok());

    let mut d = good.clone();
    d[0] = b'X';
    assert_eq!(Message::decode(&d), Err(Error::Magic));

    for v in [0, 1, VERSION + 1, u8::MAX] {
        let mut d = good.clone();
        d[2] = v;
        assert_eq!(Message::decode(&d), Err(Error::Version(v)));
    }

    // The draft and reserved kinds stay unimplemented; unknown kinds are refused.
    for k in [
        kind::CENTROIDS,
        kind::STATUS,
        kind::RESERVED_ANNOUNCE,
        kind::CONFIG,
        kind::FRAME_EPOCH,
        0x00,
        0x23,
        0xFF,
    ] {
        let mut d = good.clone();
        d[3] = k;
        assert_eq!(Message::decode(&d), Err(Error::Kind(k)));
    }

    // A request body under a response kind does not decode as one.
    let mut d = good.clone();
    d[3] = kind::CAM_RESPONSE;
    assert_eq!(Message::decode(&d), Err(Error::Body));
}

#[test]
fn short_truncated_and_trailing_datagrams_are_rejected() {
    let good = poll_datagram();
    for n in 0..HEADER_LEN {
        assert_eq!(Message::decode(&good[..n]), Err(Error::Short));
    }
    assert_eq!(Message::decode(&good[..HEADER_LEN]), Err(Error::Body));
    let mut long = good.clone();
    long.push(0);
    assert_eq!(Message::decode(&long), Err(Error::Trailing));
}

#[test]
fn too_small_a_buffer_is_reported() {
    let req = Request {
        seq: u32::MAX,
        op: Op::SetCamera(camera(true)),
    };
    let mut buf = [0u8; MAX_DATAGRAM];
    let need = req.encode(&mut buf).unwrap().len();
    for n in [0, HEADER_LEN - 1, HEADER_LEN, need - 1] {
        assert_eq!(req.encode(&mut buf[..n]), Err(Error::BufferFull));
    }
    assert_eq!(req.encode(&mut buf[..need]).unwrap().len(), need);
}
