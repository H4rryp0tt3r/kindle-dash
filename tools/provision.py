#!/usr/bin/env python3
"""Personalise one copy of a Dash image; never place secrets in the release tree."""
import argparse
import base64
import hashlib
import os
from pathlib import Path
import secrets
import subprocess
import tarfile
import tempfile


def public_key(path):
    with Path(path).open("rb") as key:
        raw = key.read(4097)
    if len(raw) > 4096:
        raise ValueError("public key is too large")
    text = raw.decode("ascii").strip()
    parts = text.split()
    if len(parts) < 2 or parts[0] != "ssh-ed25519" or "\n" in text or "\r" in text:
        raise ValueError("supply one OpenSSH Ed25519 public key, not a private key")
    blob = base64.b64decode(parts[1], validate=True)
    expected = b"\x00\x00\x00\x0bssh-ed25519\x00\x00\x00\x20"
    if len(blob) != 51 or not blob.startswith(expected):
        raise ValueError("invalid Ed25519 public-key encoding")
    # Comments are unnecessary and could contain control characters.
    return f"ssh-ed25519 {parts[1]}\n".encode("ascii")


def provision(key, source, destination, image):
    source, destination = Path(source).absolute(), Path(destination).absolute()
    if not source.is_file() or source.is_symlink() or source.stat().st_size != 64 * 1024 * 1024:
        raise ValueError("source must be a 64 MiB rootfs image file")
    if destination.exists() or destination.is_symlink() or source.resolve() == destination.resolve():
        raise ValueError("output must not exist; canonical input is never overwritten")
    auth = public_key(key)
    script = Path(__file__).with_name("provision-image.sh")
    # Both directories are owner-only, including the temporary personalised image.
    with tempfile.TemporaryDirectory(prefix="dash-ssh-") as scratch:
        scratch = Path(scratch)
        with tarfile.open(scratch / "input.tar", "w") as archive:
            for name, data in (("seed", secrets.token_bytes(64)), ("authorized_keys", auth),
                               ("provision-image.sh", script.read_bytes())):
                path = scratch / name
                path.write_bytes(data)
                path.chmod(0o600)
                archive.add(path, arcname=name)
            archive.add(source, arcname="rootfs.img")
        fd, temporary = tempfile.mkstemp(prefix=".dash-ssh-", dir=destination.parent)
        try:
            with os.fdopen(fd, "wb") as out, (scratch / "input.tar").open("rb") as inp:
                subprocess.run(["podman", "run", "-i", "--rm", "--pull", "never", image,
                                "bash", "-c", "set -euo pipefail; umask 077; mkdir /s; tar -C /s -xf -; bash /s/provision-image.sh"],
                               stdin=inp, stdout=out, check=True)
                out.flush()
                os.fsync(out.fileno())
            # Link without overwrite, even if another process created output meanwhile.
            os.link(temporary, destination)
        finally:
            os.unlink(temporary)
    print(f"Personalised image: {destination}")
    print(f"SHA256: {hashlib.sha256(destination.read_bytes()).hexdigest()}")
    print("PRIVATE DEVICE STATE: do not publish or install this copy on multiple devices.")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("public_key")
    ap.add_argument("source")
    ap.add_argument("output")
    ap.add_argument("--image", required=True)
    args = ap.parse_args()
    try:
        provision(args.public_key, args.source, args.output, args.image)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        ap.exit(1, f"provision-ssh: {error}\n")


if __name__ == "__main__":
    main()
