# IMU

A 6-axis motion sensor (U12, ST LSM6DS3TR-C: accelerometer + gyroscope) on its own I2C bus to the P4. It does two jobs:

- **Bump detection:** its wake-on-motion interrupt tells firmware the camera was knocked, so the host knows the rig calibration is stale.
- **Gravity vector:** the accelerometer gives camera tilt and the floor plane for rig calibration.

The gyroscope is spare. Chosen because it was the cheapest in-stock part with a stock KiCad symbol and footprint and no config upload at boot.

## Wiring decisions

- **Own bus, not the camera's SCCB:** the IMU stays reachable while the camera is off or unplugged (the module's load on SCCB when unpowered is unknown), and IMU reads never wait behind camera register writes.
- **Address 0x6A:** SA0 is tied to GND. It has no internal pull, so it must be tied. (DS §6.3.1)
- **SDx, SCx tied to GND:** the sensor hub is unused; the datasheet says tie them to Vdd_IO or GND. (DS Table 2)
- **CS tied high:** selects I2C instead of SPI. (DS §6.1)
- **INT1 to GPIO54, push-pull, no pull resistor:** U12 drives INT1 low from power-up and GPIO54 has no reset pull, so the line is defined before firmware runs. This holds only while INT1 stays push-pull: see [firmware rule 11](firmware-rules.md). (DS Table 18)
- **INT2 not connected:** it is driven low by default. (DS Table 18)
- Bus timing, power-up delay and bus recovery: [firmware rules 11 and 12](firmware-rules.md).

## Values

- **Supply:** Vdd and Vdd_IO both on +3V3 (3.20–3.44 V worst case). Limits are Vdd 1.71–3.6 V and Vdd_IO 1.62 V to Vdd + 0.1 V, so both hold. Draw ≤ 0.9 mA. (DS Table 4)
- **Decoupling:** 100 nF on each supply pin (C50, C51), as close as possible. (DS §7.1, Fig. 15)
- **Pull-ups:** 10.0 kΩ on SCL (R64) and SDA (R65), as in the datasheet's example. Sink current 0.33 mA, far inside the 4 mA the pin is rated for at V_OL ≤ 0.2 V. (DS Fig. 15, Table 4)
- **Rise time:** about 15 pF of bus (estimate) gives 0.8473 × 10 kΩ × 15 pF ≈ 127 ns. Even 30 pF gives 254 ns, under the 300 ns Fast-mode limit, so 400 kHz works. (UM10204)
- **Gravity accuracy:** zero-g offset ±40 mg typical (about 2.3° of tilt), plus ±0.5 mg/°C drift. A per-board 6-orientation calibration removes most of it if the rig needs better. (DS Table 3)

## Part gotchas

- **Two ERC exclusions:** the stock `LSM6DS3` symbol types SDO/SA0 and SDx as bidirectional (their SPI and sensor-hub roles), so tying them to GND gives two `pin_to_pin` warnings. Both are excluded, with a comment, in `espion-cam.kicad_pro`.
- The stock `LSM6DS3` symbol has the same pinout as the TR-C. Pin 7 (GND) and pins 10, 11 (NC, leave soldered) are hidden pins.

Layout: see [layout.md](layout.md).

## Sources

- DS: ST LSM6DS3TR-C datasheet, DocID030071 Rev 3 (May 2017).
- UM10204: NXP I2C-bus specification and user manual.
