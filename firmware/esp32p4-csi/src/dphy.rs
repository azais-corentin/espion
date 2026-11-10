//! MIPI RX D-PHY test-code interface and status (TRM 42.5.3, ESP-IDF `mipi_csi_hal.c` /
//! `mipi_csi_phy_ll.h`).

use esp32p4::mipi_csi_host::RegisterBlock as Host;

/// `soc_mipi_csi_phy_pll_ranges` (ESP-IDF `esp_hal_cam/esp32p4/mipi_csi_periph.c`): inclusive
/// lane-rate range in Mbit/s and the D-PHY `hsfreqrange` code for it.
const HSFREQRANGE: [(u16, u16, u8); 38] = [
    (90, 99, 0x00),
    (100, 109, 0x10),
    (110, 129, 0x20),
    (130, 139, 0x01),
    (140, 149, 0x11),
    (150, 169, 0x21),
    (170, 179, 0x02),
    (180, 199, 0x12),
    (200, 219, 0x22),
    (220, 239, 0x03),
    (240, 249, 0x13),
    (250, 269, 0x23),
    (270, 299, 0x04),
    (300, 329, 0x14),
    (330, 359, 0x05),
    (360, 399, 0x15),
    (400, 449, 0x25),
    (450, 499, 0x06),
    (500, 549, 0x16),
    (550, 599, 0x07),
    (600, 649, 0x17),
    (650, 699, 0x08),
    (700, 749, 0x18),
    (750, 799, 0x09),
    (800, 849, 0x19),
    (850, 899, 0x29),
    (900, 949, 0x39),
    (950, 999, 0x0A),
    (1000, 1049, 0x1A),
    (1050, 1099, 0x2A),
    (1100, 1149, 0x3A),
    (1150, 1199, 0x0B),
    (1200, 1249, 0x1B),
    (1250, 1299, 0x2B),
    (1300, 1349, 0x3B),
    (1350, 1399, 0x0C),
    (1400, 1449, 0x1C),
    (1450, 1500, 0x2C),
];

/// Lowest lane rate in the IDF table (the TRM quotes 80 Mbit/s, the table starts at 90).
pub const MIN_MBPS: u16 = 90;
/// Highest lane rate (TRM 42.3: 1.5 Gbit/s per lane).
pub const MAX_MBPS: u16 = 1500;

/// `hsfreqrange` for a lane bit rate, `None` outside [`MIN_MBPS`]..=[`MAX_MBPS`].
pub fn hsfreqrange(lane_mbps: u16) -> Option<u8> {
    HSFREQRANGE
        .iter()
        .find(|(lo, hi, _)| (*lo..=*hi).contains(&lane_mbps))
        .map(|&(_, _, code)| code)
}

/// Test code "HS RX control of lane 0" (TRM 42.5.3.2): data bits [6:1] are `hsfreqrange`.
pub const TEST_CODE_HSFREQRANGE: u8 = 0x44;
/// Test code "normal operation": leaves the test interface inactive.
const TEST_CODE_NORMAL: u8 = 0x00;

const TESTCLR: u32 = 1 << 0;
const TESTCLK: u32 = 1 << 1;
const TESTEN: u32 = 1 << 16;

fn ctrl0(host: &Host, v: u32) {
    host.phy_test_ctrl0().write(|w| unsafe { w.bits(v) });
}

fn ctrl1(host: &Host, v: u32) {
    host.phy_test_ctrl1().write(|w| unsafe { w.bits(v) });
}

/// Resets the test-code logic: `PHY_TESTCLR` pulse, as `mipi_csi_hal_init` does (test code 0 on
/// DIN with TESTEN set, then TESTCLR 1 -> 0).
pub fn test_clear(host: &Host) {
    ctrl1(host, TESTEN);
    ctrl0(host, TESTCLR);
    ctrl0(host, 0);
}

/// Latches `code` as the current test code (falling TESTCLK edge while TESTEN is 1). Leaves
/// TESTCLK low and TESTEN high.
fn latch_code(host: &Host, code: u8) {
    ctrl0(host, 0);
    ctrl1(host, TESTEN | code as u32);
    ctrl0(host, TESTCLK);
    ctrl0(host, 0);
}

/// Writes one test code and its data (`s_mipi_csi_hal_phy_write_register`): the data is
/// programmed on the rising TESTCLK edge with TESTEN low.
pub fn write_test_code(host: &Host, code: u8, data: u8) {
    latch_code(host, code);
    ctrl1(host, data as u32);
    ctrl0(host, TESTCLK);
    ctrl0(host, 0);
}

/// Reads back what `code` reports on `PHY_TESTDOUT` (TRM 42.5.3.1: TESTDOUT shows the data of
/// the latched test code), then parks the interface on test code 0x00 as the TRM recommends.
/// No rising TESTCLK edge happens with TESTEN low, so nothing is programmed.
pub fn read_test_code(host: &Host, code: u8) -> u8 {
    latch_code(host, code);
    let dout = ((host.phy_test_ctrl1().read().bits() >> 8) & 0xFF) as u8;
    latch_code(host, TEST_CODE_NORMAL);
    ctrl1(host, 0);
    dout
}

/// D-PHY and host link state, read without side effects (no clear-on-read register is touched).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhyStatus {
    /// `CSI_HOST_PHY_STOPSTATE`: clock lane in Stop state (LP-11).
    pub clock_stop: bool,
    /// `CSI_HOST_PHY_STOPSTATE`: data lanes 0 and 1 in Stop state (LP-11).
    pub data_stop: [bool; 2],
    /// `CSI_HOST_PHY_RX.PHY_RXCLKACTIVEHS`: the clock lane receives a high-speed clock. The RX
    /// D-PHY has no PLL; this is the nearest thing to "lock".
    pub clock_active_hs: bool,
    /// `CSI_HOST_PHY_RX.PHY_RXULPSCLKNOT` (active low: `false` = clock lane in ULPS).
    pub clock_not_ulps: bool,
    /// `CSI_HOST_PHY_RX.PHY_RXULPSESC_n`: data lane n in ULPS.
    pub data_ulps: [bool; 2],
    /// `CSI_HOST_N_LANES` + 1.
    pub lanes: u8,
    /// `CSI_HOST_CSI2_RESETN`, `PHY_SHUTDOWNZ`, `DPHY_RSTZ` (all 1 when released).
    pub host_released: bool,
    /// Raw `CSI_HOST_PHY_STOPSTATE`.
    pub raw_stopstate: u32,
    /// Raw `CSI_HOST_PHY_RX`.
    pub raw_rx: u32,
}

/// Reads [`PhyStatus`].
pub fn status(host: &Host) -> PhyStatus {
    let stop = host.phy_stopstate().read().bits();
    let rx = host.phy_rx().read().bits();
    PhyStatus {
        clock_stop: stop & (1 << 16) != 0,
        data_stop: [stop & 1 != 0, stop & 2 != 0],
        clock_active_hs: rx & (1 << 17) != 0,
        clock_not_ulps: rx & (1 << 16) != 0,
        data_ulps: [rx & 1 != 0, rx & 2 != 0],
        lanes: (host.n_lanes().read().bits() & 0x7) as u8 + 1,
        host_released: host.csi2_resetn().read().bits() & 1 != 0
            && host.phy_shutdownz().read().bits() & 1 != 0
            && host.dphy_rstz().read().bits() & 1 != 0,
        raw_stopstate: stop,
        raw_rx: rx,
    }
}
