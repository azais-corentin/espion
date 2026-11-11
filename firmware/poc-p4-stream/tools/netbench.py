#!/usr/bin/env python3
"""Host side of the poc-p4-stream test service (src/bench.rs). Python standard library only.

Every socket is bound to the link-local address of one interface (default eth0), and the host
always sends first, so a stateful firewall lets the board's replies in.

  netbench.py tx --source internal|psram|camera|capture [--seconds 20] [--pgm out.pgm]
      Board to host: asks the board (UDP 5010) to send for that long, receives, checks every
      byte (internal, psram) or reassembles camera frames (camera; --pgm writes the last whole
      frame), and prints the host's figures and the board's report.
  netbench.py rx --rate 1000 [--size 1452] [--seconds 60] [--stream 1]
      Host to board: sends datagrams of known content to UDP 5011 at that rate (0: as fast as
      the socket takes them), then asks the board for its counts and prints both.

  netbench.py done
      The board logs "bench: host done": the end of a test sequence.

Both can run at the same time (two processes): video out, control in.
"""

import argparse
import os
import socket
import struct
import sys
import time

TX_PORT = 5010
RX_PORT = 5011
DATAGRAM = 1452
HEADER = 16
REPORT_FRAME = 0xFFFF_FFFF
QUERY = 0xFFFF_FFFF
RX_SHIFT = 251
KINDS = {"internal": b"I", "psram": b"P", "camera": b"F", "capture": b"C"}


def pattern(n):
    """Bytes 0..n of the board's test pattern: byte i is (i * 2654435761 mod 2^32) >> 24."""
    return bytes(((i * 2654435761) & 0xFFFF_FFFF) >> 24 for i in range(n))


def link_local(iface):
    """The link-local IPv6 address of `iface`, from /proc/net/if_inet6."""
    with open("/proc/net/if_inet6") as f:
        for line in f:
            addr, _idx, _plen, scope, _flags, name = line.split()
            if name == iface and scope == "20":
                return socket.inet_ntop(socket.AF_INET6, bytes.fromhex(addr))
    sys.exit(f"no link-local address on {iface}")


def udp6_counters():
    """The host's UDP6 error counters (/proc/net/snmp6)."""
    out = {}
    with open("/proc/net/snmp6") as f:
        for line in f:
            k, v = line.split()
            if k.startswith("Udp6"):
                out[k] = int(v)
    return out


def iface_counters(iface):
    base = f"/sys/class/net/{iface}/statistics/"
    names = ("rx_packets", "rx_dropped", "rx_errors", "tx_packets", "tx_dropped", "tx_errors")
    return {n: int(open(base + n).read()) for n in names}


def diff(a, b):
    return {k: b[k] - a[k] for k in a if b[k] != a[k]}


def open_socket(args):
    scope = socket.if_nametoindex(args.iface)
    s = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
    s.bind((link_local(args.iface), 0, 0, scope))
    return s, (args.board, scope)


def cmd_tx(args):
    s, (board, scope) = open_socket(args)
    kind = KINDS[args.source]
    expected = {}
    seen = bytearray(4_000_000)
    buf = bytearray(2048)
    mv = memoryview(buf)
    got = dup = late = bad = bad_bytes = 0
    payload = 0
    top = -1
    first = last = None
    frames = {}  # camera: frame -> [data, chunks received, chunks needed]
    whole = []
    report = None
    udp0, if0 = udp6_counters(), iface_counters(args.iface)
    prefetch = 0 if args.prefetch is None else args.prefetch + 1
    s.sendto(kind + bytes([prefetch]) + struct.pack("<H", args.seconds), (board, TX_PORT, 0, scope))
    # Until the first datagram (or, for a capture run, the report): the whole run plus the
    # board's camera start; then a gap of 2 s ends it.
    s.settimeout(args.seconds + 5.0)
    while True:
        try:
            n = s.recv_into(buf)
        except socket.timeout:
            break
        s.settimeout(2.0)
        now = time.perf_counter()
        seq, frame, off, total = struct.unpack_from("<IIII", buf)
        if frame == REPORT_FRAME:
            report = bytes(mv[HEADER:n]).decode(errors="replace")
            break
        if first is None:
            first = now
        last = now
        got += 1
        payload += n
        if seq < len(seen):
            if seen[seq]:
                dup += 1
            seen[seq] = 1
        if seq < top:
            late += 1
        top = max(top, seq)
        data = bytes(mv[HEADER:n])
        if args.source in ("internal", "psram"):
            exp = expected.get(total)
            if exp is None:
                exp = expected[total] = pattern(total)
            if data != exp[off : off + len(data)]:
                bad += 1
                bad_bytes += sum(1 for x, y in zip(data, exp[off:]) if x != y)
        elif args.source == "camera":
            f = frames.get(frame)
            if f is None:
                f = frames[frame] = [bytearray(total), 0, -(-total // (DATAGRAM - HEADER))]
            f[0][off : off + len(data)] = data
            f[1] += 1
            if f[1] == f[2]:
                whole.append(frame)
                if args.pgm:
                    last_whole = f[0]
                del frames[frame]
    udp1, if1 = udp6_counters(), iface_counters(args.iface)
    span = (last - first) if first is not None and last != first else 0
    lost = top + 1 - got + dup if top >= 0 else 0
    print(f"host: {got} datagrams, {payload} B UDP payload, span {span:.3f} s")
    if span:
        print(
            f"host: {payload / span / 1e6:.3f} MB/s = {payload * 8 / span / 1e6:.2f} Mbit/s of UDP"
            f" payload, {got / span:.0f} datagrams/s"
        )
    print(f"host: lost {lost}, duplicates {dup}, reordered {late}, bad {bad} ({bad_bytes} bytes)")
    if args.source == "camera":
        print(
            f"host: {len(whole)} whole frames"
            + (f" = {len(whole) / span:.2f} fps" if span else "")
            + f", {len(frames)} incomplete; first {whole[:3]} last {whole[-3:]}"
        )
        if args.pgm and whole:
            with open(args.pgm, "wb") as f:
                f.write(b"P5\n%d %d\n255\n" % (args.width, args.height))
                f.write(last_whole[: args.width * args.height])
            print(f"host: frame {whole[-1]} written to {args.pgm}")
    print(f"host: UDP6 counters changed: {diff(udp0, udp1)}; {args.iface}: {diff(if0, if1)}")
    print(f"board: {report}" if report else "board: no report")


def cmd_rx(args):
    s, (board, scope) = open_socket(args)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, args.sndbuf)
    dest = (board, RX_PORT, 0, scope)
    body = args.size - 8
    base = pattern(body + RX_SHIFT)
    payloads = [base[k : k + body] for k in range(RX_SHIFT)]
    stream = struct.pack("<I", args.stream)
    count = int(args.rate * args.seconds) if args.rate else None
    udp0, if0 = udp6_counters(), iface_counters(args.iface)
    start = time.perf_counter()
    end = start + args.seconds
    sent = errors = 0
    seq = 0
    while True:
        now = time.perf_counter()
        if count is not None:
            if seq >= count:
                break
            due = start + seq / args.rate
            if due > now:
                if due - now > 0.002:
                    time.sleep(due - now - 0.001)
                continue
        elif now >= end:
            break
        try:
            s.sendto(struct.pack("<I", seq) + stream + payloads[seq % RX_SHIFT], dest)
            sent += 1
        except OSError:
            errors += 1
        seq += 1
    span = time.perf_counter() - start
    time.sleep(0.2)
    udp1, if1 = udp6_counters(), iface_counters(args.iface)
    print(
        f"host: sent {sent} datagrams of {args.size} B in {span:.3f} s ="
        f" {sent / span:.0f}/s, {sent * args.size / span / 1e6:.3f} MB/s; send errors {errors}"
    )
    print(f"host: UDP6 counters changed: {diff(udp0, udp1)}; {args.iface}: {diff(if0, if1)}")
    s.settimeout(1.0)
    for _ in range(5):
        s.sendto(struct.pack("<II", 0, QUERY), dest)
        try:
            print("board:", s.recv(2048).decode(errors="replace"))
            return
        except socket.timeout:
            pass
    print("board: no reply to the query")


def cmd_done(args):
    s, (board, scope) = open_socket(args)
    s.sendto(b"D", (board, TX_PORT, 0, scope))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    p.add_argument("--iface", default="eth0")
    p.add_argument("--board", default="fe80::4553:9281")
    sub = p.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("tx")
    t.add_argument("--source", choices=KINDS, default="internal")
    t.add_argument("--seconds", type=int, default=20)
    t.add_argument("--pgm")
    t.add_argument("--width", type=int, default=1280)
    t.add_argument("--height", type=int, default=800)
    t.add_argument("--prefetch", type=int, choices=range(7),
                   help="cache prefetch mode for this and later runs (src/prefetch.rs: 0 off, "
                   "1 L1+L2 autoload, 2 L1, 3 L2, 4 L1+L2 on a hit, 5 L1 preload, "
                   "6 L1 preload ahead); default: unchanged")
    r = sub.add_parser("rx")
    r.add_argument("--rate", type=float, default=1000.0, help="datagrams/s, 0 = unpaced")
    r.add_argument("--size", type=int, default=DATAGRAM)
    r.add_argument("--seconds", type=float, default=60.0)
    r.add_argument("--stream", type=int, default=os.getpid())
    r.add_argument("--sndbuf", type=int, default=32768, help="SO_SNDBUF (bounds the host queue)")
    sub.add_parser("done", help="tell the board the test sequence is over (it logs it)")
    args = p.parse_args()
    {"tx": cmd_tx, "rx": cmd_rx, "done": cmd_done}[args.cmd](args)


if __name__ == "__main__":
    main()
