//! Internal LDO channels (`ESP_LDO_VO1`..`VO4`), as far as the MIPI D-PHY supply needs them.
//!
//! Port of ESP-IDF `esp_ldo_acquire_channel` (`components/esp_hw_support/ldo/esp_ldo_regulator.c`)
//! and `components/esp_hal_pmu/esp32p4/include/hal/ldo_ll.h`, including the eFuse calibration of
//! VO3/VO4. Only VO3 and VO4 are accepted: VO1/VO2 usually feed the flash and PSRAM.

use esp32p4 as pac;

/// Highest output the IDF recommends for an adjustable channel (`LDO_LL_RECOMMEND_MAX_VOLTAGE_MV`).
pub const MAX_MV: u16 = 2700;
/// Lowest output the IDF recommends (`LDO_LL_RECOMMEND_MIN_VOLTAGE_MV`).
pub const MIN_MV: u16 = 500;
/// Rail voltage: asking for exactly this passes the 3.3 V rail through (`tieh = 1`).
pub const RAIL_MV: u16 = 3300;

// `pmu_ext_ldo_reg_t` (hw_ver3/soc/pmu_struct.h), checked against the PAC's EXT_LDO_* fields.
const FORCE_TIEH_SEL: u32 = 1 << 7;
const XPD: u32 = 1 << 8;
const TIEH_SEL_MASK: u32 = 0b111 << 9;
const TIEH: u32 = 1 << 14;
// `pmu_ext_ldo_ana_reg_t`.
const MUL_SHIFT: u32 = 23;
const MUL_MASK: u32 = 0b111 << MUL_SHIFT;
const EN_VDET: u32 = 1 << 26;
const EN_CUR_LIM: u32 = 1 << 27;
const DREF_SHIFT: u32 = 28;
const DREF_MASK: u32 = 0xF << DREF_SHIFT;

/// An internal LDO channel that may be driven by this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LdoChannel {
    /// `ESP_LDO_VO3` (IDF channel id 3). Feeds `VDD_MIPI_DPHY` on the ESP32-P4-Core and on
    /// Espressif's dev kits.
    Vo3,
    /// `ESP_LDO_VO4` (IDF channel id 4).
    Vo4,
}

/// What [`enable_ldo`] programmed, for logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LdoSetting {
    /// Reference selector (`dref`, 0..=15).
    pub dref: u8,
    /// Multiplier selector (`mul`, 0..=7).
    pub mul: u8,
    /// `true` if the eFuse held calibration values for this channel (`K`, `Vos`, `C`).
    pub calibrated: bool,
    /// Expected output in mV for the chosen `dref`/`mul` (IDF formula, calibrated if available).
    pub expected_mv: u16,
    /// `PMU_EXT_LDO_*` register after the write.
    pub ctrl: u32,
    /// `PMU_EXT_LDO_*_ANA` register after the write.
    pub ana: u32,
}

/// Errors of [`enable_ldo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LdoError {
    /// Voltage outside [`MIN_MV`]..=[`MAX_MV`] and not [`RAIL_MV`].
    InvalidVoltage,
}

fn regs(channel: LdoChannel) -> (*mut u32, *mut u32) {
    // IDF `index_array = {0, 3, 1, 4}` maps unit (channel - 1) to `PMU.ext_ldo[index]`:
    // VO3 -> ext_ldo[1] = P0_0P2A, VO4 -> ext_ldo[4] = P1_0P2A. esp-hal's DSI driver drives
    // VO3 through the same `ext_ldo_p0_0p2a` registers.
    let pmu = unsafe { &*pac::PMU::ptr() };
    match channel {
        LdoChannel::Vo3 => (
            pmu.ext_ldo_p0_0p2a().as_ptr(),
            pmu.ext_ldo_p0_0p2a_ana().as_ptr(),
        ),
        LdoChannel::Vo4 => (
            pmu.ext_ldo_p1_0p2a().as_ptr(),
            pmu.ext_ldo_p1_0p2a_ana().as_ptr(),
        ),
    }
}

/// eFuse calibration (`ldo_ll_voltage_to_dref_mul`), as fixed point ×1000: `(K, Vos, C)`.
fn calibration(channel: LdoChannel) -> Option<(i32, i32, i32)> {
    let efuse = unsafe { &*pac::EFUSE::ptr() };
    // EFUSE_RD_MAC_SYS_2..4 (offsets 0x4c..0x54), field layout from hw_ver3/soc/efuse_struct.h.
    let sys2 = efuse.rd_mac_sys2().read().bits();
    let sys3 = efuse.rd_mac_sys3().read().bits();
    let sys4 = efuse.rd_mac_sys4().read().bits();
    let blk_version = ((sys2 >> 11) & 0x3) * 100 + ((sys2 >> 8) & 0x7);
    if blk_version < 1 {
        return None;
    }
    let (k, vos, c) = match channel {
        LdoChannel::Vo3 => ((sys3 >> 6) & 0xFF, (sys3 >> 14) & 0x3F, (sys3 >> 20) & 0x3F),
        LdoChannel::Vo4 => (
            ((sys4 & 0x3) << 6) + ((sys3 >> 26) & 0x3F),
            (sys4 >> 2) & 0x3F,
            (sys4 >> 8) & 0x3F,
        ),
    };
    let (k, vos, c) = (k as i32, vos as i32, c as i32);
    let k_1000 = match k {
        0 => 1000,
        k if k & 0x80 != 0 => -(k & 0x7F) + 975,
        k => k + 975,
    };
    let vos_1000 = match vos {
        0 => 0,
        v if v & 0x20 != 0 => -(v & 0x1F) - 3,
        v => v - 3,
    };
    let c_1000 = match c {
        0 => 1000,
        c if c & 0x20 != 0 => -(c & 0x1F) + 990,
        c => c + 990,
    };
    Some((k_1000, vos_1000, c_1000))
}

/// Turns on internal LDO `channel` at `voltage_mv`, as ESP-IDF's `esp_ldo_acquire_channel` does
/// for a free channel: current limit on, `dref`/`mul` from the (eFuse-calibrated) formula,
/// software ownership, ripple suppression, power up, current limit off.
///
/// The output needs time to settle before the D-PHY is used: esp-hal's DSI driver waits 500 µs.
/// This function does not wait; start the CSI receiver later (e.g. after an
/// `embassy_time::Timer::after_micros(500)`).
///
/// Whether a board needs this depends on its schematic: the ESP32-P4-Core runs `VDD_MIPI_DPHY`
/// from VO3, which is 0 V after reset. Calling it on a board where VO3 is unconnected is harmless.
pub fn enable_ldo(channel: LdoChannel, voltage_mv: u16) -> Result<LdoSetting, LdoError> {
    if voltage_mv != RAIL_MV && !(MIN_MV..=MAX_MV).contains(&voltage_mv) {
        return Err(LdoError::InvalidVoltage);
    }
    let cal = calibration(channel);
    let (k_1000, vos_1000, c_1000) = cal.unwrap_or((1000, 0, 1000));

    // Exhaustive search, same arithmetic as `ldo_ll_voltage_to_dref_mul`.
    let mut best = (i32::MAX, 0u8, 0u8, 0i32);
    for dref in 0u8..16 {
        let vref_20 = if dref < 9 {
            10 + dref as i32
        } else {
            20 + (dref as i32 - 9) * 2
        };
        for mul in 0u8..8 {
            let vout_80000000 = (vref_20 * k_1000 + 20 * vos_1000) * (4000 + mul as i32 * c_1000);
            let diff = (voltage_mv as i32 * 80000 - vout_80000000).abs();
            if diff < best.0 {
                best = (diff, dref, mul, vout_80000000);
            }
        }
    }
    let (_, dref, mul, vout_80000000) = best;
    let use_rail = voltage_mv == RAIL_MV;

    let (ctrl, ana) = regs(channel);
    let setting = critical_section::with(|_| unsafe {
        // ldo_ll_enable_current_limit(true)
        ana.write_volatile(ana.read_volatile() | EN_CUR_LIM);
        // ldo_ll_adjust_voltage
        let c = ctrl.read_volatile();
        ctrl.write_volatile(if use_rail { c | TIEH } else { c & !TIEH });
        let a = ana.read_volatile() & !(DREF_MASK | MUL_MASK);
        ana.write_volatile(a | ((dref as u32) << DREF_SHIFT) | ((mul as u32) << MUL_SHIFT));
        // ldo_ll_set_owner(SW): force_tieh_sel = 1, tieh_sel = 0
        let c = ctrl.read_volatile() & !TIEH_SEL_MASK;
        ctrl.write_volatile(c | FORCE_TIEH_SEL);
        // ldo_ll_enable_ripple_suppression(true)
        ana.write_volatile(ana.read_volatile() | EN_VDET);
        // ldo_ll_enable(true)
        ctrl.write_volatile(ctrl.read_volatile() | XPD);
        // ldo_ll_enable_current_limit(false)
        ana.write_volatile(ana.read_volatile() & !EN_CUR_LIM);
        (ctrl.read_volatile(), ana.read_volatile())
    });

    Ok(LdoSetting {
        dref,
        mul,
        calibrated: cal.is_some(),
        expected_mv: if use_rail {
            RAIL_MV
        } else {
            (vout_80000000 / 80000) as u16
        },
        ctrl: setting.0,
        ana: setting.1,
    })
}

/// Powers `channel` down (`xpd = 0`), as `esp_ldo_release_channel` does for the last user.
pub fn disable_ldo(channel: LdoChannel) {
    let (ctrl, _) = regs(channel);
    critical_section::with(|_| unsafe { ctrl.write_volatile(ctrl.read_volatile() & !XPD) });
}
