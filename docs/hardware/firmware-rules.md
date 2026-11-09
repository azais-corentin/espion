# Firmware rules for the espion-cam board

What `firmware/cam-p4` must do for this board to work as designed: the pin map, the boot
straps it must leave alone and the behaviour the hardware relies on. `firmware/cam-p4` has no
pin map yet (it only initialises esp-hal and idles); when it gets one, it must follow the
[GPIO map](#gpio-map) below. A GPIO move on the board changes this file and the firmware
together.

Sources: Espressif ESP32-P4 datasheet v0.7 (`esp32-p4_datasheet_en.pdf`, "P4 DS" below),
ESP32-P4 technical reference manual (preliminary, "TRM"), ESP32-P4 Hardware Design
Guidelines ("HDG"), the Waveshare ESP32-P4-Core module schematic (U6's Datasheet field) and
the part datasheets named in each rule. Block details: [mcu.md](mcu.md),
[imu.md](imu.md), [ethernet.md](ethernet.md), [camera.md](camera.md), [led.md](led.md),
[power.md](power.md).

## GPIO map

Every U6 module pin that carries a net. Direction is seen from the P4. Pulls: ↑ to +3V3, ↓ to
GND, on the board; "internal" is the P4 pad pull the firmware sets. All P4 I/O domains
(VDD_LP, VDD_IO_0, VDD_IO_4, VDD_IO_6) run from the module's ESP_3V3, which is +3V3, so every
signal below is a 3.3 V signal (P4 DS Table 2-1 pp.14–16; Waveshare module schematic p.1).

| GPIO | U6 pin | Net | Function / peripheral | Dir | Pull | Constraints |
|---|---|---|---|---|---|---|
| — | 1 | `ESP_EN` | CHIP_PU (chip enable, reset) | in | R33 ↑, C27 to GND (their time constant sets the power-up delay, [Straps](#esp32-p4)) | SW1 RESET pulls it low. Not a GPIO |
| GPIO2 | 4 | `PWR_SRC` | TPS2121 ST: high = PoE (IN1), low = USB (IN2) | in | R29 ↑; internal pull-up **off** | MTCK pad, internal pull-up on after reset (P4 DS Table 2-1 note 5). Input only, never driven: the node is the TPS2121's open-drain ST and also feeds the R75 switchover hysteresis. Select the GPIO function (IO MUX function 0 is JTAG) |
| GPIO3 | 5 | `IMU_SDA` | I²C controller A, SDA (accelerometer) | I/O, open drain | R74 ↑; internal off | MTDI pad: select the GPIO/matrix function. ≤ 400 kHz |
| GPIO4 | 6 | `IMU_SCL` | I²C controller A, SCL | out, open drain | R73 ↑; internal off | MTMS pad: select the GPIO/matrix function |
| GPIO6 | 8 | `VBUS_SENSE` | Digital input: USB host power present | in | R27/R28 divider from VBUS, C26; internal off | VDD_LP pad, no ADC on this pin (touch channel only). The RC edge (R27 ∥ R28 × C26) is slow and the P4 publishes no input hysteresis: debounce |
| GPIO7 | 9 | `XVS` | RMT TX: camera frame trigger | out | R40/R44 divider to GND: the camera sees V(GPIO7) × R44/(R40 + R44) | Load R40 + R44. RMT through the GPIO matrix |
| GPIO8 | 10 | `SCCB_SDA` | I²C controller B, SDA (camera SCCB) | I/O, open drain | R41 ↑; internal off | ≤ 400 kHz |
| GPIO9 | 11 | `SCCB_SCL` | I²C controller B, SCL | out, open drain | R42 ↑; internal off | |
| GPIO10 | 12 | `CAM_EN` | Camera module PWDN (FFC pin 11) | out | R43 ↓ | Polarity unknown until measured (FW-17). R52 (not fitted) can also route it to FFC pin 12 |
| GPIO11 | 13 | `LED_EN` | LED gate enable (U9 input) | out | R56 ↓ | Default low. TP11 |
| GPIO12 | 14 | `STROBE` | GPIO ETM event: STROBE edge capture | in | none (driven push-pull by U7) | Never an output: U7 drives the net, which also feeds the STROBE timeout (R54). TP9 |
| GPIO13 | 15 | `STATUS_LED` | RMT TX: D1 data (through R30) | out | R26 ↓ at D1's DIN | Timing and byte order: FW-21 |
| GPIO20 | 16 | `LED_PWR_EN` | U8 load switch ON (LED boost supply) | out | R57 ↓ | Default low. No reset pull on the pad, R57 holds it |
| GPIO21 | 17 | `VLED_SENSE` | ADC1 channel 5: +32V monitor | analog | R71/R72 divider from +32V, C49 at the pin | V(+32V) = V(pin) × (R71 + R72)/R72 |
| GPIO22 | 18 | `PHY_RST_N` | IP101G RESET_N | out, push-pull | R8 ↓ | Low = PHY in reset. TP1 |
| GPIO23 | 19 | `LED_K_SENSE` | ADC1 channel 7: LED sink drain (`LED_K`, Q1) | analog | R70 from `LED_K`, R76 to GND, C48 at the pin | V(`LED_K`) = V(pin) × (R70 + R76)/R76. Sampled late in each LED pulse (FW-22) |
| — | 28–33 | `CSI_D0_N`, `CSI_D0_P`, `CSI_CLK_P`, `CSI_CLK_N`, `CSI_D1_N`, `CSI_D1_P` | MIPI CSI-2 receiver, 2 lanes (dedicated pins) | in | — | Fixed lane order, no remapping or P/N swap in the P4 ([camera.md](camera.md#csi-2-link)) |
| GPIO24 | 37 | `USB_D_N` | USB Serial/JTAG D− (USB 1.1 FS PHY) | I/O | R32 series | Keep in USB function (FW-1) |
| GPIO25 | 38 | `USB_D_P` | USB Serial/JTAG D+ | I/O | R31 series; internal D+ pull-up | D+ pull-up off while `VBUS_SENSE` is low (FW-4) |
| GPIO28 | 40 | `RMII_CRS_DV` | EMAC RMII CRS_DV (IO MUX function 3) | in | none (R15 at the PHY); internal **off** | Shares the IP101G FX_HEN strap: no internal pull while the PHY leaves reset (FW-11) |
| GPIO29 | 41 | `RMII_RXD0` | EMAC RMII RXD0 (function 3) | in | none (R11 at the PHY) | |
| GPIO30 | 42 | `RMII_RXD1` | EMAC RMII RXD1 (function 3) | in | none (R12 at the PHY) | |
| GPIO31 | 43 | `MDC` | EMAC MDC through the GPIO matrix | out | none | |
| GPIO32 | 44 | `PPS` | EMAC PTP PPS output (matrix signal `emac_ptp_pps_pad_out`) | out | none; R34 link to GPIO33, TP6 | Never select its EMAC function-3 RMII clock |
| GPIO33 | 45 | `PPS_IN` | GPIO ETM event: PPS edge capture | in | none | Input only: R34 ties it to GPIO32. Never select its EMAC function-3 TX_EN |
| GPIO34 | 46 | `RMII_TXD0_MCU` | EMAC RMII TXD0 (function 3) | out | none; R35 series | Strap (JTAG source). No firmware pulls ([Straps](#straps)) |
| GPIO35 | 47 | `RMII_TXD1_MCU` | EMAC RMII TXD1 (function 3) | out | R38 ↑, internal pull-up at reset; R36 series; R39 to SW2 BOOT | Boot strap. No firmware pulls. SW2 is not a runtime button (FW-3) |
| GPIO49 | 67 | `RMII_TX_EN_MCU` | EMAC RMII TX_EN (function 3) | out | none; R37 series | |
| GPIO50 | 68 | `RMII_REF_CLK` | EMAC RMII clock input (`EMAC_CLK_EXT_IN`), 50 MHz from the PHY | in | none (R13 at the PHY) | Never an output |
| GPIO51 | 69 | `IMU_INT1` | GPIO interrupt from the accelerometer | in | none (sensor INT1 is push-pull); internal off | Active low by default |
| GPIO52 | 70 | `MDIO` | EMAC MDIO through the GPIO matrix | I/O | R14 ↑ | PHY address 1 |
| GPIO53 | 71 | `BST_EN` | TPS61170 CTRL: boost enable and output set point (PWM) | out | R59 ↓ (plus the TPS61170's internal pull-down, 800 kΩ, SLVS789D) | Steady high to start, 50 kHz PWM in operation, low ≥ 2.5 ms to stop (FW-8) |
| — | 74 | `+3V3` | VBAT supply pin | power | | |
| — | 75, 76 | `+3V3` | ESP_3V3 supply pins | power | | |
| — | 20, 27, 34, 55, 73 | `GND` | Ground | | | |

Pins left unconnected on the board: 2/3 GPIO0/GPIO1 (the module's 32.768 kHz crystal),
7 GPIO5 (MTDO), 21–26 MIPI DSI, 35/36 USB 2.0 HS (USBD_N/P), 39 GPIO27, 48 GPIO36 (strap,
pulled up on the module), 49/50 GPIO37/GPIO38 (straps, UART0), 51–54 ESP_LDO_VO1–VO4 (used
inside the module), 56 GPIO26, 57–66 GPIO39–GPIO48 and 72 GPIO54.

Peripheral use: two HP I²C controllers (accelerometer on GPIO3/4, SCCB on GPIO8/9; P4 DS p.4),
three of the four RMT TX channels (XVS, STATUS_LED, the `BST_EN` PWM; TRM pp.1007–1008), two GPIO
ETM event channels (STROBE, PPS_IN; any GPIO 0–54 per channel, TRM p.1017), ADC1 channels 5 and 7
(P4 DS Table 2-7 pp.24–25), the EMAC in RMII mode on its IO MUX function-3 pins (P4 DS
Table 2-3 pp.21–22) with MDC/MDIO and PPS through the GPIO matrix (TRM p.1002, p.1007), and
USB Serial/JTAG on GPIO24/25 (P4 DS p.28 note 1).

## Straps

### ESP32-P4

The P4 samples GPIO34–GPIO38 at reset (P4 DS §3 p.36, Table 3-3 p.37; TRM Table 12.2-2
p.1220). Timing (P4 DS Table 3-2 p.37): the rails must be stable before CHIP_PU goes high
(t_SU ≥ 0 ms); CHIP_PU rises R33 × C27 × ln 4 after +3V3 (an RC delay, always above 0), which meets it. The
straps must then stay valid for t_H = 3 ms after CHIP_PU is high; they are set by static
resistors (R38, the module's 10 kΩ on GPIO36), so they hold as long as the board is powered.

| Strap | U6 pin / net | Board | Level at reset | Meaning |
|---|---|---|---|---|
| GPIO34 | 46 `RMII_TXD0_MCU` | no pull; the IP101G TXD0 input is high impedance | floating | JTAG source select, ignored while EFUSE_JTAG_SEL_ENABLE = 0 (the default; P4 DS Table 3-7 p.39) |
| GPIO35 | 47 `RMII_TXD1_MCU` | R38 ↑ plus the internal pull-up; SW2 pulls it through R39 | 1; below VIL with SW2 held (formula below) | 1 = SPI boot; 0 with GPIO36 = 1 = joint download boot (USB Serial/JTAG download included) |
| GPIO36 | 48, not connected | 10 kΩ ↑ on the module (Waveshare schematic p.1) | 1 | Makes GPIO35 = 0 a valid download combination |
| GPIO37, GPIO38 | 49, 50, not connected | none | floating | Don't care in SPI and joint download boot (TRM p.1220) |

SW2 held: R38 ∥ the internal pull-up (R_pu, 45 kΩ typical, P4 DS Table 5-4 p.90) pulls GPIO35
to V(+3V3) × R39 / (R39 + (R38 ∥ R_pu)). At the top of the +3V3 set-point range
([README](README.md#rails)) that must stay below VIL = 0.25 × V(+3V3) at the bottom of the range
(P4 DS Table 5-4 p.89). After boot GPIO34/35 become RMII transmit outputs; they carry no
other load that could disturb the next reset.

### IP101G PHY

The PHY latches its straps when RESET_N rises (IP101G datasheet p.17, p.47 Table 4, p.53):
PHY address 1 (LED0/LED3 pins, R1/R7), RMII mode (COL/RMII, R10), LED mode 2 (CRS/LEDMOD,
R9) and twisted-pair mode (RXDV/CRS_DV/FX_HEN, the PHY's internal pull-down). Only the last
one is shared with a P4 pin: GPIO28 through R15, with no board strap resistor. A P4 pull-up
there can latch fibre mode, so GPIO28 keeps its internal pulls off (FW-11). Details:
[ethernet.md](ethernet.md#straps).

## Rules

### Boot, USB and eFuses

- **FW-1** Keep GPIO24/GPIO25 in their USB Serial/JTAG function and keep the CDC RTS/DTR reset
  recognition on, so the host can reset the board into download mode without buttons (TRM
  pp.3190–3191). USB Serial/JTAG is the only console, JTAG and flashing path: UART0
  (GPIO37/38) is not brought out.
- **FW-2** Never burn these eFuses (P4 DS p.28 note 1, pp.38–39):
  - EFUSE_USB_PHY_SEL: moves USB Serial/JTAG to GPIO26/27, which are unconnected.
  - EFUSE_DIS_USB_JTAG and EFUSE_JTAG_SEL_ENABLE: pad JTAG would take GPIO2–5 (PWR_SRC and the
    accelerometer bus), and the GPIO34 JTAG strap has no defined level.
  - The download-mode and USB-Serial-JTAG disables: the board has no UART0 to recover with.
  - EFUSE_0PXA_TIEH_SEL_0: selects 1.8 V VDDO_FLASH, but the module's flash runs at 3.3 V.
- **FW-3** GPIO34 and GPIO35 are boot straps shared with RMII TXD0/TXD1: no firmware pulls on
  them. SW2 BOOT is not a runtime button: while it is held, any reset (watchdog, brown-out,
  host RTS/DTR) enters download mode instead of running the firmware. While the firmware
  drives TXD1, SW2 only adds a load of V(TXD1)/R39 through R39, which the P4's
  output (20 mA default drive, P4 DS p.17 note 3) holds; whether Ethernet frames survive it
  is a first-board check ([mcu.md](mcu.md#first-board-checks)).
- **FW-4** `VBUS_SENSE` (GPIO6) is a digital input. While it reads low (no USB host power),
  turn the USB Serial/JTAG D+ pull-up off; turn it back on when it reads high. The input
  edge is slow (time constant R27 ∥ R28 × C26) and the P4 datasheet gives no input hysteresis for
  this pad, so debounce the edges or poll the level. Levels: [power.md](power.md#usb-vbus).

### Power source and LED supply

- **FW-5** `PWR_SRC` (GPIO2): input only, internal pull-up off, never driven. High = PoE (IN1),
  low = USB (IN2) (TI TPS2121 SLVSEA3F p.18). Driving it would fight the TPS2121's open-drain
  ST output and shift the switchover thresholds set by R75.
- **FW-6** LEDs stay off on USB power alone, by this rule and no hardware gate (user decision
  2026-10-31): `LED_PWR_EN` (GPIO20) is low whenever `PWR_SRC` is low. Take the `PWR_SRC`
  falling edge (PoE lost) as an interrupt and run the PoE-loss power-down of FW-7 at once.
- **FW-7** LED supply sequence. This is the one place it is stated; [led.md](led.md) and
  [power.md](power.md) follow it.
  - **Normal power-down** (any LED shutdown while on PoE): `LED_EN` low, `BST_EN` low (PWM
    stopped, pin held low), wait ≥ 3 ms, then `LED_PWR_EN` low. `LED_EN` first stops the LED
    current pulses. The TPS61170 enters shutdown only after CTRL has been low for more than
    2.5 ms (TI TPS61170 SLVS789D p.5 t_off, §8.3.5 p.10); a shorter low only lowers its feedback
    reference through the 180 µs t_REF filter (§8.4.2 p.11, p.5). The 3 ms (2.5 ms plus margin)
    lets the boost shut down by its own enable before U8 removes its input.
  - **PoE-loss power-down** (on the `PWR_SRC` falling edge, the board now running on USB):
    `LED_EN` low, `BST_EN` low, `LED_PWR_EN` low, back to back with no wait, because the USB
    supply must not carry the boost. The boost then stops on its collapsing input, through its
    undervoltage lockout (p.5); this is accepted, as `LED_EN` is already low and no LED
    current flows.
  - **Power-up** (on PoE only, FW-6): `LED_PWR_EN` high; wait a fixed 60 ms for `+5V_BST`
    (below); `BST_EN` steady high (FW-8); wait for +32V by `VLED_SENSE` (FW-8); then `LED_EN`,
    the strobe pulses and the headroom loop (FW-22).
  - `LED_PWR_EN` is an output, default low (R57 holds it low until the firmware drives it).
  - The `+5V_BST` wait is fixed (the board has no `+5V_BST` sense): **60 ms** (user decision
    2026-11-09, with the X7R C35) until the first-board scope gives a measured value. C35 on the
    TPS22918's CT pin sets the slew rate SR = 0.55 µs/(V·pF) × C(C35) + 30 µs/V (TI TPS22918
    SLVSD76C §8.3.3 Eq. 3 p.14; Table 2 lists typical rise times, measured with an X7R CT), with
    C35 at its high tolerance and the X7R temperature coefficient (FH MLCC datasheet p.5). That is
    a 10–90 % figure, so the full swing takes ≈ SR × V(+5V_BST) / 0.8 plus the turn-on delay; the
    fixed wait must exceed it at the top of the `+5V` range. All typical values, none guaranteed.
- **FW-8** `BST_EN` (GPIO53, TPS61170 CTRL) is both the boost's enable and its output set point,
  in the TPS61170's PWM mode: V_FB = duty × 1.229 V (TI TPS61170 SLVS789D §8.4.2 p.11;
  [led.md](led.md#headroom-control)).
  - **Start:** drive it steadily high (duty 1). Within 1 ms of a start from shutdown, no low
    pulse ≥ 260 µs: that enters EasyScale mode, default feedback 200 mV (pp.10–11). Before raising
    `LED_EN`, confirm the boost is up by reading `VLED_SENSE` (FW-10): at duty 1
    V(+32V) = VREF × (1 + R65/R66) (TPS61170 SLVS789D p.5), so wait until V(+32V) reaches the
    value for the datasheet's minimum VREF, i.e. V_ADC ≥ VREF_min × (1 + R65/R66) × R72/(R71 + R72),
    less the ADC and divider errors. Before the
    boost runs, +32V only follows `+5V_BST` through the inductor and diode, a few volts, far
    below the LED string's forward voltage.
  - **Run:** a 50 kHz PWM, duty between D_min ([led.md](led.md#headroom-control)) and 1,
    set by the headroom loop (FW-22), with a step of one tick (0.13 % of the period: 800 ticks of
    a 40 MHz RMT clock, for example), which moves the output by
    Δduty × 1.229 V × (1 + R65/R66).
    Stay within 5–100 kHz: below 5 kHz the TPS61170 can mistake the PWM for EasyScale, above
    100 kHz its 40 ns on-time error grows (p.11). The low phases, at most (1 − D_min)/f, never
    reach the EasyScale or shutdown times. esp-hal 1.2.2 has an RMT driver for the P4 but no
    LEDC or MCPWM driver ([architecture](../architecture.md#esp-hal-support-and-gaps)): the plan
    is a third RMT TX channel in loop mode.
  - **Stop:** hold it low ≥ 2.5 ms (p.5).
- **FW-9** LED pulse ≤ 0.5 ms per frame at ≤ 120 fps (user decision 2026-10-31, exact answer:
  "≤ 0.5 ms at ≤ 120 fps (6 %)"). The STROBE hardware timeout (a window over all tolerances;
  the spread comes from U9's input threshold, [led.md](led.md#strobe-gating-and-timeout)) is only
  a backstop: it passes any pulse up to the low end of the window whole, and its upper end was
  accepted as a backstop only (user decision 2026-11-02; window widened by the X7R C33, user
  decision 2026-11-09). The firmware keeps the sensor's strobe width within 0.5 ms; this rule, not the
  timeout, is the limit. [led.md](led.md#strobe-gating-and-timeout)
  - **Pulse-width fault:** time every STROBE pulse from both edges (FW-16). If a lit pulse is
    longer than the exposure cap plus 50 µs, drive `LED_EN` low before the next frame and report;
    raise it again only once the exposure is back within 0.5 ms. The 50 µs assumes STROBE's width
    matches the set exposure within 50 µs, which [led.md](led.md#first-board-checks) check 5
    measures. At full boost output a run of pulses at the upper end of the timeout window would
    overheat Q1 beyond its 150 °C junction limit ([led.md](led.md#q1-protection)).
  - **Pending (first-board measurement):** a firmware LED limit on PoE power. The PoE power
    budget was accepted to be measured on the first boards (user decision 2026-11-02: "Accept,
    measure on first boards"); if the measured PD input exceeds the IEEE 802.3af 12.95 W PD
    limit or J1's 350 mA per pair, the firmware gets a lower LED duty or pulse limit on PoE.
    No number yet. [power.md](power.md#poe-input)
- **FW-10** ADC1 (P4 DS Table 2-7 pp.24–25) at ATTEN3 (0–3300 mV effective range, ±15 mV total
  error after calibration, P4 DS Table 5-6 p.90), which covers both inputs:
  - `VLED_SENSE` on channel 5 (GPIO21): V(+32V) = V_ADC × (R71 + R72)/R72; the +32V maximum
    (VREF maximum × (1 + R65/R66)) must stay inside the ATTEN3 range at the pin.
  - `LED_K_SENSE` on channel 7 (GPIO23): V(`LED_K`) = V_ADC × (R70 + R76)/R76; between pulses the
    pin sees the +32V maximum through the same divider, which must stay inside the ATTEN3 range.
    The ADC's ±15 mV error scales by the divider ratio at `LED_K`, which matters at the low end of
    the headroom band. The pin's time constant is (R70 ∥ R76) × C48, so a sample settles within
    the 0.1 ms sampling delay of FW-22.
- **FW-22** Headroom loop: while the LEDs pulse, hold V(`LED_K`) at the end of each pulse in
  0.6–0.9 V by the `BST_EN` duty ([led.md](led.md#headroom-control); user decision 2026-11-03,
  design option 1b).
  - **Sample** `LED_K_SENSE` ≥ 0.1 ms after a lit pulse starts and within its last 0.05 ms,
    timed from the STROBE edge capture (FW-16) and the set exposure, and once more between
    pulses in the same frame, after the bank has recovered its droop and before the next pulse
    (the reference for the sink-off fault; recovery time in [led.md](led.md#recovery-and-start-up)).
    Skip dark (LED-off) frames.
  - **Step** the duty once per average of a few samples (e.g. 8): below 0.6 V raise it one step,
    above 0.9 V lower it one step. Raise at least 4× as often as it lowers. Clamp to D_min … 1 ([led.md](led.md#headroom-control)).
  - **Start** at duty 1 on every power-up (FW-8). After the first average V of lit-pulse readings,
    make one coarse step, D = 1 − (V − V_T) / S_max (V_T = 1.2 V, the chosen coarse target; S_max
    the steepest +32V(D) on any unit, both in [led.md](led.md#headroom-control); clamped to D_min … 1), and
    hold D until two averages in a row differ by < 0.1 V (only the pulses pull +32V down); then
    fine steps. The coarse step reaches the band far sooner than fine steps alone
    ([led.md](led.md#headroom-control)). The start-up burst (the first average at full scale plus
    the bank's bleed-down during the hold after the coarse step) is the only time Q1 sees near
    full-scale power: take the first average from a few frames (e.g. 8) and step at once
    ([led.md](led.md#q1-dissipation) bounds the burst).
  - **Fall back:** `LED_EN` low and report when there is no valid reading for 1 s or the ADC
    fails. Never hold duty 1 with the LEDs pulsing: the loop is a requirement, because Q1's
    junction reaches its maximum at full scale (user decision 2026-11-09,
    [led.md](led.md#q1-dissipation)).
  - **Faults:** an in-pulse reading less than 1.5 V below the same frame's between-pulse reading
    means the sink isn't conducting or the sample missed the pulse. Conducting, the string's
    forward voltage rises from its µA value to its pulse value (12 LEDs × VF at the pulse
    current; Amicc Fig. 3 p.4, VF is given only at 1 A, so the low end at the lowest pulse
    current is an inference), and the late reference adds the recovered droop. With the sink off
    both readings are the same node at the same +32V, equal within the ADC error (±15 mV each,
    scaled by (R70 + R76)/R76; the resistor ratio cancels). The 1.5 V sits between the two
    bounds; it is a first value, and first-board check 7 in
    [led.md](led.md#first-board-checks) measures both differences on a low-VF and a high-VF unit
    and sets it. An absolute test can't work: the pulse and off levels of different units
    overlap (string forward voltage range in led.md).
    After a few in a row, `LED_EN` low and report. A reading below 1 V between pulses while +32V
    is up means the string is open: `LED_EN` low and report. Still below 0.6 V at duty 1: the
    string needs more than full scale, report. Still above 0.9 V at D_min: a shorted LED,
    report.
  - **Q1 power:** keep (V(`LED_K`) at the end + ½ × the worst droop over a pulse) × the maximum
    pulse current × LED duty ≤ P_limit (0.40 W, a chosen limit recalculated on 2026-11-09 for Q1's
    150 °C junction maximum, [led.md](led.md#q1-protection), which gives the droop and the
    current). It applies once the fine steps begin, after the post-step hold; not during the
    start-up burst, where V(`LED_K`) is still bleeding down and the rule would trip on typical
    units. From there, in the band, it must not trip on any unit in tolerance, reading error
    included. A loop stuck at full scale trips it on most units, well before Q1 overheats. Above P_limit, shorten the
    exposure until it holds; if it can't, `LED_EN` low and report.

### Ethernet and PTP

- **FW-11** `PHY_RST_N` = GPIO22, push-pull. R8 holds the IP101G in reset through boot. Release
  it (high) and wait ≥ 2.5 ms before installing the EMAC driver or any MDC/MDIO access: the
  PHY supplies the EMAC's only RMII clock (50M_CLKO, high impedance in reset), and the MAC
  needs that clock stable during any MAC/PHY access. The PHY driver must not reset the PHY
  again once the MAC is up (in ESP-IDF terms `phy_config.reset_gpio_num = -1`). A later reset
  pulse is low ≥ 10 ms with the 25 MHz crystal running, then ≥ 2.5 ms before use. No internal
  pull on GPIO28 (CRS_DV / FX_HEN strap) while RESET_N rises (IP101G datasheet p.16, p.17,
  p.18, p.54; ESP-IDF ESP32-P4 Ethernet guide, read 2026-11-01).
- **FW-12** EMAC RMII on IO MUX function 3: CRS_DV GPIO28, RXD0 GPIO29, RXD1 GPIO30, TXD0
  GPIO34, TXD1 GPIO35, TX_EN GPIO49. The RMII clock is an input on GPIO50
  (`EMAC_CLK_EXT_IN`) from the PHY's 50 MHz output; never select GPIO32/GPIO44 as the clock
  or GPIO33 as TX_EN. MDC on GPIO31 and MDIO on GPIO52 through the GPIO matrix; the PHY is at
  MDIO address 1 (P4 DS Table 2-3 pp.21–22; TRM p.1002). MDC ≤ 3.33 MHz: period ≥ 300 ns,
  high and low ≥ 150 ns (IP101G datasheet Table 20 p.62).
- **FW-13** Never disable the IP101G's auto-MDI/MDIX: never set page 16 register 16 bit 11
  (AUTO_MDIX_DIS). The board's MDI pairs are swapped at the jack, which makes the straight (MDI)
  wiring the PHY's MDI-X channel. If auto-MDIX ever has to be off, set page 16 register 30 bit 3
  (FORCE_MDIX) = 1 (IP101G datasheet p.25, p.29, §5.1.6 p.46). Both pairs are also P/N-inverted
  between U1 and J1 on purpose, so both route on F.Cu without vias (user decision 2026-11-04):
  the read-only POLARITY status bit, page 16 register 18 bit 8 (p.27), may read reversed (1) on
  the receive pair, at least at 10 Mbit/s. That is the board's wiring, not a fault: never raise an
  error, retry or reset the PHY on it. [ethernet.md](ethernet.md#magjack-and-mdi)
- **FW-14** PPS: route `emac_ptp_pps_pad_out` to GPIO32 (TRM p.1007). GPIO33 (`PPS_IN`, tied to
  GPIO32 by R34) is an input only and is captured with a GPIO ETM event channel (TRM
  p.1017); as an output it would fight GPIO32 through R34.

### Camera

- **FW-15** `XVS` = GPIO7 on an RMT TX channel (frame-trigger edge schedule). The camera sees
  V(GPIO7) × R44/(R40 + R44) through the divider; the camera module's I/O level is not known yet
  ([camera.md](camera.md#xvs-level-options)). Keep `XVS` low, RMT idle level low, whenever
  `CAM_EN` is in its off state and until `CAM_3V3` is up: the OV9281 allows its I/O at most
  1 V above its I/O supply (OV9281 datasheet v1.1 Table 8-1), and nothing on the board gates
  `XVS` by `CAM_EN`. Until the `CAM_EN` polarity is measured (FW-17), treat both `CAM_EN`
  levels as possibly off and start triggers only after an SCCB ACK.
- **FW-16** `STROBE` = GPIO12: input, captured on both edges with one GPIO ETM event channel,
  whose rise-edge and fall-edge events each trigger a capture (`GPIO_EVT_CHn_RISE_EDGE` /
  `_FALL_EDGE` in SOC_ETM, esp32p4 PAC 0.3.0): the pulse width for FW-9, the sample timing for
  FW-22. Never configure it as an output (U7 drives it).
- **FW-17** SCCB on GPIO8 (SDA) / GPIO9 (SCL), ≤ 400 kHz, with the bus recovery of FW-20 at
  init. `CAM_EN` = GPIO10: R43 holds it low through reset; whether low means sensor on or off
  is not known and is measured on the first module (supply current with `CAM_EN` low and
  high). Record the measured polarity here.
- **FW-18** The ESP32-P4-Core powers its MIPI PHY from `ESP_LDO_VO3`, which is 0 V by default:
  the CSI driver sets it to 2.5 V before starting the CSI host
  ([architecture](../architecture.md)).

### Accelerometer

- **FW-19** MSA3S02 on GPIO3 (SDA) / GPIO4 (SCL), 7-bit address 0x62, ≤ 400 kHz, internal pulls
  off (R73/R74 on the board). `IMU_INT1` = GPIO51: push-pull, active low,
  non-latched after reset (registers 0x20 = 0x00, 0x21 = 0x00; MSA3S02 datasheet V1.0 p.24).
  Register 0x01 (PARTID) reads 0x13 (p.19). It replaced the LSM6DS3: no gyroscope, a
  different address and register map. [imu.md](imu.md)
- **FW-20** Bus recovery at init, on both I²C buses: clock SCL 9 times with SDA released, send
  a STOP, then soft-reset the MSA3S02 (register 0x00, MSA3S02 p.20). Both SCL lines are P4
  outputs. The accelerometer has no reset pin and no power switch. The camera's PWDN
  (`CAM_EN`, GPIO10) may serve as a camera reset once its polarity is measured (FW-17);
  until then the SCCB bus clear is the only recovery.

### Status LED

- **FW-21** `STATUS_LED` = GPIO13 on an RMT TX channel, driving D1 (Yongyu
  YY-0807BRGK26) through R30. One LED, no chain (DOUT open). From its datasheet A.0:
  - Bit times (p.6): T0H 0.3 µs, T1H 0.6 µs, T0L 0.6 µs, T1L 0.3 µs, each ± 0.05 µs; reset
    (latch) low ≥ 80 µs.
  - The datasheet contradicts itself on speed: its switching table gives 800 kHz typical,
    1100 kHz maximum (p.5), while the nominal bit times above make 0.90 µs per bit
    (1.11 MHz). Use T0H 0.325 / T0L 0.625 µs and T1H 0.625 / T1L 0.325 µs: 0.95 µs per bit
    (1.053 MHz ≤ 1100 kHz), each time 25 ns inside its ± 0.05 µs window. The RMT resolution
    must represent these exactly (25 ns ticks, e.g. a 40 MHz RMT clock). Scope DIN on the
    first board and check all three colours latch correctly.
  - 24 bits per LED, MSB first, in the order **G, R, B** (p.7: "按照 GRB 的顺序发送数据"),
    although the part name and page headers say "BRG". Confirm the order on the first board
    (send pure red, then pure green, then pure blue) and correct this rule if it differs.
  - The LED is off after power-up (p.2) and each channel sinks a fixed 5 mA (p.4); colour
    and brightness are set by the PWM values only.

## GPIO changes in this revision

Moved against the schematic this revision started from, all to make routing possible on the
4-layer board with U6 on the bottom. Every new pin was checked against the P4 datasheet v0.7
(Table 2-1 pp.14–16 I/O domain and reset state, Table 2-7 pp.24–25 analog functions,
§2.3.4 p.26 and §2.3.5 p.27 JTAG GPIO2–5, §3 p.36 straps GPIO34–38, p.28 note 1 USB
GPIO24–27): none of them is a strap, JTAG or USB pin.

| Signal | Was | Now | Reason |
|---|---|---|---|
| `PHY_RST_N` | GPIO51 (U6.69) | GPIO22 (U6.18) | From GPIO51 its only route crossed U6's top pin row in the F.Cu gap the LED sink gate (`SINK_G`) needs. U6.18 is reached by a via from F.Cu inside the LED ring, where `PHY_RST_N` runs. GPIO22 has no pull at reset, so R8 still holds the PHY in reset through boot |
| `VLED_SENSE` | GPIO22 (U6.18), ADC1 ch 6 | GPIO21 (U6.17), ADC1 ch 5 | Swapped with `PHY_RST_N`: R71/R72 sit outside the LED ring and reach U6.17 from the west. Still ADC1 |
| `XVS` | GPIO11 (U6.13) | GPIO7 (U6.9) | Swapped with `LED_EN`. U6's west column can only leave west in a narrow column or, from pins 13–16, east into the pocket under U6. `XVS` takes the lower pin and leaves west to its long route to the camera |
| `LED_EN` | GPIO7 (U6.9) | GPIO11 (U6.13) | Its parts (U9, R56, TP11) sit in an F.Cu island reachable only through the pocket under U6, which U6.13 reaches. Both pins are plain VDD_LP GPIOs; RMT works through the GPIO matrix on either |
| `IMU_INT1` | GPIO54 (U6.72) | GPIO51 (U6.69) | U6.72 could not reach U12 pin 5 (the `BST_EN` and +32V copper close it off). GPIO51 was freed by the `PHY_RST_N` move: same I/O domain (VDD_IO_6), same reset state, fine for a digital interrupt |
| `LED_PWR_EN` | GPIO6 (U6.8) | GPIO20 (U6.16) | U6's west column has fewer lanes than nets that needed it; R57 and U8 sit in an F.Cu pocket only U6.13/16/17 reach. GPIO20 has no reset pull, so R57 still holds the LED supply off at boot |
| `VBUS_SENSE` | GPIO20 (U6.16), ADC1 ch 4 | GPIO6 (U6.8), digital only | Swapped with `LED_PWR_EN`. GPIO6 has no ADC, which is fine: the firmware only needs the level (FW-4). The divider output meets the GPIO6 input levels ([power.md](power.md#usb-vbus)) |

Not GPIO moves, but firmware-visible part changes in the same revision: the accelerometer is
now the MSA3S02 (FW-19), the status LED the YY-0807BRGK26 (FW-21), GPIO23 reads the LED
sink's drain (`LED_K_SENSE`) instead of an NTC, and `BST_EN` carries a PWM (FW-8, FW-22).
