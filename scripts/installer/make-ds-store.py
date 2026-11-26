#!/usr/bin/env python3
"""Zero-dependency macOS DMG .DS_Store generator (no Finder, no AppleScript, CI-safe).

Writes a clean Bud1 B-tree .DS_Store for a mounted RW DMG volume so Finder opens it
with exact window dimensions (680x420), hidden toolbar/sidebar/statusbar, Retina TIFF
background image (via classic Alias v2), and exact icon coordinates for HIVECAST.app
and Applications.
"""
import argparse
import os
import plistlib
import struct
from pathlib import Path

HFS_EPOCH_OFFSET = 2082844800  # seconds between 1904-01-01 and 1970-01-01


def hfs_timestamp(unix_ts: float) -> int:
    val = int(unix_ts) + HFS_EPOCH_OFFSET
    return max(0, min(0xFFFFFFFF, val))


def build_alias_v2(vol_path: Path, vol_name: str, bg_rel_dir: str, bg_filename: str) -> bytes:
    """Build a classic macOS Alias v2 record pointing to <vol>/<bg_rel_dir>/<bg_filename>."""
    bg_dir = vol_path / bg_rel_dir
    bg_file = bg_dir / bg_filename
    vol_st = os.stat(vol_path)
    dir_st = os.stat(bg_dir)
    file_st = os.stat(bg_file)

    vol_birth = getattr(vol_st, "st_birthtime", vol_st.st_mtime)
    file_birth = getattr(file_st, "st_birthtime", file_st.st_mtime)

    vol_ascii = vol_name.encode("ascii", errors="replace")[:27]
    target_ascii = bg_filename.encode("ascii", errors="replace")[:63]

    def tagged(tag: int, payload: bytes) -> bytes:
        chunk = struct.pack(">hH", tag, len(payload)) + payload
        if len(payload) % 2 == 1:
            chunk += b"\x00"
        return chunk

    carbon_path = f"{vol_name}:{bg_rel_dir}:{bg_filename}".encode("utf-8")
    posix_rel = f"{bg_rel_dir}/{bg_filename}".encode("utf-8")
    posix_vol = f"/Volumes/{vol_name}".encode("utf-8")
    target_u16 = bg_filename.encode("utf-16-be")
    vol_u16 = vol_name.encode("utf-16-be")

    extras = b"".join([
        tagged(0, bg_rel_dir.encode("utf-8")),
        tagged(1, struct.pack(">I", dir_st.st_ino & 0xFFFFFFFF)),
        tagged(2, carbon_path),
        tagged(14, struct.pack(">H", len(bg_filename)) + target_u16),
        tagged(15, struct.pack(">H", len(vol_name)) + vol_u16),
        tagged(18, posix_rel),
        tagged(19, posix_vol),
        struct.pack(">hH", -1, 0),
    ])

    total_len = 150 + len(extras)
    header = struct.pack(
        ">IHhhB27sI2shIB63sII4s4shhIH10s",
        0,                                      # app info
        total_len,                              # record size
        2,                                      # alias version 2
        0,                                      # kind = file
        len(vol_ascii),
        vol_ascii.ljust(27, b"\x00"),
        hfs_timestamp(vol_birth),
        b"H+",                                  # HFS+ signature
        5,                                      # disk image / other
        dir_st.st_ino & 0xFFFFFFFF,             # parent folder CNID
        len(target_ascii),
        target_ascii.ljust(63, b"\x00"),
        file_st.st_ino & 0xFFFFFFFF,            # file CNID
        hfs_timestamp(file_birth),
        b"\x00\x00\x00\x00",                    # file type
        b"\x00\x00\x00\x00",                    # creator
        -1,                                     # nlvl_from
        -1,                                     # nlvl_to
        0x00000D02,                             # volume attributes
        0,                                      # fs_id
        b"\x00" * 10,                           # reserved
    )
    assert len(header) == 150
    return header + extras


def encode_record(filename: str, rec_type: bytes, data_type: bytes, payload: bytes) -> bytes:
    name_u16 = filename.encode("utf-16-be")
    out = bytearray()
    out += struct.pack(">I", len(filename))
    out += name_u16
    out += rec_type
    out += data_type
    if data_type == b"blob":
        out += struct.pack(">I", len(payload))
        out += payload
    elif data_type == b"type":
        assert len(payload) == 4
        out += payload
    else:
        raise ValueError(f"unsupported data_type: {data_type!r}")
    return bytes(out)


def write_ds_store(
    vol_path: Path,
    vol_name: str,
    bg_rel_dir: str,
    bg_filename: str,
    width: int,
    height: int,
    icon_size: float,
    app_pos: tuple[int, int],
    apps_pos: tuple[int, int],
) -> Path:
    alias_bytes = build_alias_v2(vol_path, vol_name, bg_rel_dir, bg_filename)

    bwsp = plistlib.dumps(
        {
            "ContainerShowSidebar": False,
            "ShowPathbar": False,
            "ShowSidebar": False,
            "ShowStatusBar": False,
            "ShowTabView": False,
            "ShowToolbar": False,
            "SidebarWidth": 0,
            "WindowBounds": f"{{{{220, 180}}, {{{width}, {height}}}}}",
        },
        fmt=plistlib.FMT_BINARY,
    )

    icvp = plistlib.dumps(
        {
            "arrangeBy": "none",
            "backgroundColorBlue": 1.0,
            "backgroundColorGreen": 1.0,
            "backgroundColorRed": 1.0,
            "backgroundImageAlias": alias_bytes,
            "backgroundType": 2,
            "gridOffsetX": 0.0,
            "gridOffsetY": 0.0,
            "gridSpacing": 100.0,
            "iconSize": float(icon_size),
            "labelOnBottom": True,
            "showIconPreview": True,
            "showItemInfo": False,
            "textSize": 13.0,
            "viewOptionsVersion": 1,
        },
        fmt=plistlib.FMT_BINARY,
    )

    iloc_app = struct.pack(">II8s", app_pos[0], app_pos[1], b"\xff\xff\xff\xff\xff\xff\x00\x00")
    iloc_apps = struct.pack(">II8s", apps_pos[0], apps_pos[1], b"\xff\xff\xff\xff\xff\xff\x00\x00")

    entries = [
        (".", b"bwsp", b"blob", bwsp),
        (".", b"icvp", b"blob", icvp),
        (".", b"vstl", b"type", b"icnv"),
        ("Applications", b"Iloc", b"blob", iloc_apps),
        ("HIVECAST.app", b"Iloc", b"blob", iloc_app),
    ]
    # DS_Store B-tree requires strict case-insensitive filename + record_type ordering
    entries.sort(key=lambda item: (item[0].lower(), item[1]))

    leaf = bytearray()
    leaf += struct.pack(">II", 0, len(entries))  # mode=0 (leaf), record count
    for name, rtype, dtype, payload in entries:
        leaf += encode_record(name, rtype, dtype, payload)
    assert len(leaf) <= 4096, f"leaf overflow: {len(leaf)} bytes"

    # Buddy allocator layout (offsets relative to file byte 4):
    #   Block 1 (DSDB header): offset 0x0040, size 64   (log2 = 6)
    #   Block 0 (Allocator):   offset 0x1000, size 2048 (log2 = 11)
    #   Block 2 (B-tree leaf): offset 0x2000, size 4096 (log2 = 12)
    alloc_offset = 0x1000
    alloc_size = 2048
    dsdb_offset = 0x0040
    leaf_offset = 0x2000

    buf = bytearray(4 + leaf_offset + 4096)

    # Header (36 bytes)
    struct.pack_into(">I4sIII16s", buf, 0, 1, b"Bud1", alloc_offset, alloc_size, alloc_offset, b"\x00" * 16)

    # DSDB superblock at 4 + dsdb_offset
    struct.pack_into(">IIIII", buf, 4 + dsdb_offset, 2, 0, len(entries), 1, 0x1000)

    # Allocator block at 4 + alloc_offset
    alloc = bytearray()
    alloc += struct.pack(">II", 3, 0)  # 3 blocks
    block_addrs = [alloc_offset | 11, dsdb_offset | 6, leaf_offset | 12]
    block_addrs.extend([0] * (256 - len(block_addrs)))
    for addr in block_addrs:
        alloc += struct.pack(">I", addr)
    # TOC: 1 entry "DSDB" -> block 1
    alloc += struct.pack(">IB", 1, 4) + b"DSDB" + struct.pack(">I", 1)
    # 32 free lists (all empty)
    alloc += b"\x00\x00\x00\x00" * 32
    assert len(alloc) <= alloc_size
    buf[4 + alloc_offset : 4 + alloc_offset + len(alloc)] = alloc

    # Leaf node at 4 + leaf_offset
    buf[4 + leaf_offset : 4 + leaf_offset + len(leaf)] = leaf

    out_path = vol_path / ".DS_Store"
    out_path.write_bytes(buf)
    return out_path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--volume", required=True, help="Mounted RW DMG volume path")
    parser.add_argument("--name", default="HIVECAST")
    parser.add_argument("--bg-dir", default=".background")
    parser.add_argument("--bg-file", default="bg.tiff")
    parser.add_argument("--width", type=int, default=680)
    parser.add_argument("--height", type=int, default=420)
    parser.add_argument("--icon-size", type=float, default=120.0)
    parser.add_argument("--app-x", type=int, default=180)
    parser.add_argument("--app-y", type=int, default=224)
    parser.add_argument("--apps-x", type=int, default=500)
    parser.add_argument("--apps-y", type=int, default=224)
    args = parser.parse_args()

    out = write_ds_store(
        Path(args.volume),
        args.name,
        args.bg_dir,
        args.bg_file,
        args.width,
        args.height,
        args.icon_size,
        (args.app_x, args.app_y),
        (args.apps_x, args.apps_y),
    )
    print(f"ds_store: {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
