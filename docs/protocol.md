# UDP protocol

`crates/espion-proto` is the source of truth for this page. Any format change bumps the
protocol version and updates this page and its changelog in the same commit.

> **Status: draft, not implemented.** Settled: header, versioning (exact match), boards identified
> by their Ethernet MAC, Q11.5 blob coordinates. Other fields are planned v1 and may still change.

## Transport

- UDP over IPv4 on the wired Ethernet shared by boards and host (100 Mbit/s per board).
  Boards get their address by DHCP, so the network needs a DHCP server.
- One message per datagram, at most **1472 bytes** (1500-byte MTU minus IPv4 and UDP
  headers), so IP never fragments.
- The host is whoever last sent the board a Config or FrameEpoch. Until then the board
  broadcasts Status to port 5005 and sends no Centroids.
- No retransmission. A lost Centroids packet is a lost frame; the host copes with gaps.
- Ports (defaults, configurable):

  | Port | Direction | Traffic |
  |---|---|---|
  | 5005 (`HOST_PORT`) | board → host, unicast (Status: broadcast until the host is known) | Centroids, Status |
  | 5007 | host → board, unicast or broadcast | Config, FrameEpoch |
  | 319, 320 | multicast 224.0.1.129 | PTP event and general messages (IEEE 1588, not espion-proto; see [timing.md](timing.md)) |

## Packet layout

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 2 | magic | `0x45 0x53` (`"ES"`). Anything else is not an espion packet |
| 2 | 1 | version | Protocol version, starts at `1` |
| 3 | 1 | kind | Message kind (table below) |
| 4 | … | body | [postcard](https://postcard.jamesmunns.com/wire-format)-encoded struct for (`version`, `kind`) |

The header is raw bytes, not postcard. A receiver checks magic and version before reading
the body. Packets with an unknown magic, version or kind are counted and dropped, never decoded.

## Versioning

- One version number covers everything: header, all kinds, all bodies.
- Bump it for **any** body change (field added, removed, reordered, retyped) or any change in
  what a field means. Postcard has no field tags, so no change is backward compatible.
- Never reuse a kind value, even after its message is removed.
- Firmware and host must speak the same version. The host logs each board's version and
  reports mismatches; it never decodes a foreign version.
- Every bump adds a changelog row.

## Message kinds (draft v1)

| Kind | Name | Direction | Rate |
|---|---|---|---|
| `0x01` | Centroids | board → host | Once per frame (120 Hz), split if needed |
| `0x02` | Status | board → host | 1 Hz |
| `0x03` | — | — | Reserved: was Announce in an earlier draft. PTP now does election and discovery |
| `0x10` | Config | host → board | On change |
| `0x11` | FrameEpoch | host → board | On change |

Times on the wire are **network time**: PTP nanoseconds on the current grandmaster's clock
(see [timing.md](timing.md)), as `u64`.

### `0x01` Centroids

| Field | Type | Meaning |
|---|---|---|
| board | `[u8; 6]` | Ethernet MAC of the board |
| frame | `u64` | Frame number `k` since the frame epoch (same `k` on every board for the same instant) |
| capture_time | `u64` | Network time (ns) of the middle of the exposure |
| sync | `SyncState` | Locked / acquiring / free-running, plus the estimated error (ns) and the grandmaster's clockIdentity (`[u8; 8]`) |
| flags | `u8` | Bit 0: dark frame (LEDs off, ambient IR only) |
| part, parts | `u8`, `u8` | This datagram's index and the datagram count for this frame |
| blobs | `[Blob]` | Centroids found in the frame (this part) |

`Blob`: `x`, `y` as `u16` in 1/32 pixel (Q11.5: up to 2048 px, enough for full 1280 × 800
and binned 640 × 400), `area` as `u16` pixels, `peak` as `u8` intensity.

### Other messages

- **`0x02` Status:** firmware version, uptime, link state (speed, duplex), PTP port state
  (master/slave/listening/passive), grandmaster clockIdentity, PTP statistics (offset from
  master, mean path delay), PPS servo lock state, dropped-frame and dropped-packet counters.
- **`0x10` Config:** exposure (µs, default 500), threshold, min and max blob area,
  dark-frame interval N (one LED-off frame every N frames), frame-rate divider.
- **`0x11` FrameEpoch:** `t0: u64`, the network time (ns) of frame 0, and the frame period
  as a ratio `num`/`den` ns (`10⁹/120` by default). See [timing.md](timing.md).

## Changelog

| Version | Change |
|---|---|
| 1 | Initial draft (not implemented). Revised before implementation for Ethernet + PTP: Announce (`0x03`) dropped, boards identified by their Ethernet MAC, `SyncState` carries the grandmaster clockIdentity instead of a timescale id |
