#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.9"
# dependencies = [
#     "pyusb==1.3.1",
#     "libusb-package==1.0.30.0",
# ]
# ///
"""Send bootmode + reboot to the lab126 Kindle fastboot U-Boot.
VID 0x1949, PID 0xd0e0. Raw PyUSB transport with bundled libusb.

Usage:
    uv run fastboot-setvar-reboot.py [mode]   # main|diags|prod; default diags
"""
import argparse
import sys
import libusb_package
import usb.core
import usb.util

VID, PID = 0x1949, 0xd0e0


def find_device():
    backend = libusb_package.get_libusb1_backend()
    if backend is None:
        print("ERROR: bundled libusb backend could not be loaded.")
        return None
    dev = usb.core.find(idVendor=VID, idProduct=PID, backend=backend)
    if dev is None:
        print(f"ERROR: device {VID:04x}:{PID:04x} not found.")
        return None
    dev.set_configuration()
    cfg = dev.get_active_configuration()
    intf = cfg[(0, 0)]
    ep_out = usb.util.find_descriptor(intf, custom_match=lambda e:
        usb.util.endpoint_direction(e.bEndpointAddress) == usb.util.ENDPOINT_OUT)
    ep_in = usb.util.find_descriptor(intf, custom_match=lambda e:
        usb.util.endpoint_direction(e.bEndpointAddress) == usb.util.ENDPOINT_IN)
    if ep_out is None or ep_in is None:
        print("ERROR: could not find bulk endpoints")
        return None
    return dev, ep_out, ep_in


def fb_cmd(ep_out, ep_in, cmd):
    print(f">> {cmd}")
    ep_out.write(cmd.encode())
    while True:
        try:
            resp = bytes(ep_in.read(64, timeout=5000))
        except usb.core.USBError:
            print("<< (no response)")
            return
        s = resp.decode(errors="replace").rstrip("\x00").strip()
        if s:
            print(f"<< {s}")
        if s.startswith("OKAY") or s.startswith("FAIL"):
            return


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", nargs="?", choices=("main", "diags", "prod"), default="diags")
    args = parser.parse_args()

    dev = find_device()
    if dev is None:
        sys.exit(1)
    _, ep_out, ep_in = dev
    try:
        fb_cmd(ep_out, ep_in, f"setvar bootmode {args.mode}")
        fb_cmd(ep_out, ep_in, "reboot")
    finally:
        try:
            usb.util.dispose_resources(dev)
        except Exception:
            pass


if __name__ == "__main__":
    main()