#!/bin/busybox sh
# Run only the candidate panel service. Installed USB/SSH remain the lifeline.
set -eu
PATH=/bin:/sbin
export PATH
ROOT=${1:?candidate directory required}
case "$ROOT" in /tmp/dash-chroot.??????) ;; *) exit 2 ;; esac
[ -d "$ROOT" ] && [ ! -L "$ROOT" ] && [ -f "$ROOT/.dash-dev-candidate" ] || exit 2

stopped=0
locked=0
child=
mounted_proc=0
mounted_fb=0
mounted_null=0
say() {
	printf '%ss  dev-chroot: %s\n' "$(cut -d. -f1 /proc/uptime)" "$*" | tee -a /var/log/dash.log
}
# Never start another panel owner while a renderer is still inside a kernel call.
quiet_panel() {
	n=0
	while pgrep -x screen >/dev/null; do
		[ "$n" -lt 10 ] || return 1
		sleep 1
		n=$((n + 1))
	done
}
cleanup() {
	rc=$?
	trap - EXIT HUP INT TERM
	if [ -n "$child" ]; then
		kill -TERM "$child" 2>/dev/null || :
		n=0
		while kill -0 "$child" 2>/dev/null; do
			if [ "$n" -ge 10 ]; then
				say "candidate still running; panel left stopped, retained at $ROOT"
				exit 1
			fi
			sleep 1
			n=$((n + 1))
		done
		wait "$child" 2>/dev/null || :
	fi
	if [ "$stopped" -eq 1 ] && ! quiet_panel; then
		say "renderer still running; panel left stopped, candidate retained at $ROOT"
		exit 1
	fi
	if [ "$stopped" -eq 1 ]; then
		say "restoring installed panel service"
		sv -w 10 up /service/10-dash || rc=1
		sv status /service/10-dash | grep -q '^run:' || rc=1
	fi
	[ ! -f "$ROOT/var/log/dash.log" ] || cat "$ROOT/var/log/dash.log"
	[ "$mounted_null" -eq 0 ] || umount "$ROOT/dev/null" || rc=1
	[ "$mounted_fb" -eq 0 ] || umount "$ROOT/dev/fb0" || rc=1
	[ "$mounted_proc" -eq 0 ] || umount "$ROOT/proc" || rc=1
	# A failed unmount must never turn cleanup into deletion through a bind mount.
	if ! awk -v root="$ROOT" 'index($2, root "/") == 1 || $2 == root { mounted=1 } END { exit mounted ? 1 : 0 }' /proc/mounts; then
		say "candidate still mounted; retained at $ROOT"
		exit 1
	fi
	rm -rf "$ROOT"
	[ "$locked" -eq 0 ] || rmdir /var/run/dash-chroot-lock || rc=1
	say "finished (rc=$rc); USB/SSH untouched"
	exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

[ "$(uname -r)" = 3.0.35-lab126 ] || exit 1
[ "$(id -u)" = 0 ] || exit 1
# Fail before touching the panel if another test or unexpected owner is present.
mkdir /var/run/dash-chroot-lock || { printf 'another chroot test is active\n' >&2; exit 1; }
locked=1
sv status /service/10-dash | grep -q '^run:' || exit 1
quiet_panel || exit 1
chroot "$ROOT" /bin/busybox --install -s /bin
mkdir -p "$ROOT/var/run" "$ROOT/var/log"
# Snapshot public status only; no live runtime directory or credentials are shared.
for name in dash-usbnet.status dash-sshd.status; do
	[ ! -f "/var/run/$name" ] || cp "/var/run/$name" "$ROOT/var/run/$name"
done
printf '0s stage1: start, mounts up (development chroot, not a boot)\n' >"$ROOT/var/log/dash.log"
printf '0s dev-chroot: candidate userspace on installed kernel\n' >>"$ROOT/var/log/dash.log"
: >"$ROOT/dev/fb0"
: >"$ROOT/dev/null"
say "binding proc and panel devices for $ROOT"
mount -o bind /proc "$ROOT/proc"
mounted_proc=1
mount -o bind /dev/fb0 "$ROOT/dev/fb0"
mounted_fb=1
mount -o bind /dev/null "$ROOT/dev/null"
mounted_null=1
chroot "$ROOT" /bin/busybox true
say "stopping installed panel for a 10-second candidate test"
stopped=1
sv -w 10 down /service/10-dash
sv status /service/10-dash | grep -q '^down:'
quiet_panel
say "starting candidate panel; USB/SSH remain installed"
chroot "$ROOT" /bin/busybox sh /service/10-dash/run &
child=$!
sleep 10
kill -0 "$child"
# ioctl errors currently do not force a nonzero renderer exit; inspect diagnostics.
if grep -E 'screen failed|screen:|status unavailable|panel service parked' "$ROOT/var/log/dash.log"; then
	exit 1
fi
[ -s "$ROOT/var/run/dash.frame" ] || exit 1
printf '\n=== Candidate frame ===\n'
cat "$ROOT/var/run/dash.frame"
