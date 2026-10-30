# Firmware rules

The board's hardware relies on what firmware does with the P4's pins. `firmware/cam-p4` must follow every rule below. Reviewers check the firmware against them by number, and the other hardware docs cite them as "firmware rule N".

## GPIO map

From the schematic (U1, the Waveshare ESP32-P4-Core module). The Ethernet pins are fixed (EMAC IO MUX). The others may still swap within their group at placement, as long as they follow the rules below.

| Signal | GPIO | Dir | Pull/default | Notes |
|---|---|---|---|---|
| — | 0, 1 | — | NC | The module's 32.768 kHz crystal |
| `PWR_SRC` | 2 | in | R26 10 kΩ pull-up (TPS2121 ST, open drain) | High = PoE powers +5V, low = USB. GPIO2's reset pull-up only adds to R26. Rules 4, 10 |
| `IMU_SDA` | 3 | I2C | R65 10 kΩ pull-up | JTAG MTDI pad, input after reset: safe for an open-drain bus. Rules 11, 12 |
| `IMU_SCL` | 4 | I2C | R64 10 kΩ pull-up | JTAG MTMS pad, input after reset. Rules 11, 12 |
| spare | 5 | — | NC | JTAG MTDO, an output: not for the I2C bus |
| `LED_PWR_EN` | 6 | out | R44 100 kΩ pull-down | TPS22918 load switch ON. Rule 4 |
| `LED_EN` | 7 | out | R49 100 kΩ pull-down | Lets STROBE light the ring; low for dark frames. Rules 4, 5 |
| `SCCB_SDA` | 8 | I2C | R33 1.8 kΩ pull-up to +3V3 | Camera registers. Rule 12 |
| `SCCB_SCL` | 9 | I2C | R34 1.8 kΩ pull-up to +3V3 | Rule 12 |
| `CAM_EN` | 10 | out | R35 47 kΩ pull-down | Camera module regulators. Rules 1, 12 |
| `XVS` | 11 | out (RMT) | Low through R31 1.5 kΩ + R32 1.8 kΩ (the level divider) | Frame trigger. Rules 1, 7 |
| `STROBE` | 12 | in (ETM) | Driven by buffer U7; R30 10 kΩ holds the camera side low | High during exposure; firmware timestamps it with a GPIO ETM event |
| `STATUS_LED` | 13 | out (RMT) | R29 10 kΩ pull-down on the LED's DIN (behind R28 220 Ω) | One-wire RGB LED. Rule 7 |
| `VBUS_SENSE` | 20 | in, ADC1 CH4 | R62 47 kΩ / R63 75 kΩ divider, C49 100 nF | Read as digital or analog. Rule 10 |
| spare | 21 | — | NC | ADC1 CH5: the only spare analog input |
| `VLED_SENSE` | 22 | ADC1 CH6 | +32V × 10/265 (R60/R61), 0–1.51 V | Rule 6 |
| `NTC_SENSE` | 23 | ADC1 CH7 | NTC divider (TH1, R59 10 kΩ) | Rule 5 |
| USB D− / D+ | 24 / 25 | USB | R24 / R23 22 Ω in series | USB Serial/JTAG (USB1P1): flashing, logs, JTAG. GPIO25 gets the D+ pull-up after every reset. Rule 10 |
| spare | 26, 27 | — | NC | USB FS OTG D−/D+ by default. GPIO27's reset state is documented inconsistently: measure before use |
| `RMII_CRS_DV` | 28 | in | None on the P4 side; the PHY's 104 kΩ pull-down (FX_HEN strap) | No P4 pull-up, ever. Rule 3 |
| `RMII_RXD0`, `RMII_RXD1` | 29, 30 | in | None | 22 Ω series resistors at the PHY |
| `MDC` | 31 | out | None | GPIO matrix. Rule 9 |
| `PPS` | 32 | out | None | EMAC PTP pulse, GPIO matrix signal 243 `emac_ptp_pps_pad_out` (TRM Table 10.12-1). Goes to TP1 and through R27 (0 Ω) to GPIO33 |
| `PPS_IN` | 33 | in (ETM) | Driven by `PPS` | Keep it an input. With R27 removed, capture on GPIO32 instead (see [mcu.md](mcu.md)) |
| `RMII_TXD0` | 34 | out | Floats (no pull anywhere) | Strap. Rule 8 |
| `RMII_TXD1` | 35 | out | R2 10 kΩ pull-up; BOOT button through R3 1 kΩ | Boot strap |
| — | 36 | — | NC; module pull-up R46 10 kΩ | Strap |
| — | 37, 38 | — | NC | Straps. UART0 TX/RX by default: GPIO37 carries the ROM log |
| — | 39–48 | — | NC | Unusable: their I/O domain (VDD_IO_5) is fed by ESP_LDO_VO4, which is 0 V until firmware sets it |
| `RMII_TX_EN` | 49 | out | None | Rule 9 |
| `RMII_REF_CLK` | 50 | in | None | 50 MHz from the PHY through R11 22 Ω. See Clocks |
| `PHY_RST_N` | 51 | out | R10 10 kΩ pull-down | Rule 3 |
| `MDIO` | 52 | in/out | R9 1.5 kΩ pull-up to PHY_3V3 | GPIO matrix |
| `BST_EN` | 53 | out | R48 100 kΩ pull-down | TPS61170 CTRL. Rule 4 |
| `IMU_INT1` | 54 | in | None: U12 drives it low from power-up | Wake-on-motion interrupt. Rule 11 |

- All used pins are in 3.3 V I/O domains. Inputs need a 3.3 V push-pull or a pulled-up open drain: V_IH is 0.75 × VDD = 2.48 V, so a 1.8 V signal does not read high. (P4 DS Table 5-4)
- Ethernet uses the EMAC's IO MUX pins, as esp-hal's P4 Ethernet example and Espressif's and Waveshare's P4 boards do. MDC, MDIO, I2C, RMT, PPS and the ETM event go through the GPIO matrix (the ETM event can use any GPIO 0–54, TRM §10.17).
- ADC: ATTEN3 covers 0–3300 mV, ±15 mV after calibration (P4 DS Table 5-6). ADC2 (GPIO49–54) is all taken by digital signals. esp-hal 1.2.2 has a P4 one-shot ADC driver (`unstable`).
- **During reset** every used pin is high-Z with no pull, except GPIO32–34 and 36–38 (input, no pull) and GPIO35 (input with pull-up). **After reset**, until firmware sets a pin: GPIO2 has input + pull-up (with EFUSE_DIS_PAD_JTAG = 0), GPIO3/4 are inputs, GPIO25 has the USB D+ pull-up; the rest stay high-Z. Internal pulls are 45 kΩ typ. (P4 DS Table 2-1, Table 5-4)
- If a pin moves at placement: no pulled-down output (`CAM_EN`, `XVS`, `LED_PWR_EN`, `LED_EN`, `BST_EN`) on GPIO2 (its 45 kΩ pull-up against 100 kΩ gives 3.3 × 100/145 = 2.3 V) or on the straps GPIO34–38. `STATUS_LED` only on GPIO6–13 (no reset pull-up). GPIO3/4 (JTAG MTDI/MTMS, inputs after reset) only for inputs or open-drain lines; GPIO5 (MTDO, an output) never for I2C. Keep GPIO20–23 for analog and `PPS`/`PPS_IN` on adjacent pins.

## Straps

The P4 reads these at reset. (TRM §12.2, Tables 12.2-1, 12.2-2, 12.2-4; P4 DS §3)

- **GPIO35 (boot mode):** 1 = boot from flash, 0 (with GPIO36 = 1) = download mode over USB Serial/JTAG, UART0 or SPI slave. R2 holds it at 1; the BOOT button pulls it to 0. The PHY's TXD1 is an input with no pull, so it never fights the strap. It must stay valid for t_H ≥ 3 ms after CHIP_PU rises. (P4 DS Table 3-2)
- **GPIO34 (JTAG source):** ignored while EFUSE_JTAG_SEL_ENABLE = 0 (the default); JTAG then comes from USB Serial/JTAG. It floats on this board. See rule 8.
- **GPIO36:** NC, 1 through the module's R46. It also selects UART0 ROM printing, but only when EFUSE_UART_PRINT_CONTROL ≠ 0. (TRM Table 12.2-3)
- **GPIO37, GPIO38:** NC, floating. They matter only when GPIO35 = 0 and GPIO36 = 0 (SPI download; 0/0/0/0 is invalid), which R46 prevents.
- **Flashing** needs neither button: the USB Serial/JTAG can force download mode from a normal boot (espflash does this). BOOT + RESET is the fallback. (TRM §12.2.2)

## Clocks

- **RMII:** the IP101GRI's 25 MHz crystal → the PHY makes the 50 MHz REF_CLK → GPIO50 (esp-hal `ExternalRefClock`). The PHY crystal clocks only RMII.
- **PTP:** the EMAC's PTP clock source is `XTAL` (default) or `PLL_F80M` (ESP-IDF `components/soc/esp32p4/include/soc/clk_tree_defs.h`, `soc_periph_emac_ptp_clk_src_t`). Both come from the P4's 40 MHz crystal, so the PTP clock and SYSTIMER share one crystal, as [timing.md](../timing.md) assumes.

## Rules

1. **XVS only while the camera is on.** Keep `XVS` low whenever `CAM_EN` is low: drive XVS low before CAM_EN falls, and start XVS edges only after the sensor's power-up sequence. XVS high pulses last ≥ 10 µs. Why: XVS high would back-feed the unpowered sensor through its I/O clamp.
2. **MIPI supply.** Set ESP_LDO_VO3 to 2.5 V before starting the CSI host (`esp32p4-csi` does this). Never program VO3 above 2.75 V. Why: the module powers the MIPI PHY from VO3, which is 0 V by default; the LDO can go up to 3.3 V but VDD_MIPI_DPHY is rated 2.75 V max. (P4 DS Table 5-1)
3. **PHY reset.** Hold `PHY_RST_N` low ≥ 11 ms after power-up, release it, wait ≥ 12.5 ms for the 50 MHz REF_CLK, then start the EMAC. The first MDIO frame carries ≥ 32 preamble bits. Keep GPIO28 (`RMII_CRS_DV`) an input with no pull, or driven low, from reset until ≥ 2.63 ms after `PHY_RST_N` rises. Why: GPIO28 shares the net with the PHY's FX_HEN strap, which only the PHY's internal 104 kΩ pull-down holds at 0 (twisted pair); a P4 pull-up would leave it mid-rail. (IP101G DS pin 19 p.16–17, pull-down p.12, Tlat_h Table 15 p.59)
4. **LED power only on PoE, in order.**
   - Raise `LED_PWR_EN` only after `PWR_SRC` has been high with no falling edge for ≥ 200 ms. Why: when PoE is plugged in while the board runs on USB, the mux (U4) keeps USB through its settling time t_SET, then picks PoE while the DP1435 may still be starting, and with no hysteresis it can toggle until PoE is stable. (SLVSEA3F §9.3.1, §9.3.7, Table 9-3; [power.md](power.md))
   - On: `LED_PWR_EN` high, wait ≥ 50 ms, then `BST_EN` high (the rail takes 50–120 ms to charge). Off: `BST_EN` low, then `LED_PWR_EN` low.
   - Never PWM `BST_EN`. Why: the TPS61170 CTRL pin decodes PWM/EasyScale; a clean low ≥ 2.5 ms resets the mode. (SLVS789D §8.4.1)
   - `LED_EN` low is what turns the light off. Why: +32V stays charged for tens of seconds after the boost stops (τ ≈ 58 s, [led.md](led.md)).
   - On a falling edge of `PWR_SRC` (GPIO interrupt: PoE lost, the mux has handed +5V to USB), drop `BST_EN` and then `LED_PWR_EN` at once, without the usual delays. Why: the boost draws up to 1.44 A, which must leave the USB port as soon as possible. ([power.md](power.md))
5. **LED exposure cap and heat.** Cap the exposure at **0.75 ms**. Why: the hardware timeout can end a pulse as early as ≈ 0.78 ms, so a longer exposure would be lit only in part on some boards; the cap also stays under the boost's sustained output (≈ 0.8 ms of light per frame at 120 fps) and bounds the sink MOSFET's average power ([led.md](led.md)). Derate or stop on `NTC_SENSE`; treat an open (≈ 3.3 V) or shorted (0 V) NTC as a fault and drop `BST_EN` and `LED_PWR_EN`.
6. **LED rail check.** Read `VLED_SENSE` just after an enabled pulse and again before the next XVS. Why: no droop after the pulse means an open string or no current; the second read shows the recovered rail. ([led.md](led.md))
7. **One RMT instance.** Create one `Rmt` and split its channels: XVS on TX channel 0, the status LED on channel 2 or 3. Why: XVS may need more than one memory block, which takes the next channels' blocks; the RMT source clock is shared by all channels. Only TX channel 3 has GDMA: if XVS ever needs DMA, move XVS to channel 3 and the LED to channel 1 or 2. (TRM §60.3.2.2, §60.3.3; P4 DS §4.2.2.17)
   - The status LED driver is `esp-hal-smartled` 0.18.0 (targets esp-hal ~1.2) with its `esp32p4` feature. If RMT channels run short, SPI MOSI or PARLIO can send the same waveform on the same pin, with no hardware change (estimate).
8. **Never burn `EFUSE_JTAG_SEL_ENABLE`.** Why: GPIO34 (`RMII_TXD0`) floats at reset (the PHY's TXD0 is an input with no pull), so it would pick the JTAG source at random. (TRM Table 12.2-4; IP101G DS §3.2 p.16)
9. **Outputs at boot.** Write each output's level before enabling its driver. Why: the P4's pins float through the ROM, the bootloader and every RESET press, so every output that reaches an active part has an external default (see the GPIO map) that a glitch must not undo.
   - `RMII_TX_EN`, `RMII_TXD0` and `MDC` have no default: R10 holds the PHY in reset until firmware releases `PHY_RST_N`, so they are ignored. `PPS` reaches only TP1 and, through R27, the P4's own GPIO33.
   - A glitch on `LED_EN` alone cannot light the ring: the gate also needs STROBE, which R30 holds low until firmware configures the sensor.
   - No pull-ups to 3.3 V sit on RMII inputs, so nothing back-powers the PHY.
10. **USB D+ pull-up follows VBUS.** Read `VBUS_SENSE` (GPIO20). While VBUS is absent, turn the USB Serial/JTAG D+ pull-up off (`USB_SERIAL_JTAG_CONF0_REG`: `PAD_PULL_OVERRIDE` = 1, `DP_PULLUP` = 0; P4 TRM Register 54.3); turn it back on when VBUS returns. Why: USB 2.0 §7.1.5.1 forbids a powered D+ pull-up without VBUS, and the board can run from PoE alone with a cable to an unpowered host. `PWR_SRC` can't stand in: it is high on PoE whether USB is plugged in or not. The pin reads high for VBUS ≥ 4.4 V; ADC1 CH4 at ATTEN3 (0–3.3 V) reads VBUS up to ≈ 5.3 V. ([mcu.md](mcu.md))
11. **IMU setup.** Keep U12's INT1 push-pull (`CTRL3_C.PP_OD` = 0, the default). Why: `IMU_INT1` has no pull resistor, so open drain would leave GPIO54 floating. U12 answers at I2C address 0x6A (SA0 tied low) on GPIO3/4, at up to 400 kHz. Wait ≥ 35 ms after power-up before the first access, and retry a NACK. Why: turn-on time T_on is 35 ms typical with no maximum given. (LSM6DS3TR-C DS Table 4 p.24)
12. **I2C bus recovery.** When starting the SCCB bus (GPIO8/9) and the IMU bus (GPIO3/4), read SDA first. If it is low, drive SCL as an open-drain GPIO and clock it ≥ 9 times until SDA reads high, then send a STOP (SDA low → high while SCL is high) before handing the pins to the I2C peripheral. Then reset the device: on the IMU write `CTRL3_C.SW_RESET` = 1; on the camera cycle `CAM_EN` (low, high, then the sensor's power-up sequence, keeping rule 1 for XVS). Why: a reset can leave a device mid-transfer, holding SDA low.

## Sources

- P4 DS: Espressif ESP32-P4 Series Datasheet v0.7.
- TRM: Espressif ESP32-P4 Technical Reference Manual v0.7 (Chip Boot Control is §12 in the body, chapter 13 in its own contents).
- Module: Waveshare ESP32-P4-Core schematic (16 MB version) and pin description, <https://docs.waveshare.com/ESP32-P4-Core>.
- HDG: Espressif ESP32-P4 hardware design guidelines (schematic checklist), docs.espressif.com, read 2026-10-28.
- IP101G DS: IC Plus IP101G datasheet IP101G-DS-R01.
- SLVSEA3F: TI TPS2120/TPS2121 datasheet (Aug 2020).
- SLVS789D: TI TPS61170 datasheet, rev D (Dec 2014).
- LSM6DS3TR-C DS: ST DocID030071 Rev 3 (May 2017).
- USB 2.0: Universal Serial Bus Specification Revision 2.0.
- ESP-IDF: `components/soc/esp32p4/include/soc/clk_tree_defs.h`.
