#!/usr/bin/env python3
"""Decode still dumps from a poc-p4-eth log capture into PGM and PNG files.

The firmware prints (see `src/camera.rs`, `dump_still`):

    @@IMG <name> <width> <height> <bytes>
    @@D <line index, hex> <base64 of up to 96 bytes>
    ...
    @@END <crc32 hex> <line count>

Usage: still.py <log file> [output dir, default /tmp/poc-p4-eth]

Only the Python standard library is used (the nix dev shell has python3, no PIL).
"""

import base64
import os
import re
import struct
import sys
import zlib

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def png(path, width, height, pixels):
    """Writes an 8-bit greyscale PNG."""
    raw = b"".join(b"\x00" + pixels[y * width:(y + 1) * width] for y in range(height))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        f.write(chunk(b"IEND", b""))


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out_dir = sys.argv[2] if len(sys.argv) > 2 else "/tmp/poc-p4-eth"
    os.makedirs(out_dir, exist_ok=True)
    with open(sys.argv[1], "rb") as f:
        text = f.read().decode("ascii", errors="replace")

    image = None
    ok = 0
    for raw_line in text.splitlines():
        line = ANSI.sub("", raw_line).strip("\r\n ")
        at = line.find("@@")
        if at < 0:
            continue
        fields = line[at:].split()
        tag = fields[0]
        if tag == "@@IMG" and len(fields) == 5:
            name, width, height, size = fields[1], int(fields[2]), int(fields[3]), int(fields[4])
            image = {"name": name, "w": width, "h": height, "size": size, "parts": {}, "bad": 0}
        elif tag == "@@D" and image is not None and len(fields) == 3:
            try:
                image["parts"][int(fields[1], 16)] = base64.b64decode(fields[2], validate=True)
            except ValueError:
                image["bad"] += 1
        elif tag == "@@END" and image is not None and len(fields) == 3:
            crc, count = int(fields[1], 16), int(fields[2])
            missing = [i for i in range(count) if i not in image["parts"]]
            data = b"".join(image["parts"].get(i, b"") for i in range(count))
            name = image["name"]
            if missing or image["bad"] or len(data) != image["size"] or zlib.crc32(data) != crc:
                print(
                    f"{name}: BAD: {len(missing)} missing lines (first {missing[:5]}), "
                    f"{image['bad']} undecodable, {len(data)} of {image['size']} B, "
                    f"crc {zlib.crc32(data):08x} vs {crc:08x}"
                )
            else:
                w, h = image["w"], image["h"]
                base = os.path.join(out_dir, name)
                with open(base + ".pgm", "wb") as f:
                    f.write(b"P5\n%d %d\n255\n" % (w, h) + data)
                png(base + ".png", w, h, data)
                lo, hi = min(data), max(data)
                mean = sum(data) / len(data)
                print(f"{name}: OK {w}x{h}, crc {crc:08x}, min {lo} max {hi} mean {mean:.1f} -> {base}.png")
                ok += 1
            image = None
    if ok == 0:
        sys.exit("no complete still found")


if __name__ == "__main__":
    main()
