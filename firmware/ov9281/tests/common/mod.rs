//! Host stand-ins for the bus, the delay and the executor.

#![allow(dead_code)]

use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

use embedded_hal_async::{
    delay::DelayNs,
    i2c::{self, ErrorKind, ErrorType, I2c, Operation},
};

/// A bus error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusError;

impl i2c::Error for BusError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// An SCCB device: records register writes, answers reads from `chip_id`.
pub struct MockSensor {
    pub address: u8,
    pub chip_id: u16,
    /// Fail every transaction.
    pub fail: bool,
    /// `(register, value)` in write order.
    pub writes: Vec<(u16, u8)>,
}

impl MockSensor {
    pub fn new(address: u8) -> Self {
        Self {
            address,
            chip_id: 0x9281,
            fail: false,
            writes: Vec::new(),
        }
    }

    fn register(&self, reg: u16) -> u8 {
        match reg {
            0x300A => (self.chip_id >> 8) as u8,
            0x300B => self.chip_id as u8,
            _ => 0,
        }
    }
}

impl ErrorType for MockSensor {
    type Error = BusError;
}

impl I2c for MockSensor {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), BusError> {
        if self.fail || address != self.address {
            return Err(BusError);
        }
        let mut pointer = None;
        for op in operations {
            match op {
                Operation::Write([hi, lo, value]) => {
                    self.writes.push((u16::from_be_bytes([*hi, *lo]), *value));
                }
                Operation::Write([hi, lo]) => pointer = Some(u16::from_be_bytes([*hi, *lo])),
                Operation::Write(other) => panic!("unexpected write {other:02x?}"),
                Operation::Read(buf) => {
                    let start = pointer.expect("read without a register address");
                    for (i, b) in buf.iter_mut().enumerate() {
                        *b = self.register(start + i as u16);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Records the delays asked for.
#[derive(Default)]
pub struct MockDelay {
    pub ms: Vec<u32>,
}

impl DelayNs for MockDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        panic!("the driver waits in milliseconds");
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.ms.push(ms);
    }
}

/// Runs a future that never waits (the mocks complete at once).
pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}
