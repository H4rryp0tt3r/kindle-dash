#!/usr/bin/env python3
"""Send bootmode + reboot to the lab126 Kindle fastboot U-Boot.
VID 0x1949, PID 0xd0e0. Raw pyusb transport.

Usage:
    python3.14 fastboot-setvar-reboot.py [mode]   # mode = main|diags|prod (default deps)
"""
import sys
import usb.core
import usb.util

VID, PID = 0x1949, 0xd0e0


def find_device():
    dev = usb.core.find(idVendor=VID, idProduct=PID)
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
    if len(sys.argv) < 2:
        mode = "diags"
    else:
        mode = sys.argv[1]

    dev = find_device()
    if dev is None:
        sys.exit(1)
    _, ep_out, ep_in = dev
    try:
        fb_cmd(ep_out, ep_in, f"setvar bootmode {mode}")
        fb_cmd(ep_out, ep_in, "reboot")
    finally:
        try:
            usb.util.dispose_resources(dev)
        except Exception:
            pass


if __name__ == "__main__":
    main()