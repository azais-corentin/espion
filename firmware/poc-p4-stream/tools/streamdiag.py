#!/usr/bin/env python3
"""Test hooks of the poc-p4-stream camera stream service (src/stream/diag.rs, UDP 5010).
Python standard library only. The socket is bound to the link-local address of one interface
(default eth0), so nothing leaves elsewhere.

  streamdiag.py mark            start a measurement window (CPU accounts, network counters)
  streamdiag.py report          the board's report since the mark, one key=value per line
  streamdiag.py fault 1|2       the next mode change fails: 1 sensor bus, 2 no first frame
  streamdiag.py stall           sensor stream off behind the receiver's back
  streamdiag.py done            the board logs "stream: host done" (board-run.sh stops on it)
  streamdiag.py garbage N       N malformed datagrams to the camera port 5009, back to back
  streamdiag.py small N         the next N JPEG encodes get a 4 KiB output (OutputTooSmall)
  streamdiag.py encodeonly 1|0  JPEG files made and dropped, not sent (1), or sent again (0)
"""

import argparse
import os
import socket
import sys

DIAG_PORT = 5010
CAM_PORT = 5009


def link_local(iface):
    """The link-local IPv6 address of `iface`, from /proc/net/if_inet6."""
    with open("/proc/net/if_inet6") as f:
        for line in f:
            addr, _idx, _plen, scope, _flags, name = line.split()
            if name == iface and scope == "20":
                return socket.inet_ntop(socket.AF_INET6, bytes.fromhex(addr))
    sys.exit(f"no link-local address on {iface}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    p.add_argument("--iface", default="eth0")
    p.add_argument("--board", default="fe80::4553:9281")
    p.add_argument(
        "command",
        choices=["mark", "report", "fault", "stall", "done", "garbage", "small", "encodeonly"],
    )
    p.add_argument("arg", nargs="?", default="")
    args = p.parse_args()

    scope = socket.if_nametoindex(args.iface)
    s = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
    s.bind((link_local(args.iface), 0, 0, scope))
    s.settimeout(1.0)

    if args.command == "garbage":
        n = int(args.arg or "1000")
        for i in range(n):
            # Wrong magic, right magic with a wrong version, truncated, random bytes.
            d = [b"XX\x02\x20" + bytes(20), b"ES\x09\x20" + bytes(20), b"ES", os.urandom(64)][i % 4]
            s.sendto(d, (args.board, CAM_PORT, 0, scope))
        print(f"sent {n} malformed datagrams to port {CAM_PORT}")
        return

    cmd = {
        "mark": b"M",
        "report": b"R",
        "fault": b"F" + args.arg.encode(),
        "stall": b"S",
        "done": b"D",
        "small": b"C" + args.arg.encode(),
        "encodeonly": b"X" + args.arg.encode(),
    }[args.command]
    for _ in range(5):
        s.sendto(cmd, (args.board, DIAG_PORT, 0, scope))
        try:
            reply = s.recv(2048).decode(errors="replace")
        except socket.timeout:
            continue
        if args.command == "report":
            print("\n".join(reply.split()))
        else:
            print(reply)
        return
    sys.exit("no answer from the board")


if __name__ == "__main__":
    main()
