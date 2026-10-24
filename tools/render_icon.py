#!/usr/bin/env python3
"""Render snap/gui/xbtune.svg to the app's other icon files.

- crates/xbtune-app/assets/icon-128.rgba: window/taskbar icon (raw RGBA).
- crates/xbtune-app/assets/xbtune.ico: Windows .exe icon (PNG entries).

Run from the repo root after changing the SVG: python3 -I tools/render_icon.py
Needs GdkPixbuf with its SVG loader (Ubuntu: gir1.2-gdkpixbuf-2.0, librsvg2-common).
"""
import struct

import gi

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf  # noqa: E402

SVG = "snap/gui/xbtune.svg"
ASSETS = "crates/xbtune-app/assets"


def render(size):
    pb = GdkPixbuf.Pixbuf.new_from_file_at_size(SVG, size, size)
    assert pb.get_has_alpha() and pb.get_n_channels() == 4
    return pb


pb = render(128)
px, rs = pb.get_pixels(), pb.get_rowstride()
with open(f"{ASSETS}/icon-128.rgba", "wb") as f:
    f.write(b"".join(px[y * rs : y * rs + 128 * 4] for y in range(128)))

pngs = []
for size in (16, 24, 32, 48, 64, 256):
    ok, data = render(size).save_to_bufferv("png", [], [])
    assert ok
    pngs.append((size, data))
header = struct.pack("<HHH", 0, 1, len(pngs))
offset = len(header) + 16 * len(pngs)
entries, blobs = b"", b""
for size, data in pngs:
    dim = 0 if size == 256 else size
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset + len(blobs))
    blobs += data
with open(f"{ASSETS}/xbtune.ico", "wb") as f:
    f.write(header + entries + blobs)
