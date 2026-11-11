//! The fake camera and the viewer's network layer over loopback, without a window.

use std::{
    net::{Ipv6Addr, UdpSocket},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use espion_proto::cam::{
    AUTO_QUALITY_MIN, CHUNK_MAX, CamError, CameraSettings, Encoding, Format, FrameChunk,
    JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, JpegRefusal, MAX_DATAGRAM, Message, Op, Readout, Request,
    STREAM_TIMEOUT_MS, State, StreamSettings, TestPattern,
};
use poc_viewer::{
    camera,
    decode::Decoder,
    fake::{self, FakeCamera, FakeConfig},
    jpeg,
    net::{Client, Command, Frame, Stats},
};

const WAIT: Duration = Duration::from_secs(10);

fn start(cfg: FakeConfig) -> (FakeCamera, Client) {
    let fake = FakeCamera::spawn(cfg).expect("fake camera starts");
    let client = Client::connect(fake.addr(), || {}).expect("client connects");
    (fake, client)
}

/// The fake's defaults with the stream in `encoding` (JPEG: automatic quality off, so every
/// frame is at the set quality).
fn stream_in(encoding: Encoding) -> StreamSettings {
    StreamSettings {
        encoding,
        auto_quality: false,
        ..fake::default_stream()
    }
}

/// The fake's defaults, streaming RAW8.
fn raw8() -> FakeConfig {
    FakeConfig {
        stream: stream_in(Encoding::Raw8),
        ..FakeConfig::default()
    }
}

fn binned_640(format: Format, rate_hz: u32) -> CameraSettings {
    CameraSettings {
        format,
        width: 640,
        height: 400,
        readout: Readout::Binned,
        origin: None,
        rate_mhz: rate_hz * 1000,
        tolerance_mhz: 500,
        exposure_lines: 100,
        gain_sixteenths: 48,
        test_pattern: TestPattern::Off,
    }
}

/// Sends `cmd` and waits for a state that satisfies `done`.
fn apply(client: &Client, cmd: Command, done: impl Fn(&State) -> bool) -> State {
    client.send(cmd);
    client
        .wait_until(WAIT, |s| s.state.as_ref().is_some_and(&done))
        .and_then(|s| s.state)
        .expect("the state shows the change")
}

/// Waits for the next frame of `generation`.
fn next_frame(client: &Client, generation: u32) -> Frame {
    let end = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < end, "no frame of generation {generation}");
        if let Some(f) = client.take_frame() {
            if f.generation == generation {
                return f;
            }
            client.recycle(f.data);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// The frame is the fake's frame `f.number` for `state` bit for bit: raw, the rendering; JPEG,
/// the file the fake's encoder makes of it at the set quality (automatic quality off).
/// The next frame of `state`'s generation captured after the state was sent.
fn frame_captured_after(client: &Client, state: &State) -> Frame {
    loop {
        let f = next_frame(client, state.generation);
        if f.timestamp_us > state.uptime_us {
            return f;
        }
        client.recycle(f.data);
    }
}

fn assert_bit_exact(f: &Frame, state: &State) {
    assert_eq!(
        (f.width, f.height),
        (state.camera.width, state.camera.height)
    );
    assert_eq!(f.encoding, state.stream.encoding);
    let mut expected = Vec::new();
    if f.encoding == Encoding::Jpeg {
        let mut raw = Vec::new();
        fake::render(&state.camera, Encoding::Raw8, f.number, &mut raw);
        fake::encode_jpeg(
            &state.camera,
            &raw,
            state.stream.jpeg_quality,
            &mut expected,
        )
        .unwrap();
    } else {
        fake::render(
            &state.camera,
            state.stream.encoding,
            f.number,
            &mut expected,
        );
    }
    assert_eq!(f.data.len(), expected.len(), "frame {}", f.number);
    assert!(f.data == expected, "frame {} differs", f.number);
}

/// PSNR of `a` against `b` (8-bit), dB.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-12)).log10()
}

#[test]
fn settings_are_applied_and_read_back() {
    let (_fake, client) = start(FakeConfig::default());
    let want = CameraSettings {
        test_pattern: TestPattern::Bars,
        ..binned_640(Format::Raw8, 250)
    };
    let mode = camera::validate(&want, Encoding::Raw8).unwrap();
    let state = apply(&client, Command::SetCamera(want), |s| s.camera.width == 640);
    assert_eq!(state.camera, camera::applied(&want, &mode));
    assert_eq!(state.mode, camera::mode_info(&mode));
    assert_eq!(state.mode.frame_rate_mhz, 250_105);

    let stream = StreamSettings {
        enabled: true,
        encoding: Encoding::Raw8,
        max_fps: 30,
        ..fake::default_stream()
    };
    let state = apply(&client, Command::SetStream(stream), |s| {
        s.stream.max_fps == 30
    });
    assert_eq!(state.stream, stream);

    // Read back with a plain GetState: the same settings, the same generation.
    let generation = state.generation;
    let before = client.stats().requests_sent;
    client.send(Command::GetState);
    let s = client
        .wait_until(WAIT, |s| s.requests_sent > before && !s.pending)
        .unwrap();
    let back = s.state.unwrap();
    assert_eq!(back.camera, state.camera);
    assert_eq!(back.stream, stream);
    assert_eq!(back.generation, generation);

    // Exposure only: the same mode, so the generation does not change.
    let brighter = CameraSettings {
        exposure_lines: 200,
        ..back.camera
    };
    let s = apply(&client, Command::SetCamera(brighter), |s| {
        s.camera.exposure_lines == 200
    });
    assert_eq!(s.generation, generation);
    assert_eq!(s.mode, back.mode);

    // The stream sends at most 30 frames per second.
    let base = client.stats();
    thread::sleep(Duration::from_secs(2));
    let now = client.stats();
    let fps = (now.frames_whole - base.frames_whole) as f64 / 2.0;
    assert!((20.0..=31.0).contains(&fps), "{fps} fps");
}

#[test]
fn an_invalid_setting_returns_its_error_and_changes_nothing() {
    let (_fake, client) = start(FakeConfig::default());
    let before = apply(&client, Command::GetState, |_| true);

    let bad_width = CameraSettings {
        width: 1276,
        ..before.camera
    };
    let (e, _) = refused(&client, Command::SetCamera(bad_width));
    assert_eq!(
        e,
        CamError::Alignment {
            width_multiple: 8,
            height_multiple: 1
        }
    );

    let too_fast = CameraSettings {
        rate_mhz: 200_000,
        tolerance_mhz: 0,
        ..before.camera
    };
    let (e, _) = refused(&client, Command::SetCamera(too_fast));
    assert_eq!(e, CamError::TooFast { max_mhz: 153_984 });

    let long = CameraSettings {
        exposure_lines: 5000,
        ..before.camera
    };
    let (e, _) = refused(&client, Command::SetCamera(long));
    assert_eq!(
        e,
        CamError::Exposure {
            max_lines: before.mode.max_exposure_lines
        }
    );

    let raw10 = StreamSettings {
        encoding: Encoding::Raw10,
        ..before.stream
    };
    let (e, s) = refused(&client, Command::SetStream(raw10));
    assert_eq!(
        e,
        CamError::Encoding {
            format: Format::Raw8,
            encoding: Encoding::Raw10
        }
    );
    // Nothing changed.
    let after = s.state.unwrap();
    assert_eq!(after.camera, before.camera);
    assert_eq!(after.stream, before.stream);
    assert_eq!(after.generation, before.generation);
}

/// Sends `cmd` and waits for its refusal.
fn refused(client: &Client, cmd: Command) -> (CamError, Stats) {
    let sent = Instant::now();
    client.send(cmd);
    let s = client
        .wait_until(WAIT, |s| {
            s.refusal
                .is_some_and(|r| r.op == op_of(cmd) && r.at >= sent)
        })
        .expect("refused");
    (s.refusal.unwrap().error, s)
}

fn op_of(c: Command) -> Op {
    match c {
        Command::GetState => Op::GetState,
        Command::SetCamera(c) => Op::SetCamera(c),
        Command::SetStream(s) => Op::SetStream(s),
    }
}

#[test]
fn raw8_frames_arrive_bit_exact() {
    let (_fake, client) = start(raw8());
    let state = apply(&client, Command::GetState, |_| true);
    assert_eq!((state.camera.width, state.camera.height), (1280, 800));
    for _ in 0..5 {
        let f = next_frame(&client, state.generation);
        assert_bit_exact(&f, &state);
        client.recycle(f.data);
    }
}

#[test]
fn raw10_frames_arrive_bit_exact() {
    let (_fake, client) = start(raw8());
    let raw10 = CameraSettings {
        format: Format::Raw10,
        ..fake::default_camera()
    };
    apply(&client, Command::SetCamera(raw10), |s| {
        s.camera.format == Format::Raw10
    });
    let stream = StreamSettings {
        encoding: Encoding::Raw10,
        ..fake::default_stream()
    };
    let state = apply(&client, Command::SetStream(stream), |s| {
        s.stream.encoding == Encoding::Raw10
    });
    for _ in 0..5 {
        let f = next_frame(&client, state.generation);
        assert_eq!(f.data.len(), 1280 * 800 * 5 / 4);
        assert_bit_exact(&f, &state);
        client.recycle(f.data);
    }

    // The same sensor format sent as RAW8: the 8 high bits, a new generation.
    let stream = StreamSettings {
        encoding: Encoding::Raw8,
        ..stream
    };
    let state8 = apply(&client, Command::SetStream(stream), |s| {
        s.stream.encoding == Encoding::Raw8
    });
    assert_ne!(state8.generation, state.generation);
    let f = next_frame(&client, state8.generation);
    assert_eq!(f.data.len(), 1280 * 800);
    assert_bit_exact(&f, &state8);
}

#[test]
fn a_slow_consumer_gets_the_newest_frame() {
    let (_fake, client) = start(FakeConfig::default());
    let state = apply(
        &client,
        Command::SetCamera(binned_640(Format::Raw8, 250)),
        |s| s.camera.width == 640,
    );
    next_frame(&client, state.generation);

    let base = client.stats();
    let mut taken = Vec::new();
    for _ in 0..15 {
        thread::sleep(Duration::from_millis(100));
        if let Some(f) = client.take_frame() {
            taken.push(f.number);
            client.recycle(f.data);
        }
    }
    let s = client.stats();
    let whole = s.frames_whole - base.frames_whole;
    let replaced = s.frames_replaced - base.frames_replaced;
    // Every whole frame was taken or replaced, but for one that may still wait.
    let accounted = taken.len() as u64 + replaced;
    assert!(
        accounted == whole || accounted + 1 == whole,
        "{whole} whole, {} taken, {replaced} replaced",
        taken.len()
    );
    assert!(taken.len() >= 10, "{taken:?}");
    // About 25 frames arrive per 100 ms: the consumer skips most, always forwards.
    assert!(taken.windows(2).all(|w| w[1] > w[0] + 5), "{taken:?}");
}

#[test]
fn frames_missing_a_chunk_are_dropped_and_the_stream_goes_on() {
    let (_fake, client) = start(FakeConfig {
        loss: 0.002,
        camera: binned_640(Format::Raw8, 250),
        ..raw8()
    });
    let state = apply(&client, Command::GetState, |_| true);
    let base = client.stats();
    let end = Instant::now() + Duration::from_secs(2);
    let mut checked = 0;
    while Instant::now() < end {
        let f = next_frame(&client, state.generation);
        assert_bit_exact(&f, &state);
        client.recycle(f.data);
        checked += 1;
    }
    let s = client.stats();
    let whole = s.frames_whole - base.frames_whole;
    let incomplete = s.frames_incomplete - base.frames_incomplete;
    // 182 chunks per frame at 0.2 % loss: about 70 % of the frames arrive whole.
    assert!(incomplete >= 20, "{incomplete} incomplete, {whole} whole");
    assert!(whole >= 100, "{incomplete} incomplete, {whole} whole");
    assert!(checked >= 20);
    assert!(s.connected(Instant::now()));
}

#[test]
fn the_link_limit_skips_frames_on_the_board() {
    let (_fake, client) = start(FakeConfig {
        mbps: Some(95.0),
        ..raw8()
    });
    let state = apply(&client, Command::GetState, |_| true);
    next_frame(&client, state.generation);
    let base = client.stats();
    thread::sleep(Duration::from_secs(2));
    let s = client.stats();
    let mbps = (s.bytes - base.bytes) as f64 * 8.0 / 2.0 / 1e6;
    let fps = (s.frames_whole - base.frames_whole) as f64 / 2.0;
    let skipped =
        s.state.unwrap().counters.frames_skipped - base.state.unwrap().counters.frames_skipped;
    // UDP payload of a 95 Mbit/s line: 95 x 1452 / (1452 + 86) = 89.7 Mbit/s at most.
    assert!((80.0..=90.0).contains(&mbps), "{mbps} Mbit/s");
    assert!((9.0..=11.5).contains(&fps), "{fps} fps");
    assert!(skipped > 150, "{skipped} skipped");
}

#[test]
fn the_board_talks_only_to_a_polling_host() {
    let fake = FakeCamera::spawn(FakeConfig {
        camera: binned_640(Format::Raw8, 250),
        ..FakeConfig::default()
    })
    .unwrap();
    let sock = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    sock.connect(fake.addr()).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut buf = [0u8; MAX_DATAGRAM];

    // Nothing comes before the host asks.
    assert!(sock.recv(&mut buf).is_err());

    let mut req = [0u8; MAX_DATAGRAM];
    let d = Request {
        seq: 77,
        op: Op::GetState,
    }
    .encode(&mut req)
    .unwrap();
    sock.send(d).unwrap();
    let mut answered = false;
    let mut chunks = 0;
    let start = Instant::now();
    let mut last_chunk = start;
    while let Ok(n) = sock.recv(&mut buf) {
        match Message::decode(&buf[..n]) {
            Ok(Message::Response(r)) => answered |= r.seq == 77,
            Ok(Message::Chunk(_)) => {
                chunks += 1;
                last_chunk = Instant::now();
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(answered);
    assert!(chunks > 1000);
    // The stream stopped about STREAM_TIMEOUT_MS after the one request.
    let stopped = (last_chunk - start).as_millis() as u32;
    assert!(
        (STREAM_TIMEOUT_MS - 100..=STREAM_TIMEOUT_MS + 100).contains(&stopped),
        "stopped after {stopped} ms"
    );
}

#[test]
fn a_frame_cut_by_a_mode_change_is_not_counted_as_lost() {
    let (_fake, client) = start(FakeConfig {
        mbps: Some(95.0),
        ..raw8()
    });
    let state = apply(&client, Command::GetState, |_| true);
    next_frame(&client, state.generation);
    // A 1280x800 frame takes about 90 ms on the line: the change lands in the middle of one.
    thread::sleep(Duration::from_millis(40));
    let base = client.stats();
    let state = apply(
        &client,
        Command::SetCamera(binned_640(Format::Raw8, 250)),
        |s| s.camera.width == 640,
    );
    next_frame(&client, state.generation);
    let s = client.stats();
    assert_eq!(s.frames_incomplete, base.frames_incomplete);
    assert!(s.frames_cut - base.frames_cut <= 1, "{} cut", s.frames_cut);
}

#[test]
fn the_fake_starts_in_jpeg_at_quality_90_with_automatic_quality() {
    let (_fake, client) = start(FakeConfig::default());
    let state = apply(&client, Command::GetState, |_| true);
    assert_eq!(
        (state.camera.format, state.camera.width, state.camera.height),
        (Format::Raw8, 1280, 800)
    );
    assert_eq!(state.camera.rate_mhz, 120_000);
    let s = state.stream;
    assert_eq!(
        (
            s.enabled,
            s.encoding,
            s.jpeg_quality,
            s.auto_quality,
            s.max_fps
        ),
        (true, Encoding::Jpeg, 90, true, 0)
    );
    assert_eq!(state.encoder.quality, 90);
    let f = next_frame(&client, state.generation);
    let mut pixels = Vec::new();
    jpeg::decode(&f.data, f.width, f.height, &mut pixels).expect("a greyscale baseline JPEG");
    // The encoder's figures in the next polls.
    client
        .wait_until(WAIT, |s| {
            s.state
                .is_some_and(|st| st.encoder.frame_len > 0 && st.encoder.encode_us > 0)
        })
        .expect("the encoder reports its last frame");
}

#[test]
fn jpeg_frames_are_the_fakes_picture_within_the_codec_error() {
    let (_fake, client) = start(FakeConfig {
        stream: stream_in(Encoding::Jpeg),
        ..FakeConfig::default()
    });
    for (camera, min_psnr) in [
        (fake::default_camera(), 38.0),
        (
            CameraSettings {
                test_pattern: TestPattern::Bars,
                ..binned_640(Format::Raw8, 250)
            },
            40.0,
        ),
    ] {
        let state = apply(&client, Command::SetCamera(camera), |s| {
            s.camera.width == camera.width && s.camera.test_pattern == camera.test_pattern
        });
        let mut lengths = Vec::new();
        for _ in 0..5 {
            let f = next_frame(&client, state.generation);
            assert_bit_exact(&f, &state);
            let mut decoded = Vec::new();
            jpeg::decode(&f.data, f.width, f.height, &mut decoded).unwrap();
            let mut raw = Vec::new();
            fake::render(&state.camera, Encoding::Raw8, f.number, &mut raw);
            let p = psnr(&decoded, &raw);
            assert!(p >= min_psnr, "frame {}: PSNR {p:.2} dB", f.number);
            lengths.push(f.data.len());
            client.recycle(f.data);
        }
        // Variable length: the moving bar and digits change the file.
        lengths.dedup();
        assert!(lengths.len() > 1, "{lengths:?}");
    }
}

#[test]
fn quality_is_applied_and_reported_and_refused_out_of_range() {
    let (_fake, client) = start(FakeConfig {
        stream: stream_in(Encoding::Jpeg),
        ..FakeConfig::default()
    });
    let mut sizes = Vec::new();
    for q in [30u8, 60, 95] {
        let want = StreamSettings {
            jpeg_quality: q,
            ..stream_in(Encoding::Jpeg)
        };
        let state = apply(&client, Command::SetStream(want), |s| {
            s.stream.jpeg_quality == q
        });
        assert_eq!(state.encoder.quality, q);
        // A frame captured after the change is encoded at the new quality.
        let f = frame_captured_after(&client, &state);
        assert_bit_exact(&f, &state);
        sizes.push(f.data.len());
    }
    assert!(sizes.windows(2).all(|w| w[1] > w[0]), "{sizes:?}");

    let before = client.stats().state.unwrap();
    for q in [0u8, 101] {
        let bad = StreamSettings {
            jpeg_quality: q,
            ..before.stream
        };
        let (e, s) = refused(&client, Command::SetStream(bad));
        assert_eq!(
            e,
            CamError::Quality {
                min: JPEG_QUALITY_MIN,
                max: JPEG_QUALITY_MAX
            }
        );
        let after = s.state.unwrap();
        assert_eq!(after.stream, before.stream);
        assert_eq!(after.encoder.quality, before.encoder.quality);
    }
}

#[test]
fn jpeg_is_refused_for_raw10_modes_and_sizes_not_a_multiple_of_8() {
    let (_fake, client) = start(FakeConfig::default());
    let before = apply(&client, Command::GetState, |_| true);
    let jpeg_err = |reason, c: &CameraSettings| CamError::Jpeg {
        reason,
        format: c.format,
        width: c.width,
        height: c.height,
    };
    // A mode JPEG cannot be made of, while the stream is JPEG.
    for (c, reason) in [
        (
            CameraSettings {
                format: Format::Raw10,
                ..fake::default_camera()
            },
            JpegRefusal::Format,
        ),
        (
            CameraSettings {
                height: 796,
                ..fake::default_camera()
            },
            JpegRefusal::Size,
        ),
        (
            CameraSettings {
                height: 396,
                ..binned_640(Format::Raw8, 250)
            },
            JpegRefusal::Size,
        ),
    ] {
        let (e, s) = refused(&client, Command::SetCamera(c));
        assert_eq!(e, jpeg_err(reason, &c));
        assert_eq!(camera::validate(&c, Encoding::Jpeg), Err(e));
        let after = s.state.unwrap();
        assert_eq!(
            (after.camera, after.stream, after.generation),
            (before.camera, before.stream, before.generation)
        );
    }
    // JPEG asked for in such a mode.
    apply(
        &client,
        Command::SetStream(stream_in(Encoding::Raw8)),
        |s| s.stream.encoding == Encoding::Raw8,
    );
    for c in [
        CameraSettings {
            format: Format::Raw10,
            ..fake::default_camera()
        },
        CameraSettings {
            height: 796,
            ..fake::default_camera()
        },
    ] {
        apply(&client, Command::SetCamera(c), |s| {
            (s.camera.format, s.camera.height) == (c.format, c.height)
        });
        let (e, s) = refused(&client, Command::SetStream(stream_in(Encoding::Jpeg)));
        let reason = if c.format == Format::Raw10 {
            JpegRefusal::Format
        } else {
            JpegRefusal::Size
        };
        assert_eq!(e, jpeg_err(reason, &c));
        assert_eq!(s.state.unwrap().stream.encoding, Encoding::Raw8);
    }
}

#[test]
fn switching_between_jpeg_and_the_raw_encodings() {
    let (_fake, client) = start(FakeConfig {
        camera: binned_640(Format::Raw8, 250),
        stream: stream_in(Encoding::Jpeg),
        ..FakeConfig::default()
    });
    let mut generation = apply(&client, Command::GetState, |_| true).generation;
    let raw10 = binned_640(Format::Raw10, 250);
    let raw8 = binned_640(Format::Raw8, 250);
    let steps: [(Option<CameraSettings>, Encoding); 6] = [
        (None, Encoding::Raw8),
        (None, Encoding::Jpeg),
        (None, Encoding::Raw8),
        (Some(raw10), Encoding::Raw10),
        (Some(raw8), Encoding::Raw8),
        (None, Encoding::Jpeg),
    ];
    for (mode, encoding) in steps {
        if let Some(m) = mode {
            if m.format == Format::Raw8 {
                // RAW10 cannot be sent from a RAW8 sensor: the stream first.
                apply(
                    &client,
                    Command::SetStream(stream_in(Encoding::Raw8)),
                    |s| s.stream.encoding == Encoding::Raw8,
                );
            }
            apply(&client, Command::SetCamera(m), |s| {
                s.camera.format == m.format
            });
        }
        let state = apply(&client, Command::SetStream(stream_in(encoding)), |s| {
            s.stream.encoding == encoding
        });
        assert_ne!(state.generation, generation, "{encoding:?}");
        generation = state.generation;
        for _ in 0..3 {
            let f = frame_captured_after(&client, &state);
            assert_bit_exact(&f, &state);
            client.recycle(f.data);
        }
    }
}

#[test]
fn jpeg_frames_missing_a_chunk_are_dropped_and_the_stream_goes_on() {
    let (_fake, client) = start(FakeConfig {
        loss: 0.01,
        stream: stream_in(Encoding::Jpeg),
        ..FakeConfig::default()
    });
    let state = apply(&client, Command::GetState, |_| true);
    let base = client.stats();
    let end = Instant::now() + Duration::from_secs(2);
    let mut checked = 0;
    while Instant::now() < end {
        let f = next_frame(&client, state.generation);
        assert_bit_exact(&f, &state);
        client.recycle(f.data);
        checked += 1;
    }
    let s = client.stats();
    let whole = s.frames_whole - base.frames_whole;
    let incomplete = s.frames_incomplete - base.frames_incomplete;
    // About 43 chunks per 1280x800 JPEG at 1 % loss: about 65 % arrive whole.
    assert!(incomplete >= 20, "{incomplete} incomplete, {whole} whole");
    assert!(whole >= 100, "{incomplete} incomplete, {whole} whole");
    assert!(checked >= 20);
    assert_eq!(s.mismatched_chunks, 0);
}

/// The fake's frame rate and quality in use over `secs`: (fps at the host, frames skipped for
/// the link, captured, quality at the end).
fn window(client: &Client, secs: f64) -> (f64, u32, u32, u8) {
    let a = client.stats();
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if let Some(f) = client.take_frame() {
            client.recycle(f.data);
        }
        thread::sleep(Duration::from_millis(2));
    }
    let z = client.stats();
    let (sa, sz) = (a.state.unwrap(), z.state.unwrap());
    (
        (z.frames_whole - a.frames_whole) as f64 / secs,
        sz.counters.frames_skipped - sa.counters.frames_skipped,
        sz.counters.frames_captured - sa.counters.frames_captured,
        sz.encoder.quality,
    )
}

#[test]
fn automatic_quality_follows_the_link() {
    // Binned 640x400 at the sensor's maximum through a 20 Mbit/s line: at quality 90 the
    // fake's frames do not fit (a small window keeps the unoptimised test build's rendering
    // ahead of the sensor).
    let fastest = CameraSettings {
        rate_mhz: 302_189,
        tolerance_mhz: 0,
        ..binned_640(Format::Raw8, 300)
    };
    let sensor = 302.19;
    let (_fake, client) = start(FakeConfig {
        mbps: Some(20.0),
        camera: fastest,
        stream: StreamSettings {
            jpeg_quality: 90,
            ..stream_in(Encoding::Jpeg)
        },
        ..FakeConfig::default()
    });
    let state = apply(&client, Command::GetState, |_| true);
    next_frame(&client, state.generation);
    let (fps_off, skipped_off, captured, q) = window(&client, 1.5);
    assert_eq!(q, 90);
    assert!(
        skipped_off > captured / 10 && fps_off < 0.9 * sensor,
        "auto off: {fps_off:.1} fps, {skipped_off} of {captured} skipped"
    );

    let on = StreamSettings {
        jpeg_quality: 90,
        auto_quality: true,
        ..stream_in(Encoding::Jpeg)
    };
    apply(&client, Command::SetStream(on), |s| s.stream.auto_quality);
    let mut trace = Vec::new();
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(200));
        trace.push(client.stats().state.unwrap().encoder.quality);
    }
    let (fps_on, skipped_on, captured, q) = window(&client, 1.5);
    assert!(
        (AUTO_QUALITY_MIN..90).contains(&q),
        "quality {q}, trace {trace:?}"
    );
    assert!(
        fps_on > 0.9 * sensor && skipped_on * 20 < captured,
        "auto on: {fps_on:.1} fps, {skipped_on} of {captured} skipped, trace {trace:?}"
    );

    // Room again (a test pattern of bars at 100 Hz compresses to a small share of the link):
    // back up to the set quality, one step per period.
    let bars = CameraSettings {
        test_pattern: TestPattern::Bars,
        rate_mhz: 100_000,
        ..fastest
    };
    apply(&client, Command::SetCamera(bars), |s| {
        s.camera.test_pattern == TestPattern::Bars
    });
    let back = client.wait_until(
        Duration::from_secs_f64(f64::from(90 - q) * 0.2 + 3.0),
        |s| s.state.is_some_and(|st| st.encoder.quality == 90),
    );
    assert!(back.is_some(), "quality did not come back from {q}");

    // Off: the set quality at once, and it stays under load.
    let off = StreamSettings {
        jpeg_quality: 70,
        auto_quality: false,
        ..on
    };
    apply(&client, Command::SetStream(off), |s| !s.stream.auto_quality);
    apply(&client, Command::SetCamera(fastest), |s| {
        s.camera.test_pattern == TestPattern::Off
    });
    let (_, _, _, q) = window(&client, 1.0);
    assert_eq!(q, 70);
}

#[test]
fn chunks_that_disagree_with_their_frame_drop_it() {
    // A camera that sends two chunks of one frame with different lengths.
    let cam = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let client = Client::connect(cam.local_addr().unwrap(), || {}).unwrap();
    let to = client.local_addr();
    let data = [7u8; CHUNK_MAX];
    let chunk = |frame: u32, frame_len: u32, index: u32| FrameChunk {
        frame,
        generation: 1,
        encoding: Encoding::Jpeg,
        width: 640,
        height: 400,
        timestamp_us: 0,
        frame_len,
        offset: index * CHUNK_MAX as u32,
        data: &data[..(frame_len as usize - index as usize * CHUNK_MAX).min(CHUNK_MAX)],
    };
    let mut buf = [0u8; MAX_DATAGRAM];
    for c in [
        chunk(5, 3000, 0),
        chunk(5, 4000, 1),
        chunk(5, 3000, 1),
        chunk(5, 3000, 2),
        // The next frame is whole.
        chunk(6, 2000, 0),
        chunk(6, 2000, 1),
    ] {
        cam.send_to(c.encode(&mut buf).unwrap(), to).unwrap();
    }
    let s = client
        .wait_until(WAIT, |s| s.frames_whole == 1)
        .expect("frame 6 arrives");
    assert_eq!(s.mismatched_chunks, 1);
    assert_eq!(s.frames_incomplete, 1);
    assert_eq!(s.stale_chunks, 2);
    let f = client.take_frame().unwrap();
    assert_eq!((f.number, f.data.len()), (6, 2000));
}

#[test]
fn the_decoder_decodes_only_the_frames_taken() {
    // 640x400 binned at 302 Hz in JPEG, a window that takes a picture every 100 ms.
    let fake = FakeCamera::spawn(FakeConfig {
        camera: CameraSettings {
            rate_mhz: 302_189,
            tolerance_mhz: 0,
            ..binned_640(Format::Raw8, 300)
        },
        stream: stream_in(Encoding::Jpeg),
        ..FakeConfig::default()
    })
    .unwrap();
    let client = Arc::new(Client::connect(fake.addr(), || {}).unwrap());
    let state = apply(&client, Command::GetState, |_| true);
    let decoder = Decoder::spawn(client.clone(), || {}).unwrap();
    let base = client.stats();
    let mut taken = 0u64;
    for _ in 0..15 {
        thread::sleep(Duration::from_millis(100));
        let Some(p) = decoder.take() else { continue };
        taken += 1;
        let f = &p.frame;
        assert_eq!(f.encoding, Encoding::Raw8);
        assert!(p.jpeg_len.is_some());
        let mut raw = Vec::new();
        fake::render(&state.camera, Encoding::Raw8, f.number, &mut raw);
        let q = psnr(&f.data, &raw);
        assert!(q >= 38.0, "frame {}: PSNR {q:.2} dB", f.number);
        decoder.recycle(p);
    }
    let s = client.stats();
    let d = decoder.stats();
    let whole = s.frames_whole - base.frames_whole;
    // One picture decoded ahead of the window at most, every other frame replaced undecoded.
    assert!(taken >= 13, "{taken} taken");
    assert!(
        d.decoded <= taken + 1,
        "{} decoded, {taken} taken",
        d.decoded
    );
    assert!(whole > 300, "{whole} whole");
    assert!(s.frames_replaced - base.frames_replaced > whole - 30);
    assert_eq!(d.failures, 0);
}

#[test]
fn a_frame_that_does_not_decode_is_counted_and_dropped() {
    let cam = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let client = Arc::new(Client::connect(cam.local_addr().unwrap(), || {}).unwrap());
    let decoder = Decoder::spawn(client.clone(), || {}).unwrap();
    let to = client.local_addr();
    let mut raw = Vec::new();
    let c = CameraSettings {
        width: 128,
        height: 64,
        ..fake::default_camera()
    };
    fake::render(&c, Encoding::Raw8, 3, &mut raw);
    let mut good = Vec::new();
    fake::encode_jpeg(&c, &raw, 90, &mut good).unwrap();
    // A whole, valid JPEG file of another size than its frame says.
    let half = CameraSettings { height: 32, ..c };
    let mut bad = Vec::new();
    fake::encode_jpeg(&half, &raw[..128 * 32], 90, &mut bad).unwrap();
    let mut buf = [0u8; MAX_DATAGRAM];
    for (frame, file) in [(1u32, &bad), (2, &good)] {
        for (i, data) in file.chunks(CHUNK_MAX).enumerate() {
            let chunk = FrameChunk {
                frame,
                generation: 1,
                encoding: Encoding::Jpeg,
                width: c.width,
                height: c.height,
                timestamp_us: 0,
                frame_len: file.len() as u32,
                offset: (i * CHUNK_MAX) as u32,
                data,
            };
            cam.send_to(chunk.encode(&mut buf).unwrap(), to).unwrap();
        }
        // One at a time: the decoder takes each before the next arrives.
        let want = frame;
        let end = Instant::now() + WAIT;
        while decoder.stats().decoded + decoder.stats().failures < u64::from(want) {
            assert!(Instant::now() < end, "frame {frame} not decoded");
            if let Some(p) = decoder.take() {
                decoder.recycle(p);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    let d = decoder.stats();
    assert_eq!((d.decoded, d.failures), (1, 1), "{d:?}");
    let why = d.last_failure.unwrap();
    assert!(why.starts_with("frame 1: picture 128x32"), "{why}");
}

#[test]
fn a_board_that_restarts_its_frame_numbers_is_followed() {
    // A camera that sends frame 200000, a late chunk of frame 199999, then frame 0 (it
    // rebooted): the late chunk is stale, frame 0 a new frame.
    let cam = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let client = Client::connect(cam.local_addr().unwrap(), || {}).unwrap();
    let to = client.local_addr();
    let data = [9u8; 100];
    let chunk = |frame: u32| FrameChunk {
        frame,
        generation: 1,
        encoding: Encoding::Jpeg,
        width: 640,
        height: 400,
        timestamp_us: 0,
        frame_len: 100,
        offset: 0,
        data: &data,
    };
    let mut buf = [0u8; MAX_DATAGRAM];
    for (sent, (frame, whole)) in [(200_000u32, 1u64), (199_999, 1), (0, 2), (1, 3)]
        .into_iter()
        .enumerate()
    {
        cam.send_to(chunk(frame).encode(&mut buf).unwrap(), to)
            .unwrap();
        let s = client
            .wait_until(WAIT, |s| s.chunks > sent as u64)
            .expect("the chunk arrives");
        assert_eq!(s.frames_whole, whole, "after frame {frame}");
    }
    let s = client.stats();
    assert_eq!(
        (s.frames_whole, s.stale_chunks, s.frames_incomplete),
        (3, 1, 0)
    );
    assert_eq!(client.take_frame().map(|f| f.number), Some(1));
}

/// The state names the last frame sent whole and where its time went: a raw frame has no
/// encode, a JPEG frame is encoded, then sent, in that order.
#[test]
fn the_state_times_the_last_frame_sent() {
    let (_fake, client) = start(raw8());
    let timed = |s: &Stats| s.state.and_then(|st| st.timing.map(|t| (st, t)));
    let s = client
        .wait_until(WAIT, |s| timed(s).is_some())
        .expect("a state with a frame's timing");
    let (state, t) = timed(&s).unwrap();
    assert_eq!(t.encode, None, "a raw frame is not encoded");
    assert!(t.send.start_us <= t.send.end_us, "{t:?}");
    assert!(t.frame < state.counters.frames_captured, "{t:?}");

    client.send(Command::SetStream(stream_in(Encoding::Jpeg)));
    let s = client
        .wait_until(WAIT, |s| timed(s).is_some_and(|(_, t)| t.encode.is_some()))
        .expect("a state with a JPEG frame's timing");
    let (state, t) = timed(&s).unwrap();
    let encode = t.encode.unwrap();
    assert!(
        encode.start_us < encode.end_us
            && encode.end_us <= t.send.start_us
            && t.send.start_us <= t.send.end_us,
        "{t:?}"
    );
    assert!(t.frame < state.counters.frames_captured, "{t:?}");
    // The frame was sent before the state was made, on the same clock.
    assert!(u64::from(t.send.end_us) < state.uptime_us, "{t:?}");

    // The network thread joins the board's times of a frame with its arrival here.
    let s = client
        .wait_until(WAIT, |s| {
            s.board_frame.is_some_and(|b| b.timing.encode.is_some())
        })
        .expect("a timed frame that arrived whole");
    let b = s.board_frame.unwrap();
    assert!(b.first_chunk <= b.completed, "{b:?}");
    assert!(
        b.timestamp_us + u64::from(b.timing.send.end_us) < s.state.unwrap().uptime_us,
        "{b:?}"
    );
}
