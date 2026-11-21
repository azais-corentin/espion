//! The encoder's input, fed band by band while the encode runs (see "Live encode" in the crate
//! docs).
//!
//! An encode is armed by [`crate::Encoder::arm`] (open: any feeder may bind it) or
//! [`crate::Encoder::arm_local`] (bound at once to the caller). A feeder takes the input side
//! with [`begin`], a guard ([`Feeding`]) that holds the state word's busy bit, and appends
//! bands of whole 8-row MCU rows with [`Feeding::append`]; the codec waits whenever the bands
//! run out [INFERENCE: TRM v1.0 §38 gives the encoder no timeout]. The core that armed the
//! encode waits for its end ([`crate::Armed::wait`]) and does every teardown; it revokes the
//! input side first, waiting only while the busy bit is set.
//!
//! What a feeder may call: atomics, volatile accesses to the driver's descriptors through the
//! non-cacheable alias, and the 2D-DMA and JPEG registers. No critical section, no interrupt,
//! no esp-hal driver call: a bare core 1 may feed.
//!
//! The appended descriptors (TRM v1.0 §8.4.8, "Restart"): band `i` is a 2D-MOD1 descriptor of
//! its own sub-picture (buffer = the band's first row, HA = width, VA = its rows, hb x vb =
//! 128 x 8, `next` 0). The first band sets the TX link address and starts TX, then the codec
//! (TRM §38.7.1 order); each later band is linked from the one before (its DW4), then TX
//! `OUTLINK_RESTART` alone, as ESP-IDF's `dma2d_append`: a channel still working on the band
//! before follows the new link, a channel parked at `next` 0 starts again from there. The
//! TRM's sequence sets `OUTLINK_STOP` first; on the board that lost 5 of 1600 encodes (no end
//! within 100 ms), the restart alone none (`firmware/poc-p4-stream/README.md`, "Encoding
//! during the readout").

use core::sync::atomic::{AtomicU32, Ordering, fence};

use crate::dma2d::{self, Descriptor, out_reg};
use crate::{BLOCK_H, BLOCK_W, TX_CHANNEL, cache, jpeg};

/// Descriptors per encode: the bands one picture may be fed in.
pub const MAX_BANDS: usize = 32;

/// State word: the state in bits 0..8, [`BUSY`] while a feeder holds the input side.
pub(crate) static STATE: AtomicU32 = AtomicU32::new(IDLE);
/// The bound feeder's tag (valid in [`FEEDING`], [`FED`], [`ABANDONED`]).
static TAG: AtomicU32 = AtomicU32::new(0);
/// Rows fed and bands used so far (the feeder's; reset by the arming core).
static FED_ROWS: AtomicU32 = AtomicU32::new(0);
static BANDS: AtomicU32 = AtomicU32::new(0);
/// The armed picture: width | height << 16.
static GEOMETRY: AtomicU32 = AtomicU32::new(0);

/// No encode armed, or torn down.
pub(crate) const IDLE: u32 = 0;
/// Armed and open: the first [`begin`] binds it.
pub(crate) const ARMED: u32 = 1;
/// Bound to a feeder ([`TAG`]), bands still to come.
pub(crate) const FEEDING: u32 = 2;
/// Every band fed.
pub(crate) const FED: u32 = 3;
/// The feeder gave up ([`abandon`]): the rest never comes.
pub(crate) const ABANDONED: u32 = 4;
pub(crate) const STATE_MASK: u32 = 0xFF;
pub(crate) const BUSY: u32 = 1 << 8;

/// The state of an armed encode's input side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Armed, no feeder bound yet.
    Open,
    /// Bound to the feeder with this tag; bands still to come.
    Feeding(u32),
    /// Every band of the feeder with this tag fed.
    Fed(u32),
    /// The feeder with this tag gave up.
    Abandoned(u32),
    /// Not armed.
    Idle,
}

/// Why a band was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandError {
    /// Rows 0, not a multiple of 8, or past the picture's height.
    Rows,
    /// The band's start is not a multiple of [`crate::INPUT_ALIGN`].
    Alignment,
    /// Every descriptor is used: only the band that ends the picture may come now.
    Full,
}

/// The input side of the armed encode, held by one feeder; dropping it lets the arming core
/// take it back again.
pub struct Feeding {
    _private: (),
}

/// Takes the input side for the feeder `tag`: binds an open encode to it, or resumes the
/// encode it is bound to. `None` if no encode is armed, another feeder holds it, or it was
/// revoked. The tag changes only when an open encode is bound, so it is stable while bound.
pub fn begin(tag: u32) -> Option<Feeding> {
    let mut s = STATE.load(Ordering::Acquire);
    loop {
        match s {
            ARMED => {}
            FEEDING if TAG.load(Ordering::Relaxed) == tag => {}
            _ => return None,
        }
        match STATE.compare_exchange_weak(s, FEEDING | BUSY, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => {
                if s == ARMED {
                    TAG.store(tag, Ordering::Relaxed);
                }
                return Some(Feeding { _private: () });
            }
            Err(now) => s = now,
        }
    }
}

/// Gives up the encode bound to `tag` (its bands will not all come): the arming core halts
/// it. `false` if it is not bound to `tag` or every band was fed already.
pub fn abandon(tag: u32) -> bool {
    TAG.load(Ordering::Relaxed) == tag
        && STATE
            .compare_exchange(FEEDING, ABANDONED, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
}

/// The armed encode's input side, as the arming core sees it.
pub(crate) fn input() -> Input {
    view(STATE.load(Ordering::Acquire))
}

/// The input side of state word `s`.
fn view(s: u32) -> Input {
    let tag = TAG.load(Ordering::Relaxed);
    match s & STATE_MASK {
        ARMED => Input::Open,
        FEEDING => Input::Feeding(tag),
        FED => Input::Fed(tag),
        ABANDONED => Input::Abandoned(tag),
        _ => Input::Idle,
    }
}

/// Opens the input side of a new encode of `width` x `height` (the arming core; state
/// [`IDLE`]). `local`: bound at once to the caller (tag 0) instead of open.
pub(crate) fn arm(width: u16, height: u16, local: bool) -> Option<Feeding> {
    FED_ROWS.store(0, Ordering::Relaxed);
    BANDS.store(0, Ordering::Relaxed);
    GEOMETRY.store(
        u32::from(width) | u32::from(height) << 16,
        Ordering::Relaxed,
    );
    if local {
        TAG.store(0, Ordering::Relaxed);
        STATE.store(FEEDING | BUSY, Ordering::Release);
        Some(Feeding { _private: () })
    } else {
        STATE.store(ARMED, Ordering::Release);
        None
    }
}

/// Takes the input side back (the arming core): waits while a feeder holds it, then
/// [`IDLE`]. Returns the input side as it was.
pub(crate) fn revoke() -> Input {
    let mut s = STATE.load(Ordering::Acquire);
    loop {
        if s & BUSY != 0 {
            // A feeder is inside an append: a few register and descriptor writes.
            core::hint::spin_loop();
            s = STATE.load(Ordering::Acquire);
            continue;
        }
        match STATE.compare_exchange_weak(s, IDLE, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(now) => s = now,
        }
    }
    view(s)
}

impl Feeding {
    /// The bound feeder's tag.
    pub fn tag(&self) -> u32 {
        TAG.load(Ordering::Relaxed)
    }

    /// Rows fed so far (from the picture's top).
    pub fn fed_rows(&self) -> u16 {
        FED_ROWS.load(Ordering::Relaxed) as u16
    }

    /// The armed picture's width and height.
    pub fn geometry(&self) -> (u16, u16) {
        let g = GEOMETRY.load(Ordering::Relaxed);
        (g as u16, (g >> 16) as u16)
    }

    /// Descriptors still free; the last one is kept for the band that ends the picture.
    pub fn bands_left(&self) -> usize {
        MAX_BANDS - BANDS.load(Ordering::Relaxed) as usize
    }

    /// Appends the next band: `rows` rows (a multiple of 8) starting at `addr`, which must be
    /// in memory already (written by a DMA, or written back). The band that reaches the
    /// picture's height ends the input.
    pub fn append(&mut self, addr: usize, rows: u16) -> Result<(), BandError> {
        let (width, height) = self.geometry();
        let fed = self.fed_rows();
        if rows == 0
            || !rows.is_multiple_of(8)
            || u32::from(fed) + u32::from(rows) > u32::from(height)
        {
            return Err(BandError::Rows);
        }
        if !addr.is_multiple_of(crate::INPUT_ALIGN) {
            return Err(BandError::Alignment);
        }
        let i = BANDS.load(Ordering::Relaxed) as usize;
        let last = fed + rows == height;
        if i >= MAX_BANDS || (i == MAX_BANDS - 1 && !last) {
            return Err(BandError::Full);
        }
        let d = band_addr(i);
        crate::write_desc(
            d,
            &Descriptor::picture(
                addr as u32,
                u32::from(width),
                u32::from(rows),
                BLOCK_W,
                BLOCK_H,
            ),
        );
        BANDS.store(i as u32 + 1, Ordering::Relaxed);
        FED_ROWS.store(u32::from(fed + rows), Ordering::Relaxed);
        if last {
            STATE.store(FED | BUSY, Ordering::Release);
        }
        let link = dma2d::out(TX_CHANNEL, out_reg::LINK_CONF);
        if i == 0 {
            dma2d::out(TX_CHANNEL, out_reg::LINK_ADDR).write(d);
            fence(Ordering::SeqCst);
            link.modify(0, out_reg::LINK_START);
            jpeg().config().modify(|_, w| w.jpeg_start().set_bit());
        } else {
            // DW4 of the band before: through the non-cacheable alias, as every descriptor.
            let next = band_addr(i - 1) as usize + core::mem::offset_of!(Descriptor, next);
            let next = cache::non_cacheable(next) as *mut u32;
            // SAFETY: the `next` word of a descriptor in the driver's static.
            unsafe { core::ptr::write_volatile(next, d) };
            fence(Ordering::SeqCst);
            link.modify(0, out_reg::LINK_RESTART);
        }
        Ok(())
    }
}

impl Drop for Feeding {
    fn drop(&mut self) {
        STATE.fetch_and(!BUSY, Ordering::Release);
    }
}

/// Bus address of band descriptor `i`.
fn band_addr(i: usize) -> u32 {
    (crate::descs_addr() + core::mem::offset_of!(crate::Descs, bands)) as u32
        + (i * core::mem::size_of::<Descriptor>()) as u32
}
