#!/usr/bin/env python3
"""Decode still dumps from a poc-p4-eth log capture into PGM and PNG files.

The firmware prints (see `src/camera.rs`, `dump_still`):

    @@IMG <name> <width> <height> <bytes> [<bits per pixel>, default 8]
    @@D <line index, hex> <base64 of up to 96 bytes>
    ...
    @@END <crc32 hex> <line count>

The bytes are the frame as it sits in memory. RAW8: one byte per pixel, written as an 8-bit PGM
and PNG. RAW10: packed CSI-2 RAW10, per 4 pixels 4 bytes with bits 9..2 of pixels 0..3, then
one byte with the four 2-bit remainders (pixel 0 in bits 1..0), a row being width*5/4 bytes;
written as a 16-bit PGM (maxval 1023) and an 8-bit PNG (the top 8 bits).

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


def unpack_raw10(data, width, height):
    """Unpacks CSI-2 RAW10 into a list of 10-bit values, row by row."""
    row_bytes = width * 5 // 4
    pixels = []
    for y in range(height):
        row = data[y * row_bytes:(y + 1) * row_bytes]
        for i in range(0, row_bytes, 5):
            low = row[i + 4]
            pixels.extend((row[i + k] << 2) | ((low >> (2 * k)) & 3) for k in range(4))
    return pixels


def write_image(base, width, height, bits, data):
    """Writes `base`.pgm and `base`.png; returns (min, max, mean) in the sensor's bit depth."""
    if bits == 10:
        values = unpack_raw10(data, width, height)
        with open(base + ".pgm", "wb") as f:
            f.write(b"P5\n%d %d\n1023\n" % (width, height))
            f.write(b"".join(struct.pack(">H", v) for v in values))
        png(base + ".png", width, height, bytes(v >> 2 for v in values))
    else:
        values = data
        with open(base + ".pgm", "wb") as f:
            f.write(b"P5\n%d %d\n255\n" % (width, height) + data)
        png(base + ".png", width, height, data)
    return min(values), max(values), sum(values) / len(values)


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
        if tag == "@@IMG" and len(fields) in (5, 6):
            name, width, height, size = fields[1], int(fields[2]), int(fields[3]), int(fields[4])
            bits = int(fields[5]) if len(fields) == 6 else 8
            image = {
                "name": name, "w": width, "h": height, "size": size, "bits": bits,
                "parts": {}, "bad": 0,
            }
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
            elif image["bits"] not in (8, 10) or len(data) != image["w"] * image["h"] * image["bits"] // 8:
                print(f"{name}: BAD: {len(data)} B do not hold {image['w']}x{image['h']} RAW{image['bits']}")
            else:
                w, h, bits = image["w"], image["h"], image["bits"]
                base = os.path.join(out_dir, name)
                lo, hi, mean = write_image(base, w, h, bits, data)
                print(
                    f"{name}: OK {w}x{h} RAW{bits}, crc {crc:08x}, min {lo} max {hi} mean {mean:.1f}"
                    f" -> {base}.png"
                )
                ok += 1
            image = None
    if ok == 0:
        sys.exit("no complete still found")


if __name__ == "__main__":
    main()
