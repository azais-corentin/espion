# UDP protocol

`crates/espion-proto` is the single source of truth for everything on this page. Any
change to the format bumps the protocol version and updates this document and the
changelog at the end of it in the same commit.

> **Status: draft.** No implementation exists yet. Decided: the header, the versioning
> rules (exact version match), boards identified by their C5 MAC, and Q11.5 blob
> coordinates. The other message fields are the intended v1 content and may change until
> the first implementation lands.

## Transport

- UDP over IPv4 on the Wi-Fi network shared by the boards and the host.
- One message per datagram. A datagram is at most **1472 bytes** (1500-byte MTU minus the IPv4
  and UDP headers), so IP fragmentation never happens.
- Ports (defaults, configurable):

  | Port | Direction | Traffic |
  |---|---|---|
  | 5005 (`HOST_PORT`) | board → host, unicast | Centroids, Status |
  | 5006 | every C5 → all, broadcast | Announce (master election and FTM discovery; same port as esp-wifi-sync) |
  | 5007 | host → board, unicast | Config, FrameEpoch |

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
| `0x03` | Announce | every C5 → broadcast | 1 Hz |
| `0x10` | Config | host → board | On change |
| `0x11` | FrameEpoch | host → board | On change |

Times on the wire are **network time**: nanoseconds on the current timescale, served by
the master C5 (see [timing.md](timing.md)), as `u64`.

### `0x01` Centroids

| Field | Type | Meaning |
|---|---|---|
| board | `[u8; 6]` | Station MAC of the board's C5 |
| frame | `u64` | Frame number `k` since the frame epoch (same `k` on every board for the same instant) |
| capture_time | `u64` | Network time (ns) of the middle of the exposure |
| sync | `SyncState` | Locked / acquiring / free-running, plus the estimated error (ns) and the `timescale` id |
| flags | `u8` | Bit 0: dark frame (LEDs off, ambient IR only) |
| part, parts | `u8`, `u8` | This datagram's index and the datagram count for this frame |
| blobs | `[Blob]` | Centroids found in the frame (this part) |

`Blob`: `x`, `y` as `u16` fixed point in 1/32 pixel (Q11.5: up to 2048 px, enough for
the full 1280 × 800 mode as well as 640 × 400 binned), `area` as `u16` pixels and `peak`
as `u8` intensity.

### `0x02` Status

Firmware versions of both chips, uptime, sync role (master/slave/candidate), FTM
statistics (session RTT, fit residual), SYNC lock state on the P4, dropped-frame and
dropped-packet counters.

### `0x03` Announce

Sent by every C5. It drives the automatic master election
([timing.md](timing.md#master-election-automatic-with-failover)) and FTM discovery
(extends esp-wifi-sync's master-only Announce):

| Field | Type | Meaning |
|---|---|---|
| board | `[u8; 6]` | Station MAC of the sender |
| role | `Role` | Master / slave / candidate |
| timescale | `u32` | Random id of the timescale the sender serves or follows; a change means network time restarted |
| locked | `bool` | Whether the sender is locked to the timescale |
| responder | `Option<[u8; 6]>` | Master only: SoftAP MAC of the FTM responder |
| time | `u64` | Sender's network time estimate (ns) at send time. Slaves use the master's to unwrap FTM timestamps |

### `0x10` Config

Exposure (µs, default 500), threshold, minimum and maximum blob area, dark frame interval
N (one LED-off frame every N frames), frame-rate divider.

### `0x11` FrameEpoch

`t0: u64`, the network time (ns) of frame 0, and the frame period as a rational
(`num`/`den` ns, `10⁹/120` by default). See [timing.md](timing.md).

## Changelog

| Version | Change |
|---|---|
| 1 | Initial draft (not implemented) |
