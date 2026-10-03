#!/usr/bin/env python3
"""
Build uboot_diags2fastboot.bin from uboot_2009-08-lab126_wario_usb_fastboot.bin.

The lab126 U-Boot takes `bootmode` from IDME, not from its environment: with
bootmode "diags" it runs `bootm 0xE41000` instead of fastboot -- and fastboot is
what would let us change bootmode. Chicken and egg.

Patch: overwrite the 18-byte C string "run bootcmd_diags\0" at file offset
0x1a505 with "fastboot\0" + NUL padding, so the diags branch does
setenv("bootcmd","fastboot") -- byte-identical to what bootmode "main" does. No
eMMC/IDME write; the IVT is untouched, so `SDP: boot -f <patched>` just works.

Mechanism, disassembly and the IDME map: docs/DEVICE.md, Appendix A.
"""

import hashlib
import os
import sys

SRC = "uboot_2009-08-lab126_wario_usb_fastboot.bin"
DST = "uboot_diags2fastboot.bin"

# file offset of the string, and the exact old/new bytes
OFF = 0x1A505
OLD = b"run bootcmd_diags\x00"
NEW = b"fastboot\x00" + b"\x00" * (len(OLD) - len(b"fastboot\x00"))


def main():
    if len(sys.argv) > 1:
        src, dst = sys.argv[1], sys.argv[2]
    else:
        src, dst = SRC, DST

    if not os.path.exists(src):
        sys.exit(f"missing {src} - run this from recovery/")

    with open(src, "rb") as f:
        blob = bytearray(f.read())

    if bytes(blob[OFF:OFF + len(OLD)]) != OLD:
        sys.exit(f"unexpected bytes at {OFF:#x}: {bytes(blob[OFF:OFF+len(OLD)])!r}")

    blob[OFF:OFF + len(OLD)] = NEW

    # sanity: IVT must survive untouched
    ivt = bytes(blob[0x400:0x430])
    assert ivt[:4] == bytes.fromhex("d1002040"), "IVT tag damaged"
    assert blob[0x420:0x424] == bytes.fromhex("00009800"), "boot_data.start damaged"

    with open(dst, "wb") as f:
        f.write(blob)

    ndiff = sum(1 for a, b in zip(open(src, "rb").read(), bytes(blob)) if a != b)
    print(f"{src}\n  md5 {hashlib.md5(open(src,'rb').read()).hexdigest()}")
    print(f"{dst}\n  md5 {hashlib.md5(bytes(blob)).hexdigest()}")
    print(f"  patched {len(OLD)} bytes at file offset {OFF:#x} "
          f"({OFF:#x} -> vaddr 0x{0x00980000 + OFF:08x}): "
          f"{OLD!r} -> {NEW!r}")
    print(f"  {ndiff} differing bytes total (expected <= {len(NEW)})")


if __name__ == "__main__":
    main()