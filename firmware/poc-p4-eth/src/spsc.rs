//! Single-producer single-consumer queue of `N` slots for passing items from one core to the
//! other without locks: the two indices are atomics, each written by one side only, and a
//! slot belongs to exactly one side at a time (Lamport's ring). The producer never overwrites:
//! when the queue is full it counts the item as dropped and keeps going.
//!
//! Items are filled and read in place ([`Producer::push_with`], [`Consumer::pop_with`]), so a
//! large item is never copied through a stack.
//!
//! No dependency at all, so the host tests run without the firmware build:
//! `rustc --edition 2024 --test src/spsc.rs -o /tmp/spsc-test && /tmp/spsc-test`.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// The queue. Lives in a `static`; [`Spsc::split`] hands out its two ends once.
pub struct Spsc<T, const N: usize> {
    /// Writer: the consumer. Items taken out so far (wrapping).
    head: AtomicU32,
    /// Writer: the producer. Items published so far (wrapping).
    tail: AtomicU32,
    /// Writer: the producer. Items not queued because every slot was full.
    dropped: AtomicU32,
    /// `split` ran.
    taken: AtomicBool,
    slots: [UnsafeCell<T>; N],
}

// SAFETY: a slot is accessed by one side at a time: the producer only between `tail` and
// `head + N`, the consumer only between `head` and `tail`, and each side publishes its index
// with Release after its access and reads the other's with Acquire before.
unsafe impl<T: Send, const N: usize> Sync for Spsc<T, N> {}

impl<T, const N: usize> Spsc<T, N> {
    const CAPACITY_OK: () = assert!(N > 0 && N < (1 << 31));

    /// A queue over `slots` (their initial contents are never read).
    pub const fn new(slots: [UnsafeCell<T>; N]) -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            taken: AtomicBool::new(false),
            slots,
        }
    }

    /// The two ends, once; `None` afterwards.
    pub fn split(&'static self) -> Option<(Producer<T, N>, Consumer<T, N>)> {
        if self.taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some((Producer { q: self }, Consumer { q: self }))
    }

    /// The slot of the wrapping index `i`.
    fn slot(&self, i: u32) -> *mut T {
        // `% N` with a constant `N > 0`: always in bounds, no panic path.
        self.slots[i as usize % N].get()
    }
}

/// The producing end. `Send`: move it to the producing core.
pub struct Producer<T: 'static, const N: usize> {
    q: &'static Spsc<T, N>,
}

impl<T, const N: usize> Producer<T, N> {
    /// Fills the next free slot with `fill` and publishes it. If every slot is full, counts a
    /// drop, does not call `fill` and returns `false`.
    pub fn push_with(&mut self, fill: impl FnOnce(&mut T)) -> bool {
        let q = self.q;
        let tail = q.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(q.head.load(Ordering::Acquire)) as usize >= N {
            let d = q.dropped.load(Ordering::Relaxed);
            q.dropped.store(d.wrapping_add(1), Ordering::Relaxed);
            return false;
        }
        // SAFETY: the slot lies between `head + N` and `tail`: the consumer has released it
        // (Acquire above) and does not touch it until `tail` is published below.
        fill(unsafe { &mut *q.slot(tail) });
        q.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }
}

/// The consuming end.
pub struct Consumer<T: 'static, const N: usize> {
    q: &'static Spsc<T, N>,
}

impl<T, const N: usize> Consumer<T, N> {
    /// Hands the oldest item to `read` and frees its slot; `None` if the queue is empty.
    pub fn pop_with<R>(&mut self, read: impl FnOnce(&T) -> R) -> Option<R> {
        let q = self.q;
        let head = q.head.load(Ordering::Relaxed);
        if head == q.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the slot lies between `head` and `tail`: published by the producer (Acquire
        // above), which does not touch it until `head` is published below.
        let r = read(unsafe { &*q.slot(head) });
        q.head.store(head.wrapping_add(1), Ordering::Release);
        Some(r)
    }

    /// Items the producer dropped because the queue was full, since the start.
    pub fn dropped(&self) -> u32 {
        self.q.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::thread;

    /// A fresh queue in a `static` of its own (the ends need `'static`, as in the firmware).
    macro_rules! queue {
        ($n:literal) => {{
            static Q: Spsc<u64, $n> = Spsc::new([const { UnsafeCell::new(0) }; $n]);
            &Q
        }};
    }

    #[test]
    fn split_once() {
        let q = queue!(2);
        assert!(q.split().is_some());
        assert!(q.split().is_none());
    }

    #[test]
    fn fifo_full_and_drops() {
        let (mut p, mut c) = queue!(3).split().unwrap();
        assert_eq!(c.pop_with(|&v| v), None);
        for v in 1..=3 {
            assert!(p.push_with(|s| *s = v));
        }
        let mut called = false;
        assert!(!p.push_with(|_| called = true));
        assert!(!called);
        assert_eq!(c.dropped(), 1);
        assert_eq!(c.pop_with(|&v| v), Some(1));
        assert!(p.push_with(|s| *s = 4));
        for v in 2..=4 {
            assert_eq!(c.pop_with(|&v| v), Some(v));
        }
        assert_eq!(c.pop_with(|&v| v), None);
    }

    #[test]
    fn indices_wrap() {
        let q = queue!(4);
        q.head.store(u32::MAX - 1, Ordering::Relaxed);
        q.tail.store(u32::MAX - 1, Ordering::Relaxed);
        let (mut p, mut c) = q.split().unwrap();
        for v in 0..10 {
            assert!(p.push_with(|s| *s = v));
            assert!(p.push_with(|s| *s = v + 100));
            assert_eq!(c.pop_with(|&v| v), Some(v));
            assert_eq!(c.pop_with(|&v| v), Some(v + 100));
        }
        assert_eq!(c.dropped(), 0);
    }

    /// Two threads: every value arrives once, in order, or is counted as dropped.
    #[test]
    fn two_threads_in_order() {
        const COUNT: u64 = 200_000;
        let (mut p, mut c) = queue!(4).split().unwrap();
        let producer = thread::spawn(move || {
            let mut sent = 0;
            for v in 0..COUNT {
                if p.push_with(|s| *s = v) {
                    sent += 1;
                }
            }
            sent
        });
        let mut last = None;
        let mut got = 0u64;
        loop {
            match c.pop_with(|&v| v) {
                Some(v) => {
                    assert!(last.is_none_or(|l| v > l));
                    last = Some(v);
                    got += 1;
                }
                None if producer.is_finished() => {
                    while let Some(v) = c.pop_with(|&v| v) {
                        assert!(last.is_none_or(|l| v > l));
                        last = Some(v);
                        got += 1;
                    }
                    break;
                }
                None => thread::yield_now(),
            }
        }
        let sent = producer.join().unwrap();
        assert_eq!(got, sent);
        assert_eq!(got + u64::from(c.dropped()), COUNT);
    }
}
