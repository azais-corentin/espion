# Camera

The OV9281 global-shutter module sits on the back of the PCB, its lens through the cutout. Two connectors join it to the board:
- J3, a 15-pin 1.0 mm FPC with the Raspberry Pi camera pinout: 2-lane MIPI CSI-2, SCCB, the module enable and 3.3 V.
- J4, a JST-SH 4-pin header for XVS (frame trigger in) and STROBE (exposure out), at the module's I/O level, which is not known (1.8, 2.8 or 3.3 V).

Layout: see [layout.md](layout.md). Firmware: [firmware rules](firmware-rules.md) 1 (XVS vs `CAM_EN`), 2 (MIPI PHY supply, before starting the CSI host) and 12 (SCCB bus recovery).

## Module and cables

- SZYTF OV9281 module from [AliExpress 1005012566040263](https://fr.aliexpress.com/item/1005012566040263.html), not LCSC; lens 79°, 110° or 160°. Confirmed by the user: it runs on 3.3 V from camera pin 15, its SCCB is 3.3 V, and camera pin 11 is its enable.
- Its back has 2.54 mm pads for XVS, STROBE, IL_PWM, GPIO2, 3V3 and GND. Only XVS, STROBE and GND are used. The listing gives no level for XVS and STROBE.
- It ships with a 15→15 FFC (used here) and a 15→22 FPC. Cable type and pin-1 edge: see [FPC connector](#fpc-connector).
- Pigtail: DEALON LNN100-157128-200-4P (C2999003), SH 1.0 mm, 200 mm, tinned ends. Solder them to the module's XVS, STROBE and GND pads; J4 order is XVS, GND, STROBE, GND (the fourth wire is a second GND return; cut it back if unused). It is an SH clone: check it mates with J4 on the first order.
- Not in the KiCad BOM (preset `Order`), so order separately: the module, the FFC and the pigtail (no schematic symbol). R36 is DNP and left out too.
- Never plug the FFC with the board powered.

## FPC connector

- **Single contact, on purpose.** The earlier XFCN F1004-H-15-20G-R was dual contact: a face-flipped FFC mirrored the pins (n → 16 − n) and put the board's +3V3 on the module's GND. With a single-contact part, a flipped cable makes no contact.
- **Pad n = camera pin 16 − n** (pad 1 = pin 15, 3V3; pad 15 = pin 1, GND). J3 sits on B.Cu facing the module. Pi rule: "lens facing down and the connector facing to your right, pin 1 is the pin closest to you", so our pin 1 must be on the right with the slot facing down; JUSHUO's drawing and the stock footprint put circuit 1 on the left (x = −7 mm). The symbol's pin numbers equal the footprint's pad numbers. Pinout from the Pi 4 B (J3), Pi 3 B+ (J3) and Camera V2.1 (J1) schematics, which agree.
- **Check the pin-1 edge before routing J3:** ohm the module's GND pad to the cable's edge conductors; that edge must meet pad 15. If it is the other edge, the footprint stays but the net-to-pad map changes, and with it the routing of the CSI pairs, SCCB, `CAM_EN` and `CAM_3V3`.
- **Contact side.** The Pi camera FFC has its contacts on opposite faces at the two ends ("type B"; RP-008146-DS-1: 0.309 ± 0.05 mm thick, 16.0 mm wide). The OV9281's cable is undocumented; a kit that works on a Pi must ship type B (estimate). Both connectors face the back of the board, so a type B cable needs the opposite contact type to the module's. Camera Module 2 uses a bottom-contact Amphenol SFW15R-1STE1LF, hence the top-contact part as the default:

| Module connector | Cable | J3 |
|---|---|---|
| Bottom contact (Camera Module 2 style) | B (Pi standard) | **Top: AFA07-S15ECA-00 (fitted, C262742)** |
| Bottom contact | A | Bottom: AFA07-S15FCA-00 (C262721) |
| Top contact | B | Bottom: AFA07-S15FCA-00 |
| Top contact | A | Top: AFA07-S15ECA-00 |

- **Cable type: not yet checked.** Record it here, and buy spares of the same type: the other type only connects when twisted, and is then mirrored. The contact side only changes J3's MPN and can wait until parts are ordered.
- JUSHUO's E (top) and F (bottom) drawings (rev A) share outline, land pattern and circuit-1 position, so one footprint fits both: `Connector_FFC-FPC_JUSHUO:JUSHUO_AFA07-S15ECA-00_…`, KiCad's stock FCA footprint copied unchanged to carry a project 3D model of the E body (KiCad ships none). JUSHUO wants 0.30 ± 0.03 mm FFC; the Pi cable's nominal 0.309 mm fits.
- Every top-contact part on LCSC is a slide lock. Second source: Yuandi FPC-1.0LS-15PH25 (top) / FPC-1.0LX-15PH25 (bottom), but it needs a project footprint and has pin 1 at the other end (pad n = camera pin n). TE 1-84953-5 and Amphenol SFW15R-2STE1LF had too little stock.
- The mounting pads (`MP`) go to GND so ERC sees no floating pin. The XFCN footprint library in `hardware/lib` is no longer used.

## Camera 3.3 V

- +3V3 → R37 (0 Ω 0805, ≥ 2 A) → `CAM_3V3` → J3 pad 1. Always on, as on the Pi; `CAM_EN` switches the module's own regulators. Lift R37 to measure the module's current, or to feed the module from a current-limited bench supply on TP7.
- Current: ≤ 250 mA (estimate, Pi-module class); the OV9281 datasheet gives IDD as "TBD". The product brief's 156 mW active is ≤ 130 mA even through linear regulators at 1.2 V, so the estimate is conservative.
- C30 10 µF supplies the module's regulator start when `CAM_EN` rises, so +3V3 does not dip; C31 100 nF covers HF. The module's own ≈ 7.7 µF (Camera V2.1) charges with the rail at buck soft-start.

## SCCB pull-ups

- R33, R34: 1.8 kΩ to +3V3. The module's SCCB is 3.3 V and nothing else pulls it up (the mcu sheet adds none). The Pi uses the same value (CM4 DS: "Internal 1.8 kΩ pull up" on SCL0/SDA0).
- Sink current 3.44 V / 1.8 kΩ = 1.91 mA, under the I2C 3 mA at 0.4 V (R_min 1.01 kΩ; UM10204). Rise ≈ 61 ns at ≈ 40 pF (estimate), far under 300 ns Fast-mode.
- SDA/SCL go through the P4's GPIO matrix (GPIO8/9), so a swapped module needs no part change.

## CAM_EN pull-down

- R35 47 kΩ holds the module off while GPIO10 floats. The module side is regulator EN pins with no pull (Camera V2.1: 2 × AP7331, 1 × PAM2301).
- AP7331 leaks ±1 µA per EN with V_IL ≤ 0.4 V: 3 µA × 47 kΩ = 0.14 V. 100 kΩ would give 0.3 V, too close. With the pin high R35 draws 70 µA. The SZYTF module's enable current is not published.
- `CAM_EN` and XVS need pins with no reset pull-up: a 45 kΩ pull-up against 47 kΩ gives ≈ 1.6 V and turns the module on during reset. GPIO2 has one; GPIO10 (`CAM_EN`) and GPIO11 (XVS) do not (P4 DS Tables 2-1, 5-4).
- R36 (0 Ω, DNP) links camera pin 12 (`CAM_IO1`) to `CAM_EN`, for a module that wants its enable there. It keeps pad 4 on a net, so there is no no-connect flag.

## XVS level option

XVS works at the module's I/O voltage (1.8, 2.8 or 3.3 V, undocumented). The P4 drives it at 3.3 V from GPIO11 (RMT) through the divider R31/R32, an assembly option. **The default (1.8 V row) is safe at every level:** `XVS_CAM` never exceeds 1.89 V, under a 1.8 V module's abs max (VDD-IO + 1 V = 2.8 V) and under any higher I/O rail. On a 2.8 or 3.3 V module it may just not trigger. Worst cases use +3V3 = 3.20–3.44 V ([power.md](power.md)).

| Module I/O | R31 | R32 | `XVS_CAM` high, worst case | Condition met |
|---|---|---|---|---|
| **1.8 V (default)** | 1.5 kΩ 1 %, 0402WGF1501TCE (C25867) | 1.8 kΩ 1 %, 0402WGF1801TCE (C25871) | 1.37–1.89 V | ≥ V_IH 1.26 V, ≤ 2.8 V abs max (OV9281 DS Tables 8-1, 8-3) |
| 2.8 V | 1.5 kΩ 1 % (same part) | 6.19 kΩ 1 %, FH RC-02K6191FT (C324848, the R4 part) | 2.04–2.78 V | ≥ 0.7 × 2.8 V = 1.96 V (estimate: V_IH is given only at 1.8 V) at 3.20 V; ≤ 2.8 V at 3.44 V |
| 3.3 V | 0 Ω 0402, 0402WGF0000TCE (C17168) | DNP | 2.53–3.44 V | the P4 pin drives the module directly |

- **Default, high:** nominal 3.3 V × 1.8/3.3 = 1.80 V. Low corner: P4 V_OH ≥ 0.8 × 3.20 V = 2.56 V, ≈ 2.53 V after the 1 mA divider current (≈ 30 Ω output, estimate), gives ≥ **1.37 V**, 0.11 V over V_IH. High corner: **1.89 V**, only 90 mV over a 1.8 V DOVDD, so the input clamp does not conduct. (P4 DS Table 5-4)
- **Default, low:** ≤ 0.19 V (P4 V_OL ≤ 0.34 V), under V_IL 0.54 V.
- **Edge:** 818 Ω (1.5 k ∥ 1.8 k) into ≈ 25 pF (estimate: FSIN 10 pF, pigtail 10 pF, pads 5 pF) gives a ≈ 45 ns 10–90 % edge, steady and ≈ 22× under the 1 µs sync target ([timing.md](../timing.md)). The wire is electrically short (≈ 1 ns) and the source is above its Z₀, so it cannot ring.
- **Pulse:** the sensor needs ≥ 5 XVCLK cycles, ≥ 0.83 µs at the 6 MHz minimum (DS §4.8.3, Table 8-4); [firmware rule 1](firmware-rules.md) asks ≥ 10 µs.
- **In reset:** GPIO11 floats (no pull); R32 holds `XVS_CAM` at 0 V, so there is no false trigger. If FSIN is misconfigured as an output (reg 0x3006 bit 1), the fight is ≤ 2.2 mA through 818 Ω: harmless.
- **Back-powering:** with `CAM_EN` low, DOVDD is ≈ 0 V and XVS high pushes ≈ 1.6 mA into the sensor's I/O clamp. Under the ±200 mA limit but outside the voltage rating, and it back-feeds DOVDD: hence [firmware rule 1](firmware-rules.md).
- **2.8 V row:** k = 6.19/7.69 = 0.805 (0.802–0.808 with 1 % parts). Low corner: V_OH 2.56 V less the 0.43 mA divider drop ≈ 2.55 V, × 0.802 = 2.04 V (+0.08 V); high corner 3.44 V × 0.808 = 2.78 V (−20 mV). Edge ≈ 66 ns (1.21 kΩ); low ≤ 0.27 V. Both are parts already on the board; no other board pair fits (1.8 k/6.19 k → 0.775, 1.00 k/5.1 k → 0.836).
- **3.3 V row:** nothing holds `XVS_CAM` low while GPIO11 floats, so firmware drives it low first thing. [Firmware rule 1](firmware-rules.md) matters more here: a 0 Ω path lets the pin's full drive into an unpowered sensor. A level above 2.8 V (DOVDD up to 3.0 V, or buffered I/O) takes this row: ≤ 3.44 V stays under VDD-IO + 1 V (OV9281 DS p.119).

## STROBE buffer

U7 (SN74LV1T34) turns the module's STROBE into a 3.3 V signal for the P4 (GPIO12, ETM capture) and the LED gate ([led.md](led.md)).
- **Reads every module level:** its inputs take 0–5.5 V whatever VCC is; a high needs ≥ 1.43 V at the 3.44 V rail (interpolated, estimate; 1.39 V at 3.0–3.3 V), a low ≤ 0.65 V, over temperature. A 1.8 V module (V_OH ≥ 1.62 V, V_OL ≤ 0.18 V) leaves 0.19 V high-side and 0.47 V low-side margin; a 2.8 or 3.3 V module leaves ≥ 1 V (estimate). So U7 needs no change for any row. (SCLS743E §6.1, §6.3, §6.5, §8.3.3; OV9281 DS Table 8-3)
- **Output:** V_OH ≥ 2.8 V at −5.5 mA; the P4 needs 2.58 V at 3.44 V and the '1G97 ≈ 2 V. On the LED sheet R72 (1 kΩ) limits the edge current into C43 to 3.4 mA, under the 7 mA rating.
- **Input edge:** must be ≤ 20 ns/V (≤ 36 ns for a 1.8 V swing); the sensor's STROBE at ≈ 25 pF is far faster (estimate).
- **R30 10 kΩ pull-down:** STROBE is high-z until firmware sets it up, and the pigtail can be unplugged. TI recommends 10 kΩ. It loads the sensor with 180 µA at 1.8 V (330 µA at 3.3 V), adds ≤ 10 mV leakage error and keeps a floating wire's glitches short (τ ≈ 150 ns). C29 100 nF decouples VCC.
- **No hysteresis**, accepted: the '1G97 has Schmitt inputs and the P4 timestamps the first edge. If the pigtail makes the edge ring, the SN74LV1T17 Schmitt buffer is the fallback (pinout not checked).

## CSI series links

R38–R43 are 0 Ω 0201 links on every CSI line, at the FPC end. Espressif asks to "reserve series resistors on the MIPI communication lines (initially 0 Ω)" (HDG §1.3.15), so a series value can be fitted if the lanes misbehave. 0201 keeps the pad close to the trace width. Lane rate ≤ 1.5 Gbps (P4 DS §4.2.1.6).

## Not measured yet

- **STROBE high level (= module I/O level).** Before soldering the XVS wire: power the module, set STROBE as an output (sensor register), measure its high level on the back pad, fit the matching [XVS row](#xvs-level-option), then connect XVS. If the XVS trigger is unreliable: compare `XVS_CAM` on TP5 with that level, check the fitted row, and on a 2.8 V module test with +3V3 lowered to ≈ 3.2 V.
- Cable type and pin-1 edge ([FPC connector](#fpc-connector)); pigtail mating with J4; module current (lift R37); STROBE edge ringing on TP6.

## Sources

- OmniVision OV9281 datasheet v1.01 PRELIMINARY; OV9281 product brief
- Espressif ESP32-P4 datasheet v0.7; ESP32-P4 Hardware Design Guidelines (master)
- TI SN74LV1T34 datasheet SCLS743E
- Raspberry Pi 4 B and 3 B+ reduced schematics; Camera V2.1 schematic RPI-CAM-V2_1 rev 2.1; camera FFC drawing RP-008146-DS-1; CM4 datasheet Release 4
- JUSHUO AFA07-S15ECA/FCA drawings rev A (2011-03-23); Amphenol drawing 10172241 (SFW15R)
- Diodes AP7331 datasheet DS31914 Rev. 3-2; NXP UM10204 (I2C-bus specification)
