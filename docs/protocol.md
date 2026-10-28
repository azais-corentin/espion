# UDP protocol

`crates/espion-proto` is the single source of truth for everything on this page. Any
change to the format bumps the protocol version and updates this document and the
changelog at the end of it in the same commit.

> **Status: draft.** No implementation exists yet. Decided: the header, the versioning
> rules (exact version match), boards identified by their Ethernet MAC, and Q11.5 blob
> coordinates. The other message fields are the intended v1 content and may change until
> the first implementation lands.

## Transport

- UDP over IPv4 on the wired Ethernet network (100 Mbit/s per board) shared by the boards
  and the host. Boards get their address by DHCP; the network needs a DHCP server.
- One message per datagram. A datagram is at most **1472 bytes** (1500-byte MTU minus the IPv4
  and UDP headers), so IP fragmentation never happens.
- The host is whoever sent the board its last Config or FrameEpoch. Until then a board
  broadcasts Status to port 5005 and sends no Centroids.
- Ports (defaults, configurable):

  | Port | Direction | Traffic |
  |---|---|---|
  | 5005 (`HOST_PORT`) | board → host, unicast (Status: broadcast until the host is known) | Centroids, Status |
  | 5007 | host → board, unicast or broadcast | Config, FrameEpoch |
  | 319, 320 | multicast 224.0.1.129 | PTP event and general messages (IEEE 1588, not espion-proto; see [timing.md](timing.md)) |

- No retransmission. Lost centroid packets are lost frames; the host tolerates gaps.

## Packet layout

```
 0        1        2        3        4 ...
+--------+--------+--------+--------+------------------------------+
| magic  | magic  |version |  kind  | body (postcard)              |
| 0x45 E | 0x53 S |  u8    |  u8    |                              |
+--------+--------+--------+--------+------------------------------+
```

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 2 | magic | `0x45 0x53` (`"ES"`). Anything else is not an espion packet |
| 2 | 1 | version | Protocol version, starts at `1` |
| 3 | 1 | kind | Message kind (table below) |
| 4 | … | body | [postcard](https://postcard.jamesmunns.com/wire-format)-encoded struct for (`version`, `kind`) |

The header is raw bytes, not postcard. A receiver checks magic and version before it
touches the body. Packets with an unknown magic, version or kind are dropped and counted,
and are never decoded.

## Versioning

- One version number covers the whole protocol: header, all message kinds and all bodies.
- Bump the version for **any** change to any body (field added, removed, reordered, retyped)
  or to the semantics of a field. Postcard has no field tags, so no change is backward
  compatible.
- Never reuse a kind value, even after a message is removed.
- Firmware and host must speak the same version. The host logs the version of every board it
  hears from and reports mismatches. It does not decode foreign versions.
- Every bump adds a row to the changelog below.

## Message kinds (draft v1)

| Kind | Name | Direction | Rate |
|---|---|---|---|
| `0x01` | Centroids | board → host | Once per frame (120 Hz), split if needed |
| `0x02` | Status | board → host | 1 Hz |
| `0x03` | — | — | Reserved: was Announce in an earlier draft. PTP now does election and discovery |
| `0x10` | Config | host → board | On change |
| `0x11` | FrameEpoch | host → board | On change |

Times on the wire are **network time**: nanoseconds of PTP time on the current
grandmaster's clock (see [timing.md](timing.md)), as `u64`.

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

`Blob`: `x`, `y` as `u16` fixed point in 1/32 pixel (Q11.5: up to 2048 px, enough for
the full 1280 × 800 mode as well as 640 × 400 binned), `area` as `u16` pixels and `peak`
as `u8` intensity.

### `0x02` Status

Firmware version, uptime, link state (speed, duplex), PTP port state
(master/slave/listening/passive), grandmaster clockIdentity, PTP statistics (offset from
master, mean path delay), PPS servo lock state, dropped-frame and dropped-packet counters.

### `0x10` Config

Exposure (µs, default 500), threshold, minimum and maximum blob area, dark frame interval
N (one LED-off frame every N frames), frame-rate divider.

### `0x11` FrameEpoch

`t0: u64`, the network time (ns) of frame 0, and the frame period as a rational
(`num`/`den` ns, `10⁹/120` by default). See [timing.md](timing.md).

## Changelog

| Version | Change |
|---|---|
| 1 | Initial draft (not implemented). Revised before implementation for Ethernet + PTP: Announce (`0x03`) dropped, boards identified by their Ethernet MAC, `SyncState` carries the grandmaster clockIdentity instead of a timescale id |
