//! Camera stream messages: round trips at their boundaries and rejection of foreign datagrams.

use espion_proto::{
    Error, HEADER_LEN, MAGIC, VERSION,
    cam::{
        CHUNK_MAX, CamError, CameraSettings, Counters, Encoding, Format, FrameChunk, MAX_DATAGRAM,
        Message, ModeInfo, Op, Origin, Readout, Reply, Request, Response, State, StreamSettings,
        TestPattern, chunk_count,
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
            encoding: if max { Encoding::Raw10 } else { Encoding::Raw8 },
            max_fps: m(u16::MAX as u32) as u16,
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
        uptime_us: if max { u64::MAX } else { 0 },
    }
}

fn every_error() -> [CamError; 11] {
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
            }),
            Op::SetStream(StreamSettings {
                enabled: true,
                encoding: Encoding::Raw10,
                max_fps: u16::MAX,
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
    let frame_len = Encoding::Raw8.frame_bytes(u16::MAX, u16::MAX) as u32;
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

#[test]
fn a_frame_cut_into_chunks_round_trips() {
    // RAW10 1280x800 and RAW8 640x400: whole chunks and a short last one.
    for (encoding, width, height) in [(Encoding::Raw10, 1280, 800), (Encoding::Raw8, 640, 400)] {
        let frame_len = encoding.frame_bytes(width, height) as u32;
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
    let frame_len = Encoding::Raw8.frame_bytes(w, h) as u32;
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
