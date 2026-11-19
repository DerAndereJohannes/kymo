"""Packs PNGs into a Windows .ico (one entry per PNG, stored as PNG, which Windows Vista
and newer read): `python3 scripts/ico.py OUT.ico IN.png…`. Used by icons.sh."""

import struct
import sys

out, *pngs = sys.argv[1:]
images = []
for path in pngs:
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"{path} is not a PNG")
    # The IHDR chunk follows the signature: width and height, big-endian.
    width, height = struct.unpack(">II", data[16:24])
    images.append((width, height, data))

header = struct.pack("<HHH", 0, 1, len(images))
offset = len(header) + 16 * len(images)
entries = b""
for width, height, data in images:
    # 256 px is written as 0.
    entries += struct.pack("<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
with open(out, "wb") as f:
    f.write(header + entries + b"".join(data for _, _, data in images))
