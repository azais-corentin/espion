//! The captured frames the image path holds (the send task for a raw frame, the encode task
//! for JPEG): a whole-frame receiver's [`Frame`], or a strip-path frame core 1 assembled in a
//! buffer of the [`Pool`]. Each frame has one holder; dropping it gives the buffer back (to the
//! receiver, or to core 1's free set).
//!
//! Whole frames still held are counted ([`whole_out`]): before the strip path starts, the
//! capture task waits until the whole-frame receiver has every frame back, so a later
//! whole-frame receiver never writes into a buffer that is still read.

use core::ops::Deref;
use core::sync::atomic::{AtomicU32, Ordering};

use esp32p4_csi::Frame;
use espion_core1::{FrameBuffers, give_frame};

use crate::camera;

/// Frame buffers of the strip path: one being assembled by core 1 and one waiting for its
/// refined measure, one being sent (raw) or two at the encode stage (JPEG), and a spare, so
/// core 1 nearly always finds a free one at a frame's start.
pub const POOL_BUFFERS: usize = 5;
const _: () = assert!(POOL_BUFFERS <= espion_core1::MAX_FRAME_BUFFERS);

/// Whole frames held now.
static WHOLE_OUT: AtomicU32 = AtomicU32::new(0);

/// Whole frames handed out and not yet dropped.
pub(super) fn whole_out() -> u32 {
    WHOLE_OUT.load(Ordering::Relaxed)
}

/// The strip path's frame buffers in PSRAM ([`POOL_BUFFERS`] x [`camera::BUFFER_LEN`]).
#[derive(Clone, Copy)]
pub(super) struct Pool {
    base: usize,
}

impl Pool {
    /// The pool on `mem` (at least [`POOL_BUFFERS`] x [`camera::BUFFER_LEN`] bytes, 128-byte
    /// aligned, given for good); every buffer goes to core 1's free set.
    pub fn new(mem: &'static mut [u8]) -> Self {
        let base = mem.as_mut_ptr() as usize;
        for i in 0..POOL_BUFFERS {
            give_frame(i as u8);
        }
        Self { base }
    }

    /// The buffers, as core 1's stream job takes them (`frame_bytes` set per run).
    pub fn buffers(&self) -> FrameBuffers {
        FrameBuffers {
            base: self.base,
            stride: camera::BUFFER_LEN,
            count: POOL_BUFFERS as u8,
            frame_bytes: 0,
        }
    }

    /// Frame buffer `index`, holding a frame of `len` bytes core 1 handed over.
    pub fn frame(&self, index: u8, len: usize) -> Option<FrameBuf> {
        if usize::from(index) >= POOL_BUFFERS || len > camera::BUFFER_LEN {
            give_frame(index);
            return None;
        }
        // SAFETY: core 1 handed the buffer over with its result (its copies complete, its lines
        // invalidated); nobody writes it until this holder gives it back.
        let data = unsafe {
            core::slice::from_raw_parts(
                (self.base + usize::from(index) * camera::BUFFER_LEN) as *const u8,
                len,
            )
        };
        Some(FrameBuf {
            inner: Inner::Pool { index, data },
        })
    }
}

/// One captured frame; derefs to its bytes. Dropping it gives the buffer back.
pub(super) struct FrameBuf {
    inner: Inner,
}

enum Inner {
    Whole(Frame),
    Pool { index: u8, data: &'static [u8] },
}

impl FrameBuf {
    /// A whole-frame receiver's frame (counted in [`whole_out`] until dropped).
    pub fn whole(f: Frame) -> Self {
        WHOLE_OUT.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Inner::Whole(f),
        }
    }
}

impl Deref for FrameBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.inner {
            Inner::Whole(f) => f,
            Inner::Pool { data, .. } => data,
        }
    }
}

impl Drop for FrameBuf {
    fn drop(&mut self) {
        match &self.inner {
            // The `Frame` itself goes back to the receiver right after.
            Inner::Whole(_) => {
                WHOLE_OUT.fetch_sub(1, Ordering::Relaxed);
            }
            Inner::Pool { index, .. } => give_frame(*index),
        }
    }
}
