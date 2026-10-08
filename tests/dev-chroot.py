#!/usr/bin/env python3
"""Host orchestration tests; no Kindle access."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("dev_chroot", HERE / "tools/dev-chroot.py")
DEV = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DEV)


class ChrootTests(unittest.TestCase):
    def test_subprocess_arguments_are_one_vector(self):
        with patch.object(DEV.subprocess, "run") as call:
            DEV.run("ssh", "-T", "root@192.168.15.244", capture_output=True)
        call.assert_called_once_with(
            ("ssh", "-T", "root@192.168.15.244"), check=True, capture_output=True)

    def test_help_does_not_access_device_or_build(self):
        result = subprocess.run(
            [sys.executable, str(HERE / "tools/dev-chroot.py"), "--help"],
            capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn("--pubkey", result.stdout)

    def test_private_key_is_not_accepted_as_selector(self):
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / "key"
            key.write_text("-----BEGIN OPENSSH PRIVATE KEY-----\n")
            with patch.object(sys, "argv", ["dev-chroot", "--pubkey", str(key)]), \
                    patch.object(DEV, "run") as call, \
                    patch("sys.stderr"):
                with self.assertRaises(SystemExit) as failure:
                    DEV.main()
                self.assertEqual(failure.exception.code, 2)
                call.assert_not_called()

    def test_device_runner_normal_and_failure_cleanup(self):
        runner = (HERE / "tools/dev-chroot-device.sh").read_text()
        for scenario in ("success", "mount-failure", "unmount-failure", "busy-lock"):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / "candidate"
                root.mkdir()
                (root / ".dash-dev-candidate").touch()
                for name in ("var/run", "var/log", "dev", "proc"):
                    (root / name).mkdir(parents=True, exist_ok=True)
                fake = base / "bin"
                fake.mkdir()
                mounts = base / "mounts"
                mounts.write_text("")
                lock = base / "lock"
                if scenario == "busy-lock":
                    lock.mkdir()
                state = base / "state"
                state.write_text("run")
                calls = base / "calls"
                mock = fake / "mock"
                mock.write_text(r'''#!/bin/bash
set -eu
name=${0##*/}
printf '%s %s\n' "$name" "$*" >> "$CALLS"
case "$name" in
uname) echo 3.0.35-lab126 ;;
id) echo 0 ;;
pgrep) exit 1 ;;
sleep) exit 0 ;;
sv)
    case " $* " in
        *" status "*) printf '%s: panel\n' "$(cat "$STATE")" ;;
        *" down "*) echo down > "$STATE" ;;
        *" up "*) echo run > "$STATE" ;;
    esac ;;
mount)
    [ "$SCENARIO" != mount-failure ] || exit 1
    printf 'source %s bind rw 0 0\n' "${@: -1}" >> "$MOUNTS" ;;
umount)
    [ "$SCENARIO" != unmount-failure ] || exit 1
    python3 -c 'import os,pathlib,sys; p=pathlib.Path(os.environ["MOUNTS"]); p.write_text("".join(x for x in p.read_text().splitlines(True) if x.split()[1]!=sys.argv[1]))' "$1" ;;
chroot)
    if [[ "$*" == *'/service/10-dash/run'* ]]; then
        printf 'mock candidate frame\n' > "$1/var/run/dash.frame"
        exec /bin/sleep 60
    fi ;;
*) exit 1 ;;
esac
''')
                mock.chmod(0o755)
                for name in ("uname", "id", "pgrep", "sleep", "sv", "mount", "umount", "chroot"):
                    (fake / name).symlink_to("mock")
                script = runner.replace("PATH=/bin:/sbin", f"PATH={fake}:/usr/bin:/bin")
                script = script.replace('case "$ROOT" in /tmp/dash-chroot.??????)',
                                        f'case "$ROOT" in "{root}")')
                script = script.replace('tee -a /var/log/dash.log', f'tee -a {base / "host.log"}')
                script = script.replace("/var/run/dash-chroot-lock", str(lock))
                script = script.replace("/proc/mounts", str(mounts))
                path = base / "runner.sh"
                path.write_text(script)
                env = dict(os.environ, CALLS=str(calls), STATE=str(state),
                           MOUNTS=str(mounts), SCENARIO=scenario)
                result = subprocess.run(["bash", str(path), str(root)], env=env,
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0 if scenario == "success" else 1,
                                 result.stdout + result.stderr)
                self.assertEqual(state.read_text().strip(), "run")
                self.assertEqual(root.exists(), scenario == "unmount-failure")
                self.assertEqual(lock.exists(), scenario in ("unmount-failure", "busy-lock"))
                self.assertNotIn("20-usbnet", calls.read_text())
                self.assertNotIn("30-sshd", calls.read_text())


if __name__ == "__main__":
    unittest.main()
