#!/bin/bash
# Mock hardware in copied scripts. Production paths remain fixed.
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
bind_driver() {
	driver=$1
	mkdir -p "$ROOT/sys/bus/platform/drivers/$driver" "$ROOT/sys/devices/$driver.0"
	ln -s "$ROOT/sys/devices/$driver.0" "$ROOT/sys/bus/platform/drivers/$driver/$driver.0"
	ln -s "$ROOT/sys/bus/platform/drivers/$driver" "$ROOT/sys/devices/$driver.0/driver"
}
case "$name" in
uname) [ "$SCENARIO" != wrong-kernel ] && echo 3.0.35-lab126 || echo 3.2.0 ;;
tail|dmesg) exit 0 ;;
stat)
	mode=$(/usr/bin/stat -c %a "${@: -1}")
	echo "0:0:$mode"
	;;
mv)
	case "$SCENARIO:$1" in
		status-failure:*dash-usbnet-worker.status.new|complete-status-failure:*dash-usbnet-worker.status.new)
			[ "$SCENARIO" != status-failure ] || exit 1
			[ "$(sed -n '2p' "$1")" != 'setup complete' ] || exit 1
			;;
	esac
	/bin/mv "$@"
	;;
insmod)
	module=${1##*/}; module=${module%.ko}
	[ "$SCENARIO" != "load-failure-$module" ] || exit 1
	[ "$SCENARIO" != "register-failure-$module" ] || exit 0
	echo "$module 1 0 - Live 0" >>"$ROOT/proc/modules"
	case "$module" in
		fsl_otg_arc) [ "$SCENARIO" = unbound-otg ] || bind_driver fsl-usb2-otg ;;
		arcotg_udc) [ "$SCENARIO" = unbound-udc ] || bind_driver fsl-usb2-udc ;;
		g_ether)
			[ "$SCENARIO" = missing-usb ] || [ "$SCENARIO" = invalid-uptime ] || mkdir -p "$ROOT/sys/class/net/usb0"
			;;
	esac
	;;
ip)
	if [ "$1" = addr ]; then
		[ "$SCENARIO" != address-failure ] || exit 1
		echo 192.168.15.244/24 >"$ROOT/address"
	elif [ "$1" = link ] && [ "$2" = set ]; then
		[ "$SCENARIO" != link-failure ] || exit 1
		: >"$ROOT/link-up"
	elif [ "$1" = link ]; then
		[ "$SCENARIO" != link-read-failure ] || exit 1
		if [ "$SCENARIO" = link-not-up ]; then
			echo '2: usb0: <BROADCAST,MULTICAST> mtu 1500'
		else
			echo '2: usb0: <BROADCAST,MULTICAST,UP> mtu 1500'
		fi
	else
		[ "$SCENARIO" != wrong-ip ] || exit 0
		[ ! -f "$ROOT/address" ] || echo '    inet 192.168.15.244/24 scope global usb0'
		[ "$SCENARIO" != address-read-failure ] || exit 1
	fi
	;;
usb-supervise) printf 'supervisor called\n' ;;
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
dash-status) [ "$SCENARIO" != listen-failure ] ;;
dropbear) exit 42 ;;
*) exit 1 ;;
esac
MOCK
chmod +x "$MOCK/command"
for name in uname tail dmesg stat mv insmod ip ssh-seed dropbearkey dropbear dash-status usb-supervise; do
	ln -s command "$MOCK/$name"
done

fixture() {
	ROOT=$TMP/root
	rm -rf "$ROOT"
	mkdir -p "$ROOT"/{var/log,var/run,var/lib/dash-ssh,etc/dropbear,proc,sys/class/net,sys/bus/platform/drivers,sys/devices,dev/pts,lib/modules/3.0.35-lab126}
	chmod 700 "$ROOT/var/lib/dash-ssh" "$ROOT/etc/dropbear"
	printf '0.00 0.00\n' >"$ROOT/proc/uptime"
	: >"$ROOT/proc/modules"
	: >"$ROOT/calls"
	printf 'devpts %s/dev/pts devpts rw 0 0\n' "$ROOT" >"$ROOT/proc/mounts"
	: >"$ROOT/dev/ptmx"
	printf 'never touch this role interface\n' >"$ROOT/proc/asession"
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
	source=${source//\/bin\/usb-supervise/$MOCK/usb-supervise}
	source=${source//\/bin\/dash-status/$MOCK/dash-status}
	source=${source//\/bin\/dropbearkey/$MOCK/dropbearkey}
	source=${source//\/sbin\/dropbear/$MOCK/dropbear}
	source=${source//\/lib\/modules\//$ROOT/lib/modules/}
	for prefix in var etc proc sys dev; do
		source=${source//\/$prefix\//$ROOT/$prefix/}
	done
	source=${source/'[ -c '/'[ -f '}
	source=${source//'"$polls" -lt 10000'/'"$polls" -lt 2'}
	printf '%s\n' "$source" >"$TMP/run"
}
run_service() {
	render "$1"
	shift
	bash "$TMP/run" "$@"
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
fixture_binding() {
	local driver=$1
	mkdir -p "$ROOT/sys/bus/platform/drivers/$driver" "$ROOT/sys/devices/$driver.0"
	ln -s "$ROOT/sys/devices/$driver.0" "$ROOT/sys/bus/platform/drivers/$driver/$driver.0"
	ln -s "$ROOT/sys/bus/platform/drivers/$driver" "$ROOT/sys/devices/$driver.0/driver"
}

count=0
usb_cases=(success missing-usb address-failure link-failure wrong-ip address-read-failure link-read-failure link-not-up
	wrong-kernel unbound-otg unbound-udc loaded-unbound-otg loaded-unbound-udc loaded-success
	fake-module-link wrong-binding invalid-uptime status-failure complete-status-failure cancelled-worker)
for module in fsl_otg_arc arcotg_udc g_ether; do
	usb_cases+=("missing-$module" "symlink-$module" "load-failure-$module" "register-failure-$module")
done
for gadget in g_file_storage g_serial g_multi g_mass_storage g_cdc; do
	usb_cases+=("conflict-$gadget")
done
for SCENARIO in "${usb_cases[@]}"; do
	fixture
	case "$SCENARIO" in
		missing-*)
			module=${SCENARIO#missing-}
			[ "$module" = usb ] || rm "$ROOT/lib/modules/3.0.35-lab126/$module.ko"
			;;
		symlink-*)
			module=${SCENARIO#symlink-}
			mv "$ROOT/lib/modules/3.0.35-lab126/$module.ko" "$ROOT/module-target"
			ln -s "$ROOT/module-target" "$ROOT/lib/modules/3.0.35-lab126/$module.ko"
			;;
		conflict-*) printf '%s 1 0 - Live 0\n' "${SCENARIO#conflict-}" >"$ROOT/proc/modules" ;;
		loaded-unbound-otg) printf 'fsl_otg_arc 1 0 - Live 0\n' >"$ROOT/proc/modules" ;;
		loaded-unbound-udc)
			printf 'fsl_otg_arc 1 0 - Live 0\narcotg_udc 1 0 - Live 0\n' >"$ROOT/proc/modules"
			fixture_binding fsl-usb2-otg
			;;
		loaded-success)
			printf 'fsl_otg_arc 1 0 - Live 0\narcotg_udc 1 0 - Live 0\ng_ether 1 0 - Live 0\n' >"$ROOT/proc/modules"
			fixture_binding fsl-usb2-otg; fixture_binding fsl-usb2-udc
			mkdir -p "$ROOT/sys/class/net/usb0"
			;;
		fake-module-link)
			printf 'fsl_otg_arc 1 0 - Live 0\n' >"$ROOT/proc/modules"
			mkdir -p "$ROOT/sys/bus/platform/drivers/fsl-usb2-otg" "$ROOT/sys/module/fsl_otg_arc"
			ln -s "$ROOT/sys/module/fsl_otg_arc" "$ROOT/sys/bus/platform/drivers/fsl-usb2-otg/module"
			;;
		wrong-binding)
			printf 'fsl_otg_arc 1 0 - Live 0\n' >"$ROOT/proc/modules"
			fixture_binding fsl-usb2-otg
			rm "$ROOT/sys/devices/fsl-usb2-otg.0/driver"
			mkdir -p "$ROOT/sys/bus/platform/drivers/wrong"
			ln -s "$ROOT/sys/bus/platform/drivers/wrong" "$ROOT/sys/devices/fsl-usb2-otg.0/driver"
			;;
		invalid-uptime) printf 'not-a-clock\n' >"$ROOT/proc/uptime" ;;
		cancelled-worker) printf 'supervisor timeout\n' >"$ROOT/var/run/dash-usbnet-failed" ;;
	esac
	# Worker never owns public markers/status; the supervisor tests cleanup/ready.
	rc=0
	run_service 20-usbnet setup >>"$ROOT/var/log/dash.log" 2>&1 || rc=$?
	if [ "$SCENARIO" = success ] || [ "$SCENARIO" = loaded-success ]; then
		[ "$rc" = 0 ]
		[ ! -e "$ROOT/var/run/dash-usbnet-ready" ]
		[ "$(<"$ROOT/var/run/dash-usbnet-worker.status")" = $'starting\nsetup complete' ]
		assert_log 'setup complete; waiting for supervisor verification'
		if [ "$SCENARIO" = success ]; then
			grep -q '^insmod .*g_ether.ko use_eem=0 host_addr=02:00:00:00:00:01 dev_addr=02:00:00:00:00:02$' "$ROOT/calls"
			[ "$(grep '^insmod ' "$ROOT/calls" | cut -d' ' -f2 | xargs -n1 basename | tr '\n' ' ')" = 'fsl_otg_arc.ko arcotg_udc.ko g_ether.ko ' ]
		else
			! grep -q '^insmod ' "$ROOT/calls"
		fi
	else
		[ "$rc" != 0 ]
		[ ! -e "$ROOT/var/run/dash-usbnet-ready" ]
		[ ! -e "$ROOT/var/run/dash-usbnet-ready.new" ]
		if [ "$SCENARIO" = cancelled-worker ]; then
			[ "$(<"$ROOT/var/run/dash-usbnet-failed")" = 'supervisor timeout' ]
			[ ! -e "$ROOT/var/run/dash-usbnet-worker.status" ]
			! grep -q '^insmod ' "$ROOT/calls"
		else
			[ ! -e "$ROOT/var/run/dash-usbnet-failed" ]
			[ "$SCENARIO" = status-failure ] || [ "$(head -1 "$ROOT/var/run/dash-usbnet-worker.status")" = failed ]
		fi
		assert_log 'unavailable:'
		case "$SCENARIO" in
			wrong-kernel|conflict-*) ! grep -q '^insmod ' "$ROOT/calls" ;;
			unbound-otg|loaded-unbound-otg|fake-module-link|wrong-binding)
				! grep -q 'insmod .*arcotg_udc\|insmod .*g_ether' "$ROOT/calls" ;;
			unbound-udc|loaded-unbound-udc) ! grep -q 'insmod .*g_ether' "$ROOT/calls" ;;
		esac
	fi
	[ ! -e "$ROOT/var/run/dash-usbnet.status" ]
	[ "$(<"$ROOT/proc/asession")" = 'never touch this role interface' ]
	! grep -q 'ip .*route' "$ROOT/calls"
	count=$((count + 1))
done

SCENARIO=wrapper
fixture
run_service 20-usbnet
[ "$(grep -c '^usb-supervise ' "$ROOT/calls")" = 1 ]
! grep -q '^insmod ' "$ROOT/calls"
assert_log 'supervisor called'
rc=0
run_service 20-usbnet invalid >"$ROOT/argument-error" 2>&1 || rc=$?
[ "$rc" = 2 ]
[ "$(grep -c '^usb-supervise ' "$ROOT/calls")" = 1 ]
count=$((count + 2))

for SCENARIO in success missing-auth unsafe-auth unsafe-dir symlink-auth symlink-state symlink-hostkey dangling-hostkey missing-seed short-seed unsafe-seed no-pty missing-usb usb-failed usb-failed-ready wrong-ip entropy-failure missing-seeded-marker key-failure invalid-key unsafe-hostkey listen-failure; do
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
		usb-failed-ready) printf 'setup failed with stale ready\n' >"$ROOT/var/run/dash-usbnet-failed" ;;
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
		if [ "$SCENARIO" = missing-auth ]; then
			assert_log 'disabled: not provisioned'
			[ "$(head -1 "$ROOT/var/run/dash-sshd.status")" = disabled ]
		else
			assert_log 'unavailable:'
			[ "$(head -1 "$ROOT/var/run/dash-sshd.status")" = failed ]
		fi
		[ "$SCENARIO" = listen-failure ] || assert_closed
	fi
	count=$((count + 1))
done
printf 'runtime: %s mocked scenarios passed\n' "$count"
