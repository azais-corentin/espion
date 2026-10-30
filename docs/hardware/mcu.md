# MCU: P4 module, reset, USB-C, status LED

The MCU sheet (`mcu.kicad_sch`) holds the ESP32-P4 module and everything local to it: reset and BOOT buttons, the USB-C bench port, VBUS sense, the `PWR_SRC` pull-up, the PPS output and the RGB status LED. Layout: see [layout.md](layout.md). GPIO map and strap table: see [firmware-rules.md](firmware-rules.md).

## Module

- U1 is a Waveshare ESP32-P4-Core with 16 MB flash (SKU 36096). Its role in the system: [../architecture.md](../architecture.md). Order it from Waveshare; it is the only part on this sheet not from LCSC (the symbol has Manufacturer and MPN, no LCSC number).
- Inside the module: ESP32-P4NRW32X, silicon rev v3.x (esp-hal does not support older revisions), 32 MB in-package PSRAM, 16 MB NOR flash, 40 MHz and 32.768 kHz crystals, and the VDD_HP DC-DC. (WS-PIN, WS-SCH)
- Pins: GPIO0/1 carry the module's 32.768 kHz crystal; GPIO14–19 are not brought out; GPIO24/25 are the USB full-speed pair (USB Serial/JTAG). The other exposed GPIOs sit on ESP_3V3. (WS-PIN, WS-SCH)
- **GPIO39–48 are unused on purpose.** They sit in the VDD_IO_5 domain, fed by ESP_LDO_VO4, which is 0 V until firmware sets it. Keep board signals off them unless firmware sets VO4 to 3.3 V. (HDG-SC, WS-SCH)
- ESP_LDO_VO1–VO4 (pins 51–54) are decoupled on the module and left unloaded here. VO1 feeds the flash, VO2 the PSRAM, VO3 the MIPI PHY (firmware rule 2), VO4 the GPIO39–48 domain.
- VBAT (pin 74) ties to +3V3: it must not float, there is no RTC battery, and its 10 µF + 100 nF are on the module.
- Supply: C26 4.7 µF + C27 100 nF at pins 75/76. The module already carries ≈ 30 µF on ESP_3V3, so the pins see ≈ 35 µF, and the smaller entrance cap keeps the buck's total output capacitance down ([power.md](power.md)).
- Unused P4 functions: DSI pins may float (HDG-SC MIPI). The USB HS OTG pins are NC; the 1 MΩ D+ pull-down HDG asks for applies to chip rev v1.x only, and this module carries rev v3.x. GPIO37/38 stay NC because GPIO37 carries the UART0 ROM log.
- Logic levels: P4 V_IH = 0.75 × VDD = 2.48 V, so every P4 input (STROBE, RMII RX, PPS_IN, PWR_SRC) must be 3.3 V push-pull or a pulled-up open drain. A 1.8 V signal would not read high. (DS Table 5-4)
- **Provisional land pattern.** Waveshare publishes the outline and pad pitch, not the pad size, so the pads (1.0 mm under the body, like KiCad's castellated Espressif modules) are a guess. Measure a real module and confirm both footprints before ordering boards (`hardware/lib/README.md`).
- **U1 is hand-soldered after the rest of the board**, on `Module_Waveshare:Waveshare_ESP32-P4-Core_HandSolder`: 0.6 × 2.5 mm pads reaching 1.5 mm past the edge for the iron tip, and no paste, so the stencil has no apertures for it. For reflow, switch U1 to `Module_Waveshare:Waveshare_ESP32-P4-Core` (0.6 × 1.5 mm pads, with paste).

## Reset (ESP_EN)

- R1 10 kΩ to +3V3 and C24 1 µF to GND on CHIP_PU, plus SW1 (RESET) to GND. The module has no RC on CHIP_PU, and the pin must not float. These are the values HDG recommends. (WS-SCH, HDG-SC)
- τ = 10 ms (8.9–11.1 ms with C24 ±10 % and R1 ±1 %). C24 is a 50 V X7R 0805, so it keeps nearly all its capacitance at 3.3 V (estimate).
- +3V3 does not step up: it follows the TPS2121 soft start, ≈ 4.2 ms to 3.3 V (estimate), so C24 is already at ≈ 0.6 V when the rail settles. The chip leaves reset at 0.75 × VDD, **10.3–13.3 ms (11.8 ms nominal) after +3V3 settles**: far above t_STBL ≥ 50 µs. (DS Table 5-4, Table 2-14)
- A button press lasts far longer than t_RST ≥ 1 ms.
- No supervisor. If the slow ramp ever causes failed boots, HDG suggests a supervisor at about 3.0 V instead of the RC.

## BOOT on GPIO35, shared with RMII TXD1

- GPIO35 is both the boot-mode strap and RMII TXD1, as on Espressif's own Ethernet board. The IP101G's TXD1 is a Hi-Z input with no pull, so it never fights the strap. (IP101G DS §3.2)
- R73 (TXD0), R74 (TXD1) and R75 (TX_EN), 22 Ω, sit at the P4 pins as source termination of the P4-driven RMII lines; the value and its reasoning are in [ethernet.md](ethernet.md). R2 and R3 connect on the P4 side of R74 (`RMII_TXD1_MCU`), so R74 is not part of the strap divider and every strap figure below holds unchanged.
- R2 10 kΩ pull-up holds GPIO35 high (SPI Boot) for the ≥ 3 ms hold time after CHIP_PU rises, on top of the internal 45 kΩ pull-up; HDG advises reserving one. No capacitor on GPIO35 or `BOOT_SW`. (DS Table 3-2, HDG-SC Strapping)
- SW2 pulls GPIO35 down through R3 1 kΩ. Pressed during reset: 3.3 V × 1 k / (1 k + 8.18 k) = 0.36 V, below V_IL 0.825 V, so the chip enters download mode (8.18 kΩ = R2 ∥ the 45 kΩ pull-up).
- Pressed while Ethernet runs: R3 limits the load on TXD1 to ≈ 3.6 mA. The pad drops about 0.1–0.2 V (estimate), so TXD1 stays ≈ 3.1 V, above the PHY's V_IH 2.64 V; R74 carries only the PHY's µA input current, so the PHY sees the same level. Frames stay intact and nothing shorts. (IP101G DS Table 12)
- Flashing normally needs neither button: USB Serial/JTAG forces download mode (espflash does this). BOOT + RESET is the fallback.
- Other straps: GPIO34 (RMII TXD0) floats and is ignored with default eFuses (firmware rule 8); GPIO36 has a 10 kΩ pull-up on the module. Board outputs float during ROM and boot, so every output that reaches an active part has a pull or divider on its own sheet. The RMII TX lines and MDC are the exception: R10 holds the PHY in reset (firmware rule 9).

## USB-C bench port

- J1 is a USB 2.0 Type-C receptacle used as a sink only: R21/R22 5.1 kΩ Rd on CC1/CC2. Both D+/D− rows are tied, so the plug works either way up; SBU is NC.
- R23/R24 22 Ω in series on D+/D−, close to the P4, as HDG recommends (22–33 Ω) for the full-speed pads of USB Serial/JTAG.
- U2 USBLC6-2SC6 clamps D+/D−. **Its VBUS pin 5 goes to +3V3 (with C25 100 nF), not to the connector's VBUS.** The P4 turns on its D+ pull-up after every reset, even on PoE alone; with pin 5 on VBUS that pull-up would leak through U2's steering diode and hold an unplugged VBUS at ≈ 2.7 V (estimate). A Type-C source only turns VBUS on after it sees vSafe0V, so plugging in USB while on PoE could fail. ST's own Ethernet example ties pin 5 to a local supply, and +3V3 is well inside U2's ratings (V_BR ≥ 6 V). (USBLC6 §2.1–2.3, Fig. 15, Table 2)
- The cost: U2 no longer clamps VBUS. D2 (TSD05DYFR TVS) does, at J1. It leaks ≤ 50 nA up to 5.5 V, breaks down at ≥ 6.0 V and clamps at ≤ 15 V at 60 A (8/20 µs), below the TPS2121's 24 V abs max on IN2. Rated ±30 kV IEC 61000-4-2; not rated for 10/1000 µs surges, which a bench port doesn't see. (TSD05 §5.1, §5.3, §5.7; SLVSEA3F §7.1)
- D2 snaps back to ≈ 6.6 V (typical curve, no minimum given), about 1 V above the rail, so it should not latch on VBUS (estimate).
- D2 symbol gotcha: stock KiCad has no unidirectional TVS symbol, so D2 uses `Device:D_Zener` with K = pin 1 = the banded pin, on VBUS. The SOD-323 hand-soldering footprint encloses TI's land pattern. (TSD05 p.3, p.19)
- R25 100 kΩ bleeds an unplugged VBUS to vSafe0V (50 µA at 5 V); the sense divider bleeds in parallel. VBUS has no capacitor on this sheet: C15 4.7 µF sits at the TPS2121 (≤ 10 µF in total on VBUS), and so do the VBUS PWR_FLAGs.
- The USB shell goes straight to GND. USB's GND wire bonds board GND to the host anyway, so an RC would isolate nothing, and a direct bond is the shortest ESD return. PoE isolation is the DP1435's 1.5 kV barrier and the RJ45 shield RC, which the USB shell doesn't touch.

## VBUS sense and PWR_SRC

- USB 2.0 §7.1.5.1 forbids a D+ pull-up while VBUS is absent. The board can run on PoE with a cable to an unpowered host, so firmware reads VBUS and turns the pull-up off (firmware rule 10). `PWR_SRC` can't do it: it reads PoE whether or not USB is plugged in.
- R62 47 kΩ (top) and R63 75 kΩ (bottom) divide VBUS into GPIO20 (ADC1_CH4, no pull at or after reset), with C49 100 nF to GND.
- Window: the pin must read high at VBUS 4.4 V (0.75 × 3.44 V = 2.580 V) and stay ≤ 3.20 V + 0.3 V = 3.500 V at 5.5 V, so the ratio must be 0.586–0.636. 75/122 = 0.615. It is the in-stock pair closest to the 0.611 window centre. Worst case with 1 % parts: 2.684 V (**104 mV margin**) and 3.407 V (**93 mV margin**); 50 nA of pin leakage moves it ≤ 2 mV. (DS Table 5-4; USB 2.0 Table 7-7; Type-C vSafe5V; +3V3 range from [power.md](power.md))
- At ATTEN3 (0–3.3 V) the ADC reads VBUS up to ≈ 5.3 V: enough for present/absent and a rough reading. (DS Table 5-6)
- C49 is the 100 nF Espressif asks for on every ADC input. τ = 28.9 kΩ × 100 nF ≈ 2.9 ms: it filters ESD and hot-plug ringing, and a plug-in still shows within ≈ 15 ms. (HDG §1.3.10)
- Load: 45 µA at 5.5 V. With the P4 unpowered and VBUS present, R62 limits the current into the GPIO20 clamp diode to ≈ 0.1 mA, too little to back-power +3V3 (estimate).
- R26 10 kΩ to +3V3 is the only pull-up on the TPS2121's open-drain ST output (6–20 kΩ to ≤ 5.5 V asked); it sinks 0.33 mA when low. High = PoE powers +5V, low = USB does. GPIO2's internal pull-up only adds to R26. (SLVSEA3F p.6, Table 9-3)

## PPS loop-back

- GPIO32 outputs the EMAC's PPS to TP1 (with TP9 GND close by for a probe), and R27 0 Ω loops it back into GPIO33 so an ETM event can capture it. The two pins are adjacent, so the loop is short.
- Open question: does the GPIO ETM event fire on a pad the EMAC drives as an output? The TRM takes the event from the pad's input path, so it probably does with the input enabled (estimate). (TRM §10.17)
- At bring-up, test both: R27 fitted, capture on GPIO33; R27 removed, capture on GPIO32 itself. If capture on GPIO32 works, R27 is not needed. Firmware keeps GPIO33 an input.

## RGB status LED

- D1 shows the board's status (power source, link, PTP lock, errors). It is a WS2812B-2020-V6 on +3V3, driven by an RMT channel on GPIO13 through R28 220 Ω. It won because it is the only in-stock addressable LED Worldsemi specifies for 3.3 V: DIN V_IH = 0.55 × 3.3 = 1.82 V against the P4's V_OH ≥ 2.64 V (0.82 V margin), so no level shifter, and +3V3 exists on PoE and on USB. Second source: WS2812C-2020-V6 (LCSC C55109522), a drop-in at 5 mA per colour instead of 12 mA (≈ 2.5× dimmer, still plenty).
- Supply: 3.3–5.3 V is a maximum rating, so never put it on `+5V_POE` (5.15 V nominal: inside the limit, with no margin). At the bottom of +3V3 (3.20 V) it runs 0.10 V under its rated minimum: **accepted** for a status light; at worst blue and green dim. Sharing the P4's rail means DIN can never back-power it.
- Low level: V_IL ≤ 0.7 V against P4 V_OL ≤ 0.33 V (0.37 V margin). The LED datasheet gives thresholds at VDD = 5 V only. (V6 DS p.3)
- C28 100 nF at VDD (V6 DS p.6). DOUT is NC: one pixel, no chain.
- R28 220 Ω at the GPIO damps edges and EMI: τ ≈ 4.4 ns, ≈ 2 % of the shortest 220 ns pulse. It also limits a shorted DIN to ≈ 13 mA (estimate).
- R29 10 kΩ pulls DIN low. GPIO6–13 have no pull at reset, so a floating DIN could latch noise; held low, the LED can't clock in data. Keep the LED on GPIO6–13 if placement swaps pins. DIN high with R29 is still ≥ 2.58 V.
- Current at full white: ≈ 37 mA, ≈ 0.12 W from +3V3 (estimate; the IC's own current isn't given). The WS2812C variant draws 15 mA. Firmware should cap brightness (e.g. ≤ 25 %).
- RMT encoding: 50 ns tick (80 MHz ÷ 4); 0 = 350/950 ns, 1 = 650/650 ns, so every timing keeps ≥ 30 ns margin. A 100 ns tick has no valid encoding. Idle low; ≥ 280 µs low between frames. If the XVS channel moves the RMT clock off 80 MHz, re-pick the LED channel divider. Channel split: firmware rule 7. (V6 DS p.5; TRM §60.3)
- Footprint: `LED_SMD_Worldsemi:LED_WS2812B-2020_PLCC4_2.0x2.0mm`, the stock `LED_WS2812B-2020` copied unchanged to carry a project 3D model (KiCad ships none). Its pads match the V6 drawing; its pad numbering is the datasheet top view rotated 180°. The symbol's Datasheet field points to the V6 PDF, not the stock V1.3 link.
- No TVS: no cable reaches it. If the enclosure has an opening over it, use a closed light pipe (estimate). MSL 5a (24 h floor life): bake 70–75 °C ≥ 24 h before reflow if the bag was opened. (V6 DS p.8)

## Deviations from Espressif's hardware design guidelines

- **Entrance cap 4.7 µF, not 10 µF:** the module already carries ≈ 30 µF on ESP_3V3 (see Module).
- **No supervisor on CHIP_PU:** the HDG RC is used; the supervisor is HDG's fallback for slow ramps (see Reset).
- Followed as written: reset RC values, BOOT pull-up and no strap capacitor, 22 Ω USB series resistors, 100 nF on ADC inputs.

## Not yet verified

- Module land pattern: measure a real module before ordering.
- PPS ETM capture on the output pad: decide R27 at bring-up.
- Boot reliability on the slow +3V3 ramp: if boots fail, add the supervisor.
- Status LED colour at +3V3 = 3.20 V.

## Sources

- WS-PIN: Waveshare ESP32-P4-Core pin description, docs.waveshare.com/ESP32-P4-Core
- WS-SCH: Waveshare ESP32-P4-Core schematic (16 MB version), page 1
- DS: Espressif ESP32-P4 datasheet v0.7
- TRM: Espressif ESP32-P4 Technical Reference Manual (Chip Boot Control §12, ETM §10.17, RMT §60)
- HDG: Espressif ESP32-P4 hardware design guidelines, schematic checklist (HDG-SC), "latest" as read 2026-10-28
- USBLC6: ST USBLC6-2 datasheet, Doc ID 11265 Rev 5 (October 2011)
- TSD05: TI TSD05 datasheet, SLVSH43C (May 2025)
- SLVSEA3F: TI TPS2120/TPS2121 datasheet
- IP101G DS: IC Plus IP101G datasheet
- V6 DS: Worldsemi WS2812B-2020-V6 datasheet (LCSC C52917434)
- USB 2.0 specification; USB Type-C specification
