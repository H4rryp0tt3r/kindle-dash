#!/bin/bash
# Disposable public keys and images only; never use the user's SSH credentials.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
export DASH_BUILD_IMAGE=${DASH_BUILD_IMAGE:-localhost/dash-build:$(shasum -a 256 "$HERE/Containerfile" | cut -c1-12)}
python3 - "$HERE" "$TMP" <<'PY'
import base64
import importlib.util
from pathlib import Path
import sys
spec = importlib.util.spec_from_file_location("provision", Path(sys.argv[1])/"tools/provision.py")
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)
tmp = Path(sys.argv[2])
blob = b"\x00\x00\x00\x0bssh-ed25519\x00\x00\x00\x20" + bytes(range(32))
key = tmp/"test.pub"
key.write_text("ssh-ed25519 " + base64.b64encode(blob).decode() + " disposable test\n")
assert p.public_key(key).startswith(b"ssh-ed25519 ")
for bad in ["-----BEGIN OPENSSH PRIVATE KEY-----", "ssh-rsa AAAA", "ssh-ed25519 ????", "ssh-ed25519 AAAA", key.read_text()*2]:
    (tmp/"bad.pub").write_text(bad)
    try:
        p.public_key(tmp/"bad.pub")
    except (ValueError, UnicodeError):
        pass
    else:
        raise AssertionError("invalid key accepted")
print("Public-key validation: passed")
PY
source_hash=$(shasum -a 256 "$HERE/artifacts/dash-rootfs.img")
"$HERE/build.sh" provision-ssh "$TMP/test.pub" "$HERE/artifacts/dash-rootfs.img" "$TMP/ssh.img"
[ "$source_hash" = "$(shasum -a 256 "$HERE/artifacts/dash-rootfs.img")" ]
if "$HERE/build.sh" provision-ssh "$TMP/test.pub" "$HERE/artifacts/dash-rootfs.img" "$TMP/ssh.img"; then
    printf 'FAIL: overwrote existing personalised image\n' >&2; exit 1
fi
if "$HERE/build.sh" provision-ssh "$TMP/test.pub" "$TMP/ssh.img" "$TMP/second.img"; then
    printf 'FAIL: reused existing device state\n' >&2; exit 1
fi
[ ! -e "$TMP/second.img" ]
python3 - "$TMP/ssh.img" <<'PY'
from pathlib import Path
import stat
import sys
assert stat.S_IMODE(Path(sys.argv[1]).stat().st_mode) == 0o600
PY
printf 'Provisioning tests: passed (canonical unchanged, read-back verified, reuse rejected)\n'
