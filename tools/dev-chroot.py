#!/usr/bin/env python3
"""Run a ten-second panel test from a disposable rootfs over USB SSH."""
import argparse
import hashlib
from pathlib import Path
import re
import shlex
import subprocess
import tarfile
import tempfile

HERE = Path(__file__).resolve().parent.parent


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pubkey", required=True, type=Path,
                        help="public-key selector for the SSH agent (private key stays in agent)")
    args = parser.parse_args()
    pubkey = args.pubkey.expanduser().resolve()
    if not pubkey.is_file() or not pubkey.read_text().startswith("ssh-ed25519 "):
        parser.error("--pubkey must be an Ed25519 public-key file")
    image = HERE / "artifacts/dash-rootfs.img"
    if not image.is_file():
        parser.error("run make build first")
    # Use the same pinned build image as make, and refuse environment drift.
    run("make", "env", cwd=HERE)
    tag = hashlib.sha256((HERE / "Containerfile").read_bytes()).hexdigest()[:12]
    ssh = ["ssh", "-T", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
           "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=10",
           "-i", str(pubkey), "root@192.168.15.244"]
    revision = run("git", "rev-parse", "--short", "HEAD", cwd=HERE,
                   capture_output=True, text=True).stdout.strip()
    if run("git", "status", "--porcelain", cwd=HERE,
           capture_output=True, text=True).stdout:
        revision += "-dirty"
    with tempfile.TemporaryDirectory(prefix="dash-chroot-") as workspace:
        archive = Path(workspace) / "candidate.tgz"
        # Extract the packaged artifact, not build/rootfs; root metadata is normalized
        # exactly as in the build. Only the disposable candidate gets a dev label.
        script = r'''set -euo pipefail
e2fsck -fn /input/rootfs.img >&2
mkdir /candidate
debugfs -R "rdump / /candidate" /input/rootfs.img >&2
test -x /candidate/bin/screen && test -x /candidate/bin/dash-status
for binary in screen dash-status; do
    arm-linux-gnueabi-readelf -h /candidate/bin/$binary | grep -q 'Machine:.*ARM'
    ! arm-linux-gnueabi-readelf -l /candidate/bin/$binary | grep -q INTERP
    ! arm-linux-gnueabi-readelf -d /candidate/bin/$binary | grep -q NEEDED
    arm-linux-gnueabi-objdump -d /candidate/bin/$binary > /tmp/disassembly
    ! grep -Eq '\b(vfma|vfms|vfnma|vfnms)\.' /tmp/disassembly
done
test -z "$(find /candidate/etc/dropbear /candidate/var/lib/dash-ssh -mindepth 1 -print -quit)"
chown -R 0:0 /candidate
cp /input/runner.sh /candidate/.dash-dev-run.sh
chmod 700 /candidate/.dash-dev-run.sh
printf 'Dash development candidate\n' > /candidate/.dash-dev-candidate
printf ' + CHROOT %s\n' "$REVISION" >> /candidate/etc/dash-release
# Keep the label on one line.
tr '\n' ' ' < /candidate/etc/dash-release > /candidate/etc/dash-release.new
mv /candidate/etc/dash-release.new /candidate/etc/dash-release
tar -C /candidate -czf - .
'''
        with archive.open("wb") as output:
            run("podman", "run", "--rm", "--pull", "never",
                "-v", f"{image}:/input/rootfs.img:ro",
                "-v", f"{HERE / 'tools/dev-chroot-device.sh'}:/input/runner.sh:ro",
                "-e", f"REVISION={revision}", f"localhost/dash-build:{tag}",
                "bash", "-c", script, stdout=output)
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        # Check the unpacked size too: compressed size alone is not a space check.
        with tarfile.open(archive, "r:gz") as contents:
            required_kb = (sum((m.size + 4095) // 4096 * 4096 + 4096
                               for m in contents) + archive.stat().st_size) // 1024 + 4096
        preflight = (
            "set -eu; [ \"$(uname -r)\" = 3.0.35-lab126 ]; "
            "[ \"$(id -u)\" = 0 ]; [ ! -e /var/run/dash-chroot-lock ]; "
            "sv status /service/10-dash | grep -q '^run:'; "
            f"[ \"$(df -k /tmp | tail -n 1 | awk '{{print $4}}')\" -gt {required_kb} ]; "
            "mktemp -d /tmp/dash-chroot.XXXXXX"
        )
        root = run(*ssh, preflight, capture_output=True, text=True).stdout.strip()
        if not re.fullmatch(r"/tmp/dash-chroot\.[A-Za-z0-9]{6}", root):
            raise RuntimeError(f"unexpected candidate directory: {root!r}")
        qroot = shlex.quote(root)
        command = (
            f"set -eu; umask 077; ROOT={qroot}; "
            'trap \'rm -f "$ROOT/archive.tgz"\' EXIT; '
            'cat > "$ROOT/archive.tgz"; '
            f'printf "%s  %s\\n" {checksum} "$ROOT/archive.tgz" | sha256sum -c -; '
            'tar -xzf "$ROOT/archive.tgz" -C "$ROOT"; chmod 700 "$ROOT"; '
            'exec /bin/busybox sh "$ROOT/.dash-dev-run.sh" "$ROOT"'
        )
        print(f"Candidate {revision}: {archive.stat().st_size} bytes → {root}", flush=True)
        try:
            with archive.open("rb") as source:
                run(*ssh, command, stdin=source)
        except (subprocess.CalledProcessError, KeyboardInterrupt):
            print(f"Test did not complete. Inspect {root} and /var/log/dash.log; "
                  "do not delete a directory with active mounts.", flush=True)
            raise
        run(*ssh, "sv status /service/10-dash /service/20-usbnet /service/30-sshd")


if __name__ == "__main__":
    main()
