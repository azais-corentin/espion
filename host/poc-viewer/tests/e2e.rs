//! The fake camera and the viewer's network layer over loopback, without a window.

use std::{
    net::{Ipv6Addr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

use espion_proto::cam::{
    CamError, CameraSettings, Encoding, Format, MAX_DATAGRAM, Message, Op, Readout, Request,
    STREAM_TIMEOUT_MS, State, StreamSettings, TestPattern,
};
use poc_viewer::{
    camera,
    fake::{self, FakeCamera, FakeConfig},
    net::{Client, Command, Frame, Stats},
};

const WAIT: Duration = Duration::from_secs(10);

fn start(cfg: FakeConfig) -> (FakeCamera, Client) {
    let fake = FakeCamera::spawn(cfg).expect("fake camera starts");
    let client = Client::connect(fake.addr(), || {}).expect("client connects");
    (fake, client)
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

fn assert_bit_exact(f: &Frame, state: &State) {
    assert_eq!(
        (f.width, f.height),
        (state.camera.width, state.camera.height)
    );
    assert_eq!(f.encoding, state.stream.encoding);
    let mut expected = Vec::new();
    fake::render(
        &state.camera,
        state.stream.encoding,
        f.number,
        &mut expected,
    );
    assert_eq!(f.data.len(), expected.len());
    assert!(f.data == expected, "frame {} differs", f.number);
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

    let refused = |client: &Client, cmd: Command| -> (CamError, Stats) {
        client.send(cmd);
        let s = client
            .wait_until(WAIT, |s| s.refusal.is_some_and(|r| r.op == op_of(cmd)))
            .expect("refused");
        (s.refusal.unwrap().error, s)
    };

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

fn op_of(c: Command) -> Op {
    match c {
        Command::GetState => Op::GetState,
        Command::SetCamera(c) => Op::SetCamera(c),
        Command::SetStream(s) => Op::SetStream(s),
    }
}

#[test]
fn raw8_frames_arrive_bit_exact() {
    let (_fake, client) = start(FakeConfig::default());
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
    let (_fake, client) = start(FakeConfig::default());
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
        ..FakeConfig::default()
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
