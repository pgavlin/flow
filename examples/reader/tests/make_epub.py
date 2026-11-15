#!/usr/bin/env python3
"""Create a tiny synthetic EPUB for manual/browser smoke tests (stdlib only)."""
from pathlib import Path
import struct
import sys
import zlib
import zipfile


def make_epub(path):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    raw = b"".join(b"\0" + bytes([40, 130, 90]) * 64 for _ in range(32))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 32, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    files = {
        "META-INF/container.xml": '<container><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>',
        "OPS/book.opf": '<package version="2.0"><metadata/><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/><item id="image" href="image.png" media-type="image/png"/></manifest><spine><itemref idref="one"/><itemref idref="two"/></spine></package>',
        "OPS/one.xhtml": '<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><h1 id="start">First chapter</h1><p><a href="two.xhtml#target">Continue to second chapter</a></p><p><img src="image.png" width="256" height="128" alt="Synthetic green rectangle"/></p>' + '<p>Synthetic scrolling paragraph. Nothing in this book is third-party content.</p>' * 45 + '</body></html>',
        "OPS/two.xhtml": '<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><h1 id="target">Second chapter</h1><p>The end.</p><p><a href="one.xhtml#start">Return to first chapter</a></p></body></html>',
        "OPS/image.png": png,
    }
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        for name, data in files.items():
            archive.writestr(name, data, compress_type=zipfile.ZIP_DEFLATED)


if __name__ == "__main__":
    path = Path(sys.argv[1])
    make_epub(path)
    print(path)
