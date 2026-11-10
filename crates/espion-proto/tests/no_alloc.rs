//! Encoding and decoding every camera stream message allocates nothing: the firmware has no
//! allocator. A counting global allocator watches this test's thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use espion_proto::cam::{
    CHUNK_MAX, CamError, CameraSettings, Counters, Encoding, Format, FrameChunk, MAX_DATAGRAM,
    Message, ModeInfo, Op, Origin, Readout, Reply, Request, Response, State, StreamSettings,
    TestPattern,
};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator; the counter is a const thread-local `Cell`,
// which allocates nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|a| a.set(a.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.with(|a| a.set(a.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn encode_and_decode_allocate_nothing() {
    let camera = CameraSettings {
        format: Format::Raw10,
        width: 1280,
        height: 800,
        readout: Readout::Crop,
        origin: Some(Origin { x: 0, y: 0 }),
        rate_mhz: 120_000,
        tolerance_mhz: 500,
        exposure_lines: 100,
        gain_sixteenths: 32,
        test_pattern: TestPattern::Bars,
    };
    let stream = StreamSettings {
        enabled: true,
        encoding: Encoding::Raw10,
        max_fps: 30,
    };
    let state = State {
        camera,
        stream,
        mode: ModeInfo {
            sensor_clock_mhz: 80,
            exceeds_datasheet_clock: false,
            hts: 728,
            vts: 914,
            psv: true,
            frame_rate_mhz: 120_049,
            rate_error_mhz: 49,
            max_exposure_lines: 889,
            line_ns: 9100,
            frame_bytes: 1_280_000,
        },
        generation: 3,
        counters: Counters::default(),
        uptime_us: 12_345_678,
    };
    let data = [0x5Au8; CHUNK_MAX];
    let frame_len = Encoding::Raw10.frame_bytes(1280, 800) as u32;
    let messages = [
        Message::Request(Request {
            seq: 1,
            op: Op::GetState,
        }),
        Message::Request(Request {
            seq: 2,
            op: Op::SetCamera(camera),
        }),
        Message::Request(Request {
            seq: 3,
            op: Op::SetStream(stream),
        }),
        Message::Request(Request {
            seq: 4,
            op: Op::Poll,
        }),
        Message::Response(Response {
            seq: 4,
            reply: Reply::State(state),
        }),
        Message::Response(Response {
            seq: 2,
            reply: Reply::Error(CamError::Exposure { max_lines: 889 }),
        }),
        Message::Chunk(FrameChunk {
            frame: 77,
            generation: 3,
            encoding: Encoding::Raw10,
            width: 1280,
            height: 800,
            timestamp_us: 12_000_000,
            frame_len,
            offset: CHUNK_MAX as u32 * 5,
            data: &data,
        }),
    ];

    let mut buf = [0u8; MAX_DATAGRAM];
    let before = allocations();
    for msg in &messages {
        let len = msg.encode(&mut buf).unwrap().len();
        let back = Message::decode(&buf[..len]).unwrap();
        assert!(back == *msg);
    }
    assert_eq!(allocations() - before, 0);
}
