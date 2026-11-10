#!/usr/bin/env python3
"""Check the board's JPEG files against their source pixels and a reference encoder.

`tools/still.py <log> <dir>` writes, from a `JPEG_PLAN` log, `<what>-<w>x<h>-q<q>.jpg` (the
board's files) and `<what>-<w>x<h>.pgm` (the pixels they were encoded from). For each JPEG this
tool:

1. parses the markers: SOI first, a baseline SOF0 with 8-bit precision, one component and the
   source's size, no other SOF, EOI last;
2. decodes it with libjpeg-turbo's `djpeg` (an independent decoder) and computes the PSNR
   against the source;
3. encodes the source with `cjpeg -grayscale -baseline -quality <q>` (libjpeg-turbo: the same
   IJG quality scaling and Annex K Huffman tables, integer DCT), decodes that, and prints its
   size and PSNR next to the board's;
4. writes `<name>.decoded.png` for a look at the board's picture.

Usage (djpeg/cjpeg from nixpkgs):
    nix shell nixpkgs#libjpeg_turbo -c python3 tools/jpeg_check.py <dir>
Only the Python standard library is used besides the two programs.
"""

import math
import os
import re
import struct
import subprocess
import sys
import tempfile
import zlib

NAME = re.compile(r"^(?P<src>.+-(?P<w>\d+)x(?P<h>\d+))-q(?P<q>\d+)\.jpg$")


def read_pgm(path):
    with open(path, "rb") as f:
        data = f.read()
    # P5\n<w> <h>\n<max>\n
    parts = data.split(b"\n", 3)
    assert parts[0] == b"P5", path
    w, h = (int(v) for v in parts[1].split())
    assert int(parts[2]) == 255, path
    return w, h, parts[3][: w * h]


def png(path, w, h, pixels):
    raw = b"".join(b"\x00" + pixels[y * w:(y + 1) * w] for y in range(h))

    def chunk(kind, body):
        b = kind + body
        return struct.pack(">I", len(body)) + b + struct.pack(">I", zlib.crc32(b))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        f.write(chunk(b"IEND", b""))


def markers(data):
    """Returns (problems, sof) after walking the header segments up to SOS."""
    problems = []
    if data[:2] != b"\xff\xd8":
        problems.append("no SOI")
    if data[-2:] != b"\xff\xd9":
        problems.append("no EOI at the end")
    i, sof, sofs = 2, None, []
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            problems.append(f"no marker at {i}")
            break
        m = data[i + 1]
        seg = struct.unpack(">H", data[i + 2:i + 4])[0]
        if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
            sofs.append(m)
            p, h, w, n = data[i + 4], *struct.unpack(">HH", data[i + 5:i + 9]), data[i + 9]
            sof = (m, p, w, h, n)
        if m == 0xDA:
            break
        i += 2 + seg
    if sofs != [0xC0]:
        problems.append(f"SOF markers {[hex(s) for s in sofs]}, want only SOF0 (baseline)")
    return problems, sof


def psnr(a, b):
    se = sum((x - y) * (x - y) for x, y in zip(a, b))
    if se == 0:
        return float("inf")
    mse = se / len(a)
    return 10 * math.log10(255 * 255 / mse)


def djpeg(path, tmp):
    out = os.path.join(tmp, "d.pgm")
    subprocess.run(["djpeg", "-grayscale", "-pnm", "-outfile", out, path], check=True)
    return read_pgm(out)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    d = sys.argv[1]
    rows, bad = [], 0
    with tempfile.TemporaryDirectory() as tmp:
        for name in sorted(os.listdir(d)):
            m = NAME.match(name)
            if not m:
                continue
            w, h, q = int(m["w"]), int(m["h"]), int(m["q"])
            src_path = os.path.join(d, m["src"] + ".pgm")
            path = os.path.join(d, name)
            with open(path, "rb") as f:
                data = f.read()
            problems, sof = markers(data)
            if sof is None or sof[1:] != (8, w, h, 1):
                problems.append(f"SOF {sof}, want precision 8, {w}x{h}, 1 component")
            try:
                dw, dh, dec = djpeg(path, tmp)
                if (dw, dh) != (w, h):
                    problems.append(f"decoded {dw}x{dh}")
                png(os.path.join(d, name[:-4] + ".decoded.png"), dw, dh, dec)
            except subprocess.CalledProcessError as e:
                problems.append(f"djpeg failed: {e}")
                dec = None
            p_board = p_ref = None
            ref_len = None
            if os.path.exists(src_path) and dec is not None:
                sw, sh, src = read_pgm(src_path)
                p_board = psnr(src, dec)
                ref = os.path.join(tmp, "r.jpg")
                subprocess.run(
                    ["cjpeg", "-grayscale", "-baseline", "-quality", str(q), "-outfile", ref,
                     src_path],
                    check=True,
                )
                ref_len = os.path.getsize(ref)
                _, _, rdec = djpeg(ref, tmp)
                p_ref = psnr(src, rdec)
            bad += bool(problems)
            rows.append((name, len(data), ref_len, p_board, p_ref, problems))
    print(f"{'file':34} {'board B':>9} {'cjpeg B':>9} {'size %':>7} {'board dB':>9} {'cjpeg dB':>9}  check")
    for name, n, rn, pb, pr, problems in rows:
        ratio = f"{100 * n / rn:6.1f}" if rn else "     -"
        fb = f"{pb:9.2f}" if pb is not None else "        -"
        fr = f"{pr:9.2f}" if pr is not None else "        -"
        print(f"{name:34} {n:9d} {rn or 0:9d} {ratio:>7} {fb} {fr}  {'; '.join(problems) or 'OK'}")
    if bad:
        sys.exit(f"{bad} file(s) with problems")


if __name__ == "__main__":
    main()
