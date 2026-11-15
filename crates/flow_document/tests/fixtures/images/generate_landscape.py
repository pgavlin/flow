#!/usr/bin/env python3
"""Generate the original, synthetic landscape.png fixture using only Python stdlib."""

from pathlib import Path
import struct
import zlib

WIDTH, HEIGHT = 960, 480


def chunk(kind, data):
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))


def pixel(x, y):
    if (x - 740) ** 2 + (y - 100) ** 2 < 45 ** 2:
        return (245, 195, 70)
    if y >= 365 - (x % 320) // 4:
        return (40, 100, 70)
    if y >= 300 - abs(x - 380) // 5:
        return (80, 140, 110)
    return (145, 195, 225)


def main():
    rows = b"".join(
        b"\0" + bytes(channel for x in range(WIDTH) for channel in pixel(x, y))
        for y in range(HEIGHT)
    )
    header = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0)
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
           + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))
    Path(__file__).with_name("landscape.png").write_bytes(png)


if __name__ == "__main__":
    main()
