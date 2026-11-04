# Schematic review checklist

Items carry an ID that findings and passes cite (`"item": "PWR-3"`). Every item applies in `change` mode (to the parts and nets in scope) and in `standard` and `full` mode, except those marked **(full)**: those run in `full` mode only. **Auto:** names the `kicad_check` check that already proves the item: read its result and don't redo it by hand. An item that doesn't apply to the scope is `na`, not a pass.

Numbers come from the datasheet (document, page, table/figure) or from `kicad_query`; computed numbers show the formula and its inputs. Worst case means component tolerance and rail min/max (worked out from the schematic's values with the block docs' formulas; the required ranges are in `docs/hardware/README.md`), as limited by the review skill's **Design assumptions** (`skill://kicad-schematic-review`): sources within their standard, accessories working, ±5 % rails in spec, no TCR or maximum-temperature leakage stacking, latest silicon revision. A case those assumptions exclude is not a finding under any item below.

## ARCH: architecture (orchestrator, phase 1)
- **ARCH-1** Power tree: every rail, its source, its loads and its range at min/max input, worked out from the schematic. Sources and loads match the Rails table in `docs/hardware/README.md` and `docs/architecture.md`, and each range meets the table's required range.
- **ARCH-2** External interfaces: every connector, button, exposed pad and shield, with the nets that reach it. Feeds PROT.
- **ARCH-3** Blocks vs design docs: every block of the `docs/hardware/README.md` block diagram exists as a sheet, with the parts its block doc names (`kicad_query hierarchy`, `symbols`).
- **ARCH-4** Reference designs: for each major IC, the vendor's application circuit or hardware design guide (for the ESP32-P4: Espressif's hardware design guidelines and schematic checklist). Deviations from it are intentional and written down in the block's doc in `docs/hardware/`; an undocumented deviation is a finding.

## PWR: power
- **PWR-1** Every regulator input stays within its operating range over the source's specified range (its standard or datasheet regulation limits), including UVLO and abs max.
- **PWR-2** Output setting: recompute the feedback divider with the datasheet's V_REF and its tolerance, and the resistor tolerance; the result fits every load's supply range.
- **PWR-3** Load current: sum the loads per rail (typ and max); each regulator, load switch, mux and fuse carries it with margin. LDOs: dropout headroom at minimum input. **(full)** Thermal: P = (V_in − V_out)·I for linear parts, efficiency for switchers; T_j at the worst ambient stays below the limit.
- **PWR-4** Switcher parts: inductor value in the datasheet's range, I_sat above the peak switch current, DCR; input and output capacitance (effective, after DC-bias derating) in the stable range, ESR as the datasheet asks; compensation parts against the datasheet's equations; diode ratings for a non-synchronous switcher.
- **PWR-5** Inrush and total capacitance per rail vs the source's limits (USB, PoE class, soft start, load switch ramp).
- **PWR-6** Sequencing, enable and power-good: each IC's required order (core before I/O, analog before digital, etc.) holds; enable thresholds and hysteresis; reset held until the rails are stable (supervisor or RC vs soft start).
- **PWR-7** Decoupling: every power pin has the capacitors the datasheet asks for (value, count, dielectric), and each capacitor is drawn next to the pin it serves. **Auto:** `decoupling` proves at least one capacitor per IC rail on the sheet; values and counts per pin are manual.
- **PWR-8** Power-path OR-ing and back-feed: no source can feed another (USB VBUS into PoE and back); behaviour at source switch-over.

## PASS: passive ratings
- **PASS-1** Capacitors: voltage rating ≥ 1.5× the working voltage for ceramics and tantalums (2× for tantalums on low-impedance rails); effective capacitance after DC-bias derating where the value matters (regulator outputs, timing, filters); dielectric (C0G for timing and filters, X7R/X5R for decoupling).
- **PASS-2** Resistors: power P = I²R or V²/R below the derated rating (≤ 50 % of the rated power for the package); voltage rating for 0402 and smaller; tolerance where the value sets a threshold or a voltage.
- **PASS-3** Inductors and ferrites: current rating and I_sat above the peak; a ferrite's DC resistance and its impedance at the noise frequency.
- **PASS-4** Polarity: diodes, LEDs, polarised capacitors and MOSFETs (source/drain, body diode direction) are the right way round, judged from the netlist, not the drawing.
- **PASS-5** Recompute every divider, LED current-limit resistor, RC time constant and filter corner in scope with the actual values; worst case where it sets a threshold.

## IC: pins and parts
- **IC-1** Every pin of every IC against the datasheet's pin table for the latest silicon revision: function, net, required pull, required external part. Every pin, not only the ones that look important.
- **IC-2** Unused pins handled as the datasheet says (grounded, tied high, left open, no-connect flag); unused units of multi-unit parts (op-amps, gates) terminated.
- **IC-3** Exposed/thermal pad connected to the net the datasheet names.
- **IC-4** Abs max of each pin vs the worst-case voltage of the net on it, including during power-up when its own supply is off (back-powering through I/O).
- **IC-5** Symbol pinout = datasheet pinout, symbol pins ↔ footprint pads. **Auto:** `footprints` proves pins ↔ pads; library parts themselves are reviewed with `kicad-library-review`.

## RST: reset, boot and defined states
- **RST-1** Reset/enable inputs have the pull and the RC (or supervisor) the datasheet recommends; the RC time constant vs the rail's ramp.
- **RST-2** Strapping/boot pins: the level each one has at reset (pulls, and what else drives the net during reset) selects the intended mode; shared strap/signal pins can't be pulled to the wrong level by the other device.
- **RST-3** Defined states while the MCU is in reset or unpowered: every enable, chip select, MOSFET gate, load switch and LED driver input has a pull that keeps its device in its safe state. Check what each MCU pin does at reset (input, pull, or a function such as USB) from the datasheet's IO table.
- **RST-4** A user-reachable way to reset the board (button, power cycle), and a way to recover a peripheral that can hang (reset line, power switch, bus recovery).

## BUS: digital interfaces
- **BUS-1** I²C/SCCB: pull-ups on SDA and SCL to the bus's rail (**Auto:** `pulls`); value: R_max = t_r / (0.8473·C_bus), R_min = (V_DD − V_OL)/I_OL, for the bus's speed and estimated capacitance; unique 7-bit addresses on each bus (address pins from the netlist); bus recovery when the MCU resets mid-transfer.
- **BUS-2** SPI: each device has its own CS, pulled to inactive; mode (CPOL/CPHA) supported by both ends.
- **BUS-3** UART: TX → RX crossed (check whose point of view each pin name takes); RTS/CTS crossed when used.
- **BUS-4** Other buses (RMII, MDIO, CSI control, one-wire LED data): direction of every signal, the clock source, and required pulls (MDIO pull-up: **Auto:** `pulls`).
- **BUS-5** Voltage domains: every signal between two devices has compatible levels (V_OH ≥ V_IH min, V_OL ≤ V_IL max, input ≤ abs max on both sides, at worst-case rails); level shifters where not, with the right direction and speed.
- **BUS-6** Open-drain/open-collector outputs have pull-ups. **Auto:** `pulls`.

## HS: high-speed and pairs
- **HS-1** Differential pairs named `_P/_N` and in the right net class. **Auto:** `netclasses`.
- **HS-2** Termination, series resistors and AC-coupling as the interface standard and the datasheet ask (e.g. series R on clocks and fast single-ended lines).
- **HS-3** The layout constraints each pair needs (impedance, length match, spacing) are carried as net classes and rules and listed in `docs/hardware/layout.md`. **Auto:** `netclasses`, `rules` prove they exist and load.

## CLK: clocks
- **CLK-1** Crystal load capacitors: C = 2·(C_L − C_stray) with the crystal's C_L and a stated stray estimate; the crystal's ESR and drive level within the oscillator's limits.
- **CLK-2** Clock distribution: who drives each clock net, the frequency each receiver expects, no two drivers.

## ANA: analog
- **ANA-1** ADC inputs: source impedance and an RC filter fitting the ADC's sampling; the input range at worst case stays within the ADC's range.
- **ANA-2** Op-amps and comparators: input polarity, supply and common-mode range, output swing vs what follows; stability with the capacitive load and the feedback network; unused sections terminated.
- **ANA-3** Sense resistors and dividers: power, tolerance and (for measurements whose accuracy the firmware relies on) temperature coefficient enough for that accuracy. Threshold dividers that only select a mode or a switchover point are judged at tolerance, not drift.

## PROT: protection
- **PROT-1** ESD protection on every contact a user reaches in use, from ARCH-2: user-pluggable connector pins, shields, buttons, exposed pads and test points the user can touch; working voltage above the line's maximum; capacitance acceptable for the signal's speed. Internal connectors to the project's own accessories (camera FPC, pigtails) are assembled unpowered and need none.
- **PROT-2** Input power within the source's standard: transient/ESD clamp on user-pluggable inputs, inrush, undervoltage and hot-plug behaviour. No overvoltage cut-off, reverse-polarity or overcurrent protection against sources outside their standard (USB stays ≤ 5.5 V, PoE follows 802.3).
- **PROT-3** Outputs on user-pluggable connectors are short-circuit safe. Supplies to the project's own accessories (e.g. the camera module) need no current limit: the accessory is assumed to work.
- **PROT-4** Series resistors or current limits on nets coming from user-pluggable connectors into MCU pins.
- **PROT-5** Inductive and pulsed loads: MOSFETs and switches protected against the transients they switch (snubber, clamp, body diode rating).

## EMC
- **EMC-1** Switching regulators have an input filter so their noise doesn't leave on the input rail; sensitive loads (analog, PLL, crystal) are filtered or fed from a clean rail.
- **EMC-2** Shield and chassis grounding as the connector and the interface standard ask (e.g. Ethernet shield RC/HV cap, isolation voltage).

## FW: firmware and bring-up
- **FW-1** GPIO map: every MCU pin's reset function, its use on this board, its pull, and its constraints (strap, ADC-capable, USB, input-only) match `docs/hardware/firmware-rules.md` (GPIO map, straps) and the schematic (`kicad_query symbol` on the MCU). Each peripheral instance and each signal is used once.
- **FW-2** The rules in `docs/hardware/firmware-rules.md` are complete and correct for the circuits in scope: every behaviour the hardware relies on firmware for has a rule, its formulas name the right parts, and the values worked out from them with the schematic's parts are consistent with the block docs' requirements. Whether `firmware/cam-p4` follows them is out of scope: a schematic review doesn't read the firmware code.
- **FW-3** Debug and programming access without soldering (USB Serial/JTAG, UART console), and a boot-mode entry.
- **FW-4** A probe point on every rail and on the signals bring-up will probe (clocks, PPS, strobe, reset); at least one status indicator. A pad of an accessible passive on the net (series resistor, pull resistor, divider, decoupling capacitor) is a probe point: such a net needs no dedicated test point. Ask for a test point only where no such pad exists (e.g. a net between two IC pins, or one under a shield or module).
- **FW-5** Firmware can measure what it needs to decide (power source, rail sense, temperature) and can switch off what it must (load switches, LED power).

## PROD: production (full)
- **PROD-1** BOM lines: one MPN per value and footprint, no MPN with two values; values that could merge without hurting the circuit. **Auto:** `bom_lines` lists all three; judging a merge candidate is manual.
- **PROD-2** Every BOM part carries Manufacturer, MPN and LCSC. **Auto:** `fields`.
- **PROD-3** Lifecycle: no EOL/NRND part. Stock, price and second sources belong to `kicad-part-sourcing`; report only what a reviewer saw while checking a datasheet.

## DOC: documentation and drawing
- **DOC-1** ERC is clean, and every exclusion carries a reason. **Auto:** `erc`.
- **DOC-2** Drawing rules. **Auto:** `style`. Readability the check can't see (crowding, signal flow left to right, misleading placement) by looking with `kicad_render`.
- **DOC-3** `docs/hardware/` matches the schematic wherever it states something: reference designators, rails, nets, net classes, GPIO numbers, IC families, and the parts each formula takes (`kicad_query symbols`, `symbol`, `netclasses`, `net`). Say which side you think is wrong. A part value, part number or result calculated from them written in `docs/hardware/` is a minor finding, fixed by removing it.
- **DOC-4** Net names say what the net is (no `Net-(…)` on nets that cross sheets or that bring-up will probe), and names are consistent across sheets.
- **DOC-5** Title block filled on every sheet (`kicad_query sheet`).

## Sources
- M. Schulte, "A Schematic Review Checklist for Firmware Engineers", Interrupt (Memfault), 2024: https://interrupt.memfault.com/blog/schematic-review-checklist (RST-3/4, BUS-1/3, FW).
- Schemalyzer, "How to Review a Schematic", 2025: https://www.schemalyzer.com/en/blog/schematic-review/best-practices/how-to-review-schematic (phases, ARCH-4, PWR, BUS-1 formulas, PASS).
- M. Harris, "Schematic Design Review Checklist", Altium, 2021/2026: https://resources.altium.com/p/schematic-review-checklist (PASS, PROT, EMC, PROD, DOC).
- Espressif, ESP Hardware Design Guidelines, schematic checklists: https://docs.espressif.com/projects/esp-hardware-design-guidelines/ (ARCH-4, RST-2).
