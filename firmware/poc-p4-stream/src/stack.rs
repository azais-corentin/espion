//! The main stack's high-water mark: [`paint`] fills the free part of core 0's main stack with
//! a pattern at boot, [`main_unused`] counts the words never written since, from the bottom up.
//! The main stack is what internal RAM leaves after the statics (the stream's strip ring is
//! the largest of them), and the executor's tasks poll on it.

/// The pattern.
const PAINT: u32 = 0xA5C3_5A3C;
/// Bytes at the bottom the paint and the scan skip: esp-hal's stack guard word (a watchpoint
/// on a word near the bottom) lies there.
const SKIP: usize = 256;
/// Bytes below the stack pointer left alone while painting (the painting function's own
/// frame and anything an interrupt pushes meanwhile lies above it).
const MARGIN: usize = 1024;

unsafe extern "C" {
    static _stack_end_cpu0: u8;
    static _stack_start_cpu0: u8;
}

fn bounds() -> (usize, usize) {
    let end = &raw const _stack_end_cpu0 as usize;
    let top = &raw const _stack_start_cpu0 as usize;
    (end, top)
}

/// Paints the main stack from its bottom (past [`SKIP`]) to [`MARGIN`] below the stack
/// pointer, with interrupts masked. Once, first thing at boot.
#[inline(never)]
pub fn paint() {
    let (end, _) = bounds();
    let sp: usize;
    // SAFETY: reads the stack pointer.
    unsafe { core::arch::asm!("mv {0}, sp", out(reg) sp, options(nomem, nostack)) };
    let from = (end + SKIP).next_multiple_of(4);
    let to = sp.saturating_sub(MARGIN) & !3;
    critical_section::with(|_| {
        let mut a = from;
        while a < to {
            // SAFETY: free words of the main stack, below the live frames (stack pointer minus
            // the margin) and above its guard word; interrupts are masked meanwhile.
            unsafe { (a as *mut u32).write_volatile(PAINT) };
            a += 4;
        }
    });
}

/// Bytes of the main stack never written since [`paint`] (from its bottom, past [`SKIP`]),
/// and the stack's size.
pub fn main_unused() -> (usize, usize) {
    let (end, top) = bounds();
    let mut a = (end + SKIP).next_multiple_of(4);
    // SAFETY: words of the main stack below the deepest frame so far, read only.
    while a < top && unsafe { (a as *const u32).read_volatile() } == PAINT {
        a += 4;
    }
    (a - (end + SKIP).next_multiple_of(4), top - end)
}
