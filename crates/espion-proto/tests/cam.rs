//! Camera stream messages: round trips at their boundaries and rejection of foreign datagrams.

use espion_proto::{
    Error, HEADER_LEN, MAGIC, VERSION,
    cam::{
        AUTO_EXPOSURE_HIGH, AUTO_EXPOSURE_LOW, AUTO_MIN_AREA, AUTO_QUALITY_MIN,
        AUTO_QUALITY_ROOM_PCT, BLOB_BYTES, Blob, CHUNK_MAX, CamError, CameraSettings, Counters,
        DetectSettings, DetectState, DetectTiming, EncodeSpan, EncoderState, Encoding, Exposure,
        FRAME_BLOBS_MAX, Format, FrameBlobs, FrameChunk, FrameTiming, GAIN_MAX, GAIN_MIN,
        JPEG_ALIGN, JPEG_FRAME_MAX, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, JpegRefusal, MAX_DATAGRAM,
        Message, ModeInfo, Op, Origin, Readout, Reply, Request, Response, Span, State,
        StreamSettings, TestPattern, auto_exposure, auto_exposure_cap, auto_quality, chunk_count,
        ijg_scale, pack_blobs,
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
            auto_exposure: true,
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
            auto_exposure: false,
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
            encode: Some(EncodeSpan {
                // The longest i32 in zig-zag varint.
                start_us: i32::MIN,
                input_end_us: u32::MAX,
                end_us: u32::MAX,
            }),
            send: Span {
                start_us: u32::MAX,
                end_us: u32::MAX,
            },
        }),
        detect: DetectState {
            settings: DetectSettings {
                enabled: max,
                auto_threshold: max,
                threshold: m(u16::MAX as u32) as u16,
                auto_area: max,
                min_area: m(u16::MAX as u32) as u16,
                max_area: m(u16::MAX as u32) as u16,
            },
            threshold: m(u16::MAX as u32) as u16,
            background_median: m(u8::MAX as u32) as u8,
            background_sigma: m(u8::MAX as u32) as u8,
            detect_us_mean: m(u32::MAX),
            detect_us_max: m(u32::MAX),
            frames_detected: m(u32::MAX),
            frames_skipped: m(u32::MAX),
            frames_cut: m(u32::MAX),
            blobs_dropped: m(u32::MAX),
            blobs_filtered: m(u32::MAX),
            fault: max,
            available: max,
        },
    }
}

fn every_error() -> [CamError; 18] {
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
        CamError::Threshold { min: 1, max: 1023 },
        CamError::Area,
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
            Op::SetDetect(DetectSettings::DEFAULT),
            Op::SetDetect(DetectSettings {
                enabled: false,
                auto_threshold: false,
                threshold: u16::MAX,
                auto_area: false,
                min_area: u16::MAX,
                max_area: u16::MAX,
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
        0x24,
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

/// `n` blobs whose every field differs.
fn blobs(n: usize) -> Vec<Blob> {
    (0..n)
        .map(|i| Blob {
            x: (i as u32).wrapping_mul(0x0101_0097),
            y: u32::MAX - i as u32,
            width: (i as u16).wrapping_mul(311),
            flux: (i as u32).wrapping_mul(0x00F0_0F01),
            area: (i as u16).wrapping_mul(13).wrapping_add(1),
            peak: i as u8,
        })
        .collect()
}

fn frame_blobs(blobs: &[u8]) -> FrameBlobs<'_> {
    FrameBlobs {
        frame: u32::MAX,
        generation: u32::MAX,
        timestamp_us: u64::MAX,
        format: Format::Raw10,
        width: u16::MAX,
        height: u16::MAX,
        threshold: u16::MAX,
        detect_us: u32::MAX,
        timing: DetectTiming {
            strips_lead_us: u32::MAX,
            strips_end_us: u32::MAX,
            published_us: u32::MAX,
            queued_us: u32::MAX,
            measure_us: u16::MAX,
            strips: u16::MAX,
            strip_lines: u16::MAX,
            late_strips: u16::MAX,
            start_max_us: u16::MAX,
            detect_max_us: u16::MAX,
            copy_wait_max_us: u16::MAX,
            hold_max_us: u16::MAX,
            hold_budget_us: u16::MAX,
        },
        flags: u8::MAX,
        blobs,
    }
}

#[test]
fn blobs_round_trip_and_the_most_fit_a_datagram() {
    for n in [0, 1, 2, FRAME_BLOBS_MAX] {
        let want = blobs(n);
        let mut packed = vec![0u8; n * BLOB_BYTES];
        assert_eq!(
            pack_blobs(want.iter().copied(), &mut packed),
            (n * BLOB_BYTES, true)
        );
        let fb = frame_blobs(&packed);
        let len = round_trip(Message::Blobs(fb));
        assert!(len <= MAX_DATAGRAM, "{n} blobs take {len} bytes");
        assert_eq!(fb.blob_count(), n);
        assert_eq!(fb.blobs().collect::<Vec<_>>(), want);
    }
    // The largest message leaves no room for one more blob.
    let mut buf = [0u8; MAX_DATAGRAM];
    let packed = vec![0xFFu8; FRAME_BLOBS_MAX * BLOB_BYTES];
    let len = frame_blobs(&packed).encode(&mut buf).unwrap().len();
    assert!(len + BLOB_BYTES > MAX_DATAGRAM, "{len}");
    assert_eq!(FRAME_BLOBS_MAX, 80);
}

#[test]
fn packing_stops_at_the_room_given_and_the_datagram_limit() {
    let want = blobs(FRAME_BLOBS_MAX + 5);
    let mut small = [0u8; 3 * BLOB_BYTES + 2];
    assert_eq!(
        pack_blobs(want.iter().copied(), &mut small),
        (3 * BLOB_BYTES, false)
    );
    let mut big = vec![0u8; (FRAME_BLOBS_MAX + 5) * BLOB_BYTES];
    assert_eq!(
        pack_blobs(want.iter().copied(), &mut big),
        (FRAME_BLOBS_MAX * BLOB_BYTES, false)
    );
    let b = Blob {
        x: 0x1234_5678,
        y: 0x9ABC_DEF0,
        width: 0x0102,
        flux: 0x0304_0506,
        area: 0x0708,
        peak: 0x09,
    };
    assert_eq!(
        b.to_bytes(),
        [
            0x78, 0x56, 0x34, 0x12, 0xF0, 0xDE, 0xBC, 0x9A, 0x02, 0x01, 0x06, 0x05, 0x04, 0x03,
            0x08, 0x07, 0x09
        ]
    );
    let small = Blob {
        x: 640 * 256 + 64,
        y: 3 * 256 + 192,
        width: 384,
        ..b
    };
    assert_eq!(
        (small.x_px(), small.y_px(), small.width_px()),
        (640.25, 3.75, 1.5)
    );
    assert_eq!(Blob::from_bytes(b.to_bytes()), b);
}

#[test]
fn blobs_that_are_not_whole_or_too_many_are_rejected() {
    let mut buf = [0u8; MAX_DATAGRAM + 64];
    for len in [
        1,
        BLOB_BYTES - 1,
        BLOB_BYTES + 1,
        (FRAME_BLOBS_MAX + 1) * BLOB_BYTES,
    ] {
        let packed = vec![0u8; len];
        assert_eq!(
            frame_blobs(&packed).encode(&mut buf),
            Err(Error::BlobsLayout),
            "{len}"
        );
    }
    // A datagram built by hand with a broken blob length is refused on decode.
    let packed = vec![0u8; 2 * BLOB_BYTES];
    let good = frame_blobs(&packed).encode(&mut buf).unwrap().to_vec();
    assert!(Message::decode(&good).is_ok());
    let len_at = good.len() - 2 * BLOB_BYTES - 1;
    assert_eq!(good[len_at], (2 * BLOB_BYTES) as u8);
    let mut cut = good[..good.len() - 1].to_vec();
    cut[len_at] -= 1;
    assert_eq!(Message::decode(&cut), Err(Error::BlobsLayout));
}

#[test]
fn detection_settings_are_checked_against_the_format() {
    let s = DetectSettings::DEFAULT;
    assert_eq!(s.check(Format::Raw8), Ok(()));
    assert_eq!(s.check(Format::Raw10), Ok(()));
    for (format, max) in [(Format::Raw8, 255), (Format::Raw10, 1023)] {
        for t in [0, max + 1, u16::MAX] {
            let bad = DetectSettings { threshold: t, ..s };
            assert_eq!(
                bad.check(format),
                Err(CamError::Threshold { min: 1, max }),
                "{t}"
            );
            // Checked with the automatic threshold on too.
            assert!(bad.auto_threshold);
        }
        for t in [1, max] {
            assert_eq!(DetectSettings { threshold: t, ..s }.check(format), Ok(()));
        }
    }
    for (min_area, max_area) in [(0, 10), (11, 10)] {
        let bad = DetectSettings {
            min_area,
            max_area,
            ..s
        };
        assert_eq!(bad.check(Format::Raw8), Err(CamError::Area));
    }
    // The threshold first.
    let both = DetectSettings {
        threshold: 0,
        min_area: 0,
        ..s
    };
    assert_eq!(
        both.check(Format::Raw8),
        Err(CamError::Threshold { min: 1, max: 255 })
    );
}

#[test]
fn area_filter_and_format_change() {
    let manual = DetectSettings {
        auto_area: false,
        min_area: 3,
        max_area: 40,
        ..DetectSettings::DEFAULT
    };
    assert_eq!(manual.area_bounds(), (3, 40));
    assert!(!manual.keeps(2) && manual.keeps(3) && manual.keeps(40) && !manual.keeps(41));
    let auto = DetectSettings {
        auto_area: true,
        ..manual
    };
    assert_eq!(auto.area_bounds(), (AUTO_MIN_AREA, u16::MAX));
    assert!(!auto.keeps(1) && auto.keeps(2) && auto.keeps(u16::MAX));

    let t = |threshold, from, to| {
        DetectSettings {
            threshold,
            ..DetectSettings::DEFAULT
        }
        .for_format(from, to)
        .threshold
    };
    assert_eq!(t(128, Format::Raw8, Format::Raw10), 512);
    assert_eq!(t(255, Format::Raw8, Format::Raw10), 1020);
    assert_eq!(t(1023, Format::Raw10, Format::Raw8), 255);
    assert_eq!(t(3, Format::Raw10, Format::Raw8), 1);
    assert_eq!(t(77, Format::Raw8, Format::Raw8), 77);
    assert_eq!(t(77, Format::Raw10, Format::Raw10), 77);
    // Every scaled threshold passes the new format's check.
    for th in 1..=255 {
        let s = DetectSettings {
            threshold: th,
            ..DetectSettings::DEFAULT
        };
        assert_eq!(
            s.for_format(Format::Raw8, Format::Raw10)
                .check(Format::Raw10),
            Ok(())
        );
    }
    for th in 1..=1023 {
        let s = DetectSettings {
            threshold: th,
            ..DetectSettings::DEFAULT
        };
        assert_eq!(
            s.for_format(Format::Raw10, Format::Raw8)
                .check(Format::Raw8),
            Ok(())
        );
    }
}

#[test]
fn automatic_exposure_cap() {
    // 1280x800 at 120 Hz: about 9.1 us a line.
    assert_eq!(auto_exposure_cap(9_100, 889), 54);
    assert_eq!(auto_exposure_cap(9_100, 20), 20);
    assert_eq!(auto_exposure_cap(1_000_000, 889), 1);
    assert_eq!(auto_exposure_cap(0, 889), 889);
    assert_eq!(auto_exposure_cap(9_100, 0), 1);
}

#[test]
fn automatic_exposure_steps() {
    let e = |lines, gain_sixteenths| Exposure {
        lines,
        gain_sixteenths,
    };
    // Within the band, or no blob: hold (after bringing the values within the limits).
    for p in [
        None,
        Some(AUTO_EXPOSURE_LOW),
        Some(220),
        Some(AUTO_EXPOSURE_HIGH),
    ] {
        assert_eq!(auto_exposure(e(30, 32), p, 54), e(30, 32), "{p:?}");
    }
    assert_eq!(auto_exposure(e(100, 8), None, 54), e(54, GAIN_MIN));
    assert_eq!(auto_exposure(e(0, 255), None, 54), e(1, GAIN_MAX));
    // Too dark: aim at 220 / peak, at most twice; whole lines at 1x, the gain rounds up the rest.
    assert_eq!(auto_exposure(e(20, 16), Some(110), 54), e(40, 16));
    assert_eq!(auto_exposure(e(20, 16), Some(10), 54), e(40, 16));
    assert_eq!(auto_exposure(e(20, 16), Some(199), 54), e(22, 17)); // aim 353
    // At the cap, the rest goes to the gain.
    assert_eq!(auto_exposure(e(40, 16), Some(110), 54), e(54, 24));
    assert_eq!(auto_exposure(e(54, 16), Some(110), 54), e(54, 32));
    assert_eq!(
        auto_exposure(e(54, GAIN_MAX), Some(10), 54),
        e(54, GAIN_MAX)
    );
    // Too bright: the gain comes down to about 1x before the exposure shortens.
    assert_eq!(auto_exposure(e(54, 64), Some(u8::MAX), 54), e(54, 32));
    assert_eq!(auto_exposure(e(54, 20), Some(u8::MAX), 54), e(33, 17)); // aim 540
    assert_eq!(auto_exposure(e(40, 16), Some(u8::MAX), 54), e(20, 16));
    assert_eq!(auto_exposure(e(40, 16), Some(250), 54), e(35, 17)); // aim 563
    assert_eq!(auto_exposure(e(54, 17), Some(241), 54), e(52, 17)); // aim 838
    assert_eq!(auto_exposure(e(1, 16), Some(u8::MAX), 54), e(1, 16));
    // A few lines, where one line is coarser than the band: the gain makes the fine step
    // (whole lines alone would swing 3 <-> 4 lines, peaks 189 <-> 253, for ever).
    assert_eq!(auto_exposure(e(3, 16), Some(189), 54), e(3, 19)); // aim 55
    assert_eq!(auto_exposure(e(4, 16), Some(253), 54), e(3, 19)); // aim 55
    assert_eq!(auto_exposure(e(2, 16), Some(241), 54), e(1, 29)); // aim 29
    assert_eq!(auto_exposure(e(1, 16), Some(199), 54), e(1, 17)); // aim 17
}

/// A linear sensor: the peak is `k` x lines x gain / 16, saturating at 255.
fn peak_of(e: Exposure, k: f64) -> u8 {
    (k * f64::from(e.lines) * f64::from(e.gain_sixteenths) / 16.0).min(255.0) as u8
}

#[test]
fn automatic_exposure_settles_in_the_band() {
    let cap = 54;
    // From the darkest and the brightest setting, for sources from very dim to very bright:
    // k from 0.3 to about 400 in steps of 3 % (49 and 63 swung for ever with whole lines).
    let ks = (0..245).map(|i| 0.3 * 1.03f64.powi(i)).chain([49.0, 63.0]);
    for k in ks {
        for start in [
            Exposure {
                lines: 1,
                gain_sixteenths: GAIN_MIN,
            },
            Exposure {
                lines: cap,
                gain_sixteenths: GAIN_MAX,
            },
        ] {
            let mut e = start;
            let mut steps = 0;
            loop {
                let p = peak_of(e, k);
                let next = auto_exposure(e, Some(p), cap);
                if next == e {
                    break;
                }
                e = next;
                steps += 1;
                assert!(steps < 30, "k {k}: no rest after {steps} steps at {e:?}");
            }
            let p = peak_of(e, k);
            let in_band = (AUTO_EXPOSURE_LOW..=AUTO_EXPOSURE_HIGH).contains(&p);
            let at_limit =
                (p < AUTO_EXPOSURE_LOW && e.lines == cap && e.gain_sixteenths == GAIN_MAX)
                    || (p > AUTO_EXPOSURE_HIGH && e.lines == 1 && e.gain_sixteenths == GAIN_MIN);
            assert!(in_band || at_limit, "k {k}: rests at {e:?}, peak {p}");
            if e.lines < cap {
                // The gain is kept low: below the cap at most one line's worth above 1x,
                // rounded up to a sixteenth.
                let (l, g) = (u32::from(e.lines), u32::from(e.gain_sixteenths));
                assert!(g <= 16 + 16u32.div_ceil(l), "k {k}: {e:?}");
            }
        }
    }
}
