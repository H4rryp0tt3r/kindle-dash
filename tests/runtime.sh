#!/bin/bash
# Mock the hardware boundary, not a production environment-variable escape hatch.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d)
cleanup() {
	rc=$?
	if [ "$rc" -ne 0 ] && [ -d "$TMP/root" ]; then
		printf 'FAILED scenario: %s\n' "${SCENARIO:-setup}" >&2
		[ ! -f "$TMP/root/var/log/dash.log" ] || cat "$TMP/root/var/log/dash.log" >&2
		[ ! -f "$TMP/root/calls" ] || cat "$TMP/root/calls" >&2
	fi
	rm -rf "$TMP"
}
trap cleanup EXIT
MOCK=$TMP/bin
mkdir -p "$MOCK"
cat >"$MOCK/command" <<'MOCK'
#!/bin/bash
set -eu
name=${0##*/}
printf '%s %s\n' "$name" "$*" >>"$ROOT/calls"
case "$name" in
uname) echo 3.0.35-lab126 ;;
tail|dmesg) exit 0 ;;
stat)
	mode=$(/usr/bin/stat -c %a "${@: -1}")
	# Filesystem ownership is represented by a root-owned device fixture.
	echo "0:0:$mode"
	;;
insmod)
	[ "$SCENARIO" != module-failure ] || exit 1
	module=${1##*/}; module=${module%.ko}
	echo "$module 1 0 - Live 0" >>"$ROOT/proc/modules"
	if [ "$module" = g_ether ] && [ "$SCENARIO" != missing-usb ]; then
		mkdir -p "$ROOT/sys/class/net/usb0"
	fi
	;;
ip)
	if [ "$1" = addr ]; then
		[ "$SCENARIO" != address-failure ] || exit 1
		echo 192.168.15.244/24 >"$ROOT/address"
	elif [ "$1" = link ]; then
		[ "$SCENARIO" != link-failure ] || exit 1
	else
		[ "$SCENARIO" != wrong-ip ] || exit 0
		[ ! -f "$ROOT/address" ] || echo '    inet 192.168.15.244/24 scope global usb0'
	fi
	;;
ssh-seed)
	[ "$SCENARIO" != entropy-failure ] || exit 1
	[ "$SCENARIO" = missing-seeded-marker ] || : >"$ROOT/var/run/dash-ssh-seeded"
	;;
dropbearkey)
	[ "$SCENARIO" != key-failure ] || exit 1
	if [ "$1" = -t ]; then
		printf 'mock private key\n' >"$4"
	else
		[ "$SCENARIO" != invalid-key ] || exit 1
	fi
	echo 'ssh-ed25519 AAAA mock-public-key'
	echo 'Fingerprint: SHA256:mock-public-fingerprint'
	;;
dropbear) exit 42 ;;
*) exit 1 ;;
esac
MOCK
chmod +x "$MOCK/command"
for name in uname tail dmesg stat insmod ip ssh-seed dropbearkey dropbear; do
	ln -s command "$MOCK/$name"
done

fixture() {
	ROOT=$TMP/root
	rm -rf "$ROOT"
	mkdir -p "$ROOT"/{var/log,var/run,var/lib/dash-ssh,etc/dropbear,proc,sys/class/net,dev/pts,lib/modules/3.0.35-lab126}
	chmod 700 "$ROOT/var/lib/dash-ssh" "$ROOT/etc/dropbear"
	printf '0.00 0.00\n' >"$ROOT/proc/uptime"
	: >"$ROOT/proc/modules"
	printf 'devpts %s/dev/pts devpts rw 0 0\n' "$ROOT" >"$ROOT/proc/mounts"
	: >"$ROOT/dev/ptmx"
	printf '0\n' >"$ROOT/proc/asession"
	printf 'ssh-ed25519 AAAA fixture\n' >"$ROOT/etc/dropbear/authorized_keys"
	dd if=/dev/zero of="$ROOT/var/lib/dash-ssh/seed" bs=64 count=1 2>/dev/null
	chmod 600 "$ROOT/etc/dropbear/authorized_keys" "$ROOT/var/lib/dash-ssh/seed"
	for module in fsl_otg_arc arcotg_udc g_ether; do
		: >"$ROOT/lib/modules/3.0.35-lab126/$module.ko"
	done
	export ROOT SCENARIO
}
render() {
	local source
	source=$(<"$HERE/overlay/service/$1/run")
	source=${source/'PATH=/bin:/sbin'/"PATH=$MOCK:$PATH"}
	source=${source//\/bin\/ssh-seed/$MOCK/ssh-seed}
	source=${source//\/bin\/dropbearkey/$MOCK/dropbearkey}
	source=${source//\/sbin\/dropbear/$MOCK/dropbear}
	source=${source//\/lib\/modules\//$ROOT/lib/modules/}
	for prefix in var etc proc sys dev; do
		source=${source//\/$prefix\//$ROOT/$prefix/}
	done
	# Fake a character device only in this copied test script.
	source=${source/'[ -c '/'[ -f '}
	# A fixed fake uptime must terminate promptly on unavailable hardware.
	source=${source//'"$polls" -lt 10000'/'"$polls" -lt 2'}
	printf '%s\n' "$source" >"$TMP/run"
}
run_service() {
	render "$1"
	bash "$TMP/run"
}
assert_log() {
	grep -q "$1" "$ROOT/var/log/dash.log" || {
		printf 'FAIL: %s: expected log %s\n' "$SCENARIO" "$1" >&2
		exit 1
	}
}
assert_closed() {
	! grep -q '^dropbear ' "$ROOT/calls" || {
		printf 'FAIL: %s unexpectedly launched SSH\n' "$SCENARIO" >&2
		exit 1
	}
}

count=0
for SCENARIO in success missing-module module-failure missing-usb address-failure link-failure wrong-ip; do
	fixture
	[ "$SCENARIO" != missing-module ] || rm "$ROOT/lib/modules/3.0.35-lab126/arcotg_udc.ko"
	run_service 20-usbnet
	if [ "$SCENARIO" = success ]; then
		[ -f "$ROOT/var/run/dash-usbnet-ready" ]
		assert_log 'ready; Mac USB Ethernet'
		grep -q '^insmod .*g_ether.ko host_addr=02:00:00:00:00:01 dev_addr=02:00:00:00:00:02$' "$ROOT/calls"
		[ "$(<"$ROOT/proc/asession")" = 0 ]
	else
		[ ! -e "$ROOT/var/run/dash-usbnet-ready" ]
		[ -s "$ROOT/var/run/dash-usbnet-failed" ]
		assert_log 'unavailable:'
	fi
	count=$((count + 1))
done

for SCENARIO in success missing-auth unsafe-auth unsafe-dir symlink-auth symlink-state symlink-hostkey dangling-hostkey missing-seed short-seed unsafe-seed no-pty missing-usb usb-failed wrong-ip entropy-failure missing-seeded-marker key-failure invalid-key unsafe-hostkey; do
	fixture
	: >"$ROOT/var/run/dash-usbnet-ready"
	echo 192.168.15.244/24 >"$ROOT/address"
	case "$SCENARIO" in
		missing-auth) rm "$ROOT/etc/dropbear/authorized_keys" ;;
		unsafe-auth) chmod 644 "$ROOT/etc/dropbear/authorized_keys" ;;
		unsafe-dir) chmod 755 "$ROOT/var/lib/dash-ssh" ;;
		symlink-auth)
			mv "$ROOT/etc/dropbear/authorized_keys" "$ROOT/etc/dropbear/key-source"
			ln -s key-source "$ROOT/etc/dropbear/authorized_keys"
			;;
		symlink-state)
			mv "$ROOT/var/lib/dash-ssh" "$ROOT/var/lib/real-state"
			ln -s real-state "$ROOT/var/lib/dash-ssh"
			;;
		symlink-hostkey)
			printf 'mock key\n' >"$ROOT/var/lib/dash-ssh/key-source"
			chmod 600 "$ROOT/var/lib/dash-ssh/key-source"
			ln -s key-source "$ROOT/var/lib/dash-ssh/dropbear_ed25519_host_key"
			;;
		dangling-hostkey) ln -s missing "$ROOT/var/lib/dash-ssh/dropbear_ed25519_host_key" ;;
		missing-seed) rm "$ROOT/var/lib/dash-ssh/seed" ;;
		short-seed) printf 'short' >"$ROOT/var/lib/dash-ssh/seed" ;;
		unsafe-seed) chmod 644 "$ROOT/var/lib/dash-ssh/seed" ;;
		no-pty) rm "$ROOT/dev/ptmx" ;;
		missing-usb) rm "$ROOT/var/run/dash-usbnet-ready" ;;
		usb-failed)
			rm "$ROOT/var/run/dash-usbnet-ready"
			printf 'module loading failed\n' >"$ROOT/var/run/dash-usbnet-failed"
			;;
		invalid-key|unsafe-hostkey)
			printf 'mock existing key\n' >"$ROOT/var/lib/dash-ssh/dropbear_ed25519_host_key"
			chmod 600 "$ROOT/var/lib/dash-ssh/dropbear_ed25519_host_key"
			[ "$SCENARIO" != unsafe-hostkey ] || chmod 644 "$ROOT/var/lib/dash-ssh/dropbear_ed25519_host_key"
			;;
	esac
	run_service 30-sshd
	if [ "$SCENARIO" = success ]; then
		grep -Fqx "dropbear -F -s -j -k -p 192.168.15.244:22 -r $ROOT/var/lib/dash-ssh/dropbear_ed25519_host_key -D $ROOT/etc/dropbear" "$ROOT/calls"
		assert_log 'SSH server returned rc=42'
		! grep -q 'mock private key' "$ROOT/var/log/dash.log"
		: >"$ROOT/calls"
		run_service 30-sshd
		! grep -q '^dropbearkey -t' "$ROOT/calls"
		! grep -q '^insmod ' "$ROOT/calls"
	else
		assert_log 'unavailable:'
		assert_closed
	fi
	count=$((count + 1))
done
printf 'runtime: %s mocked scenarios passed\n' "$count"
