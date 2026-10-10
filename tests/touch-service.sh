#!/bin/bash
# Mocked-hardware scenarios for overlay/service/40-touch/run. Production paths
# stay fixed in the service; this copy rewrites them under a scratch root.
# Needs bash, perl (for a real setsid), mkfifo and readlink -f.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
SERVICE=$HERE/overlay/service/40-touch/run
TMP=$(mktemp -d)
cleanup() {
	rc=$?
	if [ "$rc" -ne 0 ] && [ -d "$TMP/root" ]; then
		printf 'FAILED scenario: %s\n' "${SCENARIO:-setup}" >&2
		[ ! -f "$TMP/root/var/log/dash.log" ] || cat "$TMP/root/var/log/dash.log" >&2
		[ ! -f "$TMP/root/calls" ] || cat "$TMP/root/calls" >&2
		[ ! -f "$TMP/root/var/run/dash-touch.status" ] || cat "$TMP/root/var/run/dash-touch.status" >&2
	fi
	# Reap anything a hang scenario left behind.
	[ ! -f "$TMP/pids" ] || while read -r pid; do kill -KILL "$pid" 2>/dev/null || true; done <"$TMP/pids"
	rm -rf "$TMP"
}
trap cleanup EXIT
pin() { sed -n "s/^$1=//p" "$SERVICE"; }
I2C_SHA=$(pin I2C_SHA) CORE_SHA=$(pin CORE_SHA) MT_SHA=$(pin MT_SHA)
[ ${#I2C_SHA} = 64 ] && [ ${#CORE_SHA} = 64 ] && [ ${#MT_SHA} = 64 ]
export I2C_SHA CORE_SHA MT_SHA TMP

MOCK=$TMP/bin
mkdir -p "$MOCK"
cat >"$MOCK/command" <<'MOCK'
#!/bin/bash
set -eu
name=${0##*/}
printf '%s %s\n' "$name" "$*" >>"$ROOT/calls"
bind() {
	mkdir -p "$ROOT/sys/bus/$1/drivers/$2" "$ROOT/sys/devices/$3"
	ln -s "$ROOT/sys/devices/$3" "$ROOT/sys/bus/$1/drivers/$2/$3"
	ln -s "$ROOT/sys/bus/$1/drivers/$2" "$ROOT/sys/devices/$3/driver"
}
input() {
	mkdir -p "$ROOT/sys/class/input/$1/device" "$ROOT/dev/input"
	echo "$2" >"$ROOT/sys/class/input/$1/device/name"
	: >"$ROOT/dev/input/$1"
}
case "$name" in
uname) [ "$SCENARIO" = wrong-kernel ] && echo 3.2.0 || echo 3.0.35-lab126 ;;
dmesg) echo 'mock kernel log' ;;
tail) [ "$1" = -f ] || exec /usr/bin/tail "$@" ;;
sha256sum)
	module=${1##*/}; module=${module%.ko}
	case "$(cat "$1"):$module" in
		pinned:cyttsp4_i2c) hash=$I2C_SHA ;;
		pinned:cyttsp4_core) hash=$CORE_SHA ;;
		pinned:cyttsp4_mt_b) hash=$MT_SHA ;;
		*) hash=0000000000000000000000000000000000000000000000000000000000000000 ;;
	esac
	echo "$hash  $1"
	;;
setsid)
	if [ "$SCENARIO" = survivor-core ]; then
		# Model a worker no signal reaches (stuck in the kernel, then resumes):
		# the real worker is detached; the supervisor only sees a decoy PID.
		perl -MPOSIX -e 'fork and exit; POSIX::setsid(); exec @ARGV or die' -- "$@"
		echo $$ >>"$TMP/pids"
		exec sleep 30
	fi
	exec perl -MPOSIX -e 'POSIX::setsid() >= 0 or die "setsid: $!"; exec @ARGV or die' -- "$@"
	;;
insmod)
	module=${1##*/}; module=${module%.ko}
	[ "$SCENARIO" != "load-failure-$module" ] || exit 1
	[ "$SCENARIO" != "register-failure-$module" ] || exit 0
	if [ "$module" = cyttsp4_core ]; then
		case "$SCENARIO" in
			hang-core|stop-during-setup) echo $$ >"$ROOT/stuck-pid"; echo $$ >>"$TMP/pids"; exec sleep 30 ;;
			survivor-core) sleep 3; : >"$ROOT/resumed" ;;
		esac
	fi
	echo "$module 1 0 - Live 0" >>"$ROOT/proc/modules"
	case "$module" in
		cyttsp4_i2c)
				mkdir -p "$ROOT/sys/bus/i2c/drivers/cyttsp4_i2c_adapter"
				[ "$SCENARIO" = no-controller ] || bind i2c cyttsp4_i2c_adapter 1-0024 ;;
		cyttsp4_core)
			mkdir -p "$ROOT/sys/module/cyttsp4_core/parameters"
			mode=${2#recovery_mode=}
			[ "$SCENARIO" != recovery-after-load ] || mode=1
			echo "$mode" >"$ROOT/sys/module/cyttsp4_core/parameters/recovery_mode"
			mkdir -p "$ROOT/sys/bus/ttsp4/drivers/cyttsp4_core"
			[ "$SCENARIO" = core-unbound ] || bind ttsp4 cyttsp4_core main_ttsp_core.0
			;;
		cyttsp4_mt_b)
			mkdir -p "$ROOT/sys/bus/ttsp4/drivers/cyttsp4_mt"
			[ "$SCENARIO" = mt-unbound ] || bind ttsp4 cyttsp4_mt cyttsp4_mt.main_ttsp_core
			input event0 gpio-keys
			[ "$SCENARIO" = no-input ] || input event2 cyttsp4_mt
			[ "$SCENARIO" != multiple-input ] || input event3 cyttsp4_mt
			;;
	esac
	;;
*) exit 1 ;;
esac
MOCK
chmod +x "$MOCK/command"
for name in uname dmesg tail sha256sum setsid insmod; do
	ln -s command "$MOCK/$name"
done

fixture() {
	ROOT=$TMP/root
	rm -rf "$ROOT"
	mkdir -p "$ROOT"/{var/log,var/run,proc,sys/class/input,sys/module,lib/modules/3.0.35-lab126}
	chmod 755 "$ROOT/var/run"
	printf '0.00 0.00\n' >"$ROOT/proc/uptime"
	printf '0272201100000000\n' >"$ROOT/proc/board_id"
	: >"$ROOT/proc/modules"
	: >"$ROOT/calls"
	: >"$ROOT/var/log/dash.log"
	for module in cyttsp4_i2c cyttsp4_core cyttsp4_mt_b; do
		printf pinned >"$ROOT/lib/modules/3.0.35-lab126/$module.ko"
	done
	export ROOT SCENARIO
}
render() {
	# Patterns and replacements are variables: bash 3.2 (macOS) mis-parses
	# quoted slashes inside ${var//pattern/replacement}.
	local source from to prefix
	source=$(<"$SERVICE")
	sub() { from=$1; to=$2; source=${source//"$from"/"$to"}; }
	sub 'PATH=/bin:/sbin' "PATH=$MOCK:$PATH"
	sub 'DEADLINE=20' "DEADLINE=${DEADLINE:-3}"
	sub '"$polls" -lt 100000' '"$polls" -lt 2'
	sub '/bin/busybox sh' bash
	sub '/service/40-touch/run' "$TMP/run"
	# Placeholders first, so a scratch root under /var/folders is not rewritten.
	sub '/var/log/' '@ROOT@/var/log/'
	sub 'RUN=/var/run' 'RUN=@ROOT@/var/run'
	for prefix in proc sys lib/modules dev/input; do
		sub "/$prefix/" "@ROOT@/$prefix/"
	done
	sub '@ROOT@' "$ROOT"
	# Fixture device nodes are regular files: no mknod on the build host.
	sub '[ -c ' '[ -f '
	grep -q "^PATH=$MOCK:" <<<"$source" || { echo "render failed" >&2; exit 1; }
	printf '%s\n' "$source" >"$TMP/run"
}
supervise() {
	render
	bash "$TMP/run" "$@"
}
status() { cat "$ROOT/var/run/dash-touch.status"; }
inserted() { grep '^insmod ' "$ROOT/calls" | cut -d' ' -f2- | sed 's|.*/||' | tr '\n' ';'; }
expect_failed() {
	[ "$(head -1 "$ROOT/var/run/dash-touch.status")" = failed ] || { echo "FAIL: $SCENARIO not failed" >&2; exit 1; }
	[ -f "$ROOT/var/run/dash-touch-failed" ]
	grep -q "40-touch: unavailable: .*$1" "$ROOT/var/log/dash.log" || { echo "FAIL: $SCENARIO: no log '$1'" >&2; exit 1; }
}
expect_inserted() {
	[ "$(inserted)" = "$1" ] || { echo "FAIL: $SCENARIO inserted '$(inserted)', want '$1'" >&2; exit 1; }
}
no_retry() {
	# A restarted service this boot must neither touch hardware nor change status.
	local before calls
	before=$(status)
	calls=$(grep -c '^insmod ' "$ROOT/calls" || true)
	supervise
	[ "$(status)" = "$before" ]
	[ "$(grep -c '^insmod ' "$ROOT/calls" || true)" = "$calls" ]
	! grep -q '^setsid ' <(tail -n 1 "$ROOT/calls")
}
preload() {
	for module in "$@"; do echo "$module 1 0 - Live 0" >>"$ROOT/proc/modules"; done
}
count=0
ALL='cyttsp4_i2c.ko;cyttsp4_core.ko recovery_mode=0;cyttsp4_mt_b.ko;'

# --- success, and the order/arguments of every hardware call
SCENARIO=success; fixture
supervise
[ "$(status)" = "ready
$ROOT/dev/input/event2" ]
expect_inserted "$ALL"
[ ! -e "$ROOT/var/run/dash-touch-failed" ]
grep -q 'loading cyttsp4_core recovery_mode=0' "$ROOT/var/log/dash.log"
# Every hardware call is logged before it is made.
[ "$(grep -c '40-touch: loading cyttsp4_' "$ROOT/var/log/dash.log")" = 3 ]
grep -q 'ready: cyttsp4_mt at' "$ROOT/var/log/dash.log"
! grep -Eq 'insmod .*(loader|debug|device_access|zforce)' "$ROOT/calls"
no_retry; count=$((count + 1))

# --- already loaded and bound (e.g. a dev-loop) is accepted without insmod
SCENARIO=loaded-success; fixture
printf '0272201100000000' >"$ROOT/proc/board_id" # proc entry has no trailing newline
preload cyttsp4_i2c cyttsp4_core cyttsp4_mt_b
mkdir -p "$ROOT/sys/module/cyttsp4_core/parameters"; echo N >"$ROOT/sys/module/cyttsp4_core/parameters/recovery_mode"
SCENARIO=success "$MOCK/insmod" /x/cyttsp4_i2c.ko; SCENARIO=success "$MOCK/insmod" /x/cyttsp4_core.ko recovery_mode=0
SCENARIO=success "$MOCK/insmod" /x/cyttsp4_mt_b.ko; : >"$ROOT/calls"
supervise
[ "$(head -1 "$ROOT/var/run/dash-touch.status")" = ready ]
expect_inserted ''
count=$((count + 1))

# --- refusals before any hardware call
for SCENARIO in wrong-kernel wrong-board missing-board conflict-cyttsp4_loader conflict-cyttsp4_debug \
	conflict-cyttsp4_device_access conflict-zforce2 recovery-preloaded core-preloaded-unverifiable \
	loaded-unbound hash-cyttsp4_i2c hash-cyttsp4_core hash-cyttsp4_mt_b missing-cyttsp4_mt_b symlink-cyttsp4_core; do
	fixture
	case "$SCENARIO" in
		wrong-board) printf '0d4abcdef\n' >"$ROOT/proc/board_id" ;;
		missing-board) rm "$ROOT/proc/board_id" ;;
		conflict-*) preload "${SCENARIO#conflict-}" ;;
		recovery-preloaded)
			preload cyttsp4_core
			mkdir -p "$ROOT/sys/module/cyttsp4_core/parameters"; echo 1 >"$ROOT/sys/module/cyttsp4_core/parameters/recovery_mode"
			;;
		core-preloaded-unverifiable) preload cyttsp4_core ;;
		loaded-unbound) preload cyttsp4_i2c; mkdir -p "$ROOT/sys/bus/i2c/drivers/cyttsp4_i2c_adapter" ;;
		hash-*) printf 'tampered' >"$ROOT/lib/modules/3.0.35-lab126/${SCENARIO#hash-}.ko" ;;
		missing-*) rm "$ROOT/lib/modules/3.0.35-lab126/${SCENARIO#missing-}.ko" ;;
		symlink-*)
			mv "$ROOT/lib/modules/3.0.35-lab126/${SCENARIO#symlink-}.ko" "$ROOT/target.ko"
			ln -s "$ROOT/target.ko" "$ROOT/lib/modules/3.0.35-lab126/${SCENARIO#symlink-}.ko"
			;;
	esac
	supervise
	case "$SCENARIO" in
		wrong-kernel) expect_failed 'kernel release does not match' ;;
		wrong-board) expect_failed 'board type 0d4 is not the proven 027 board' ;;
		missing-board) expect_failed 'cannot read board id' ;;
		conflict-zforce2) expect_failed 'competing zforce module' ;;
		conflict-*) expect_failed "competing module ${SCENARIO#conflict-}" ;;
		recovery-preloaded) expect_failed 'cyttsp4_core recovery_mode is enabled' ;;
		core-preloaded-unverifiable) expect_failed 'cannot verify cyttsp4_core recovery_mode' ;;
		loaded-unbound) expect_failed 'cyttsp4_i2c_adapter has no bound device' ;;
		hash-*) expect_failed "module ${SCENARIO#hash-} does not match its pinned hash" ;;
		*) expect_failed 'missing or unsafe module' ;;
	esac
	! grep -q '0272201100000000' "$ROOT/var/log/dash.log"
	expect_inserted ''
	no_retry; count=$((count + 1))
done

# --- failures part-way: nothing after the failing step runs
for SCENARIO in no-controller core-unbound mt-unbound no-input multiple-input recovery-after-load \
	load-failure-cyttsp4_i2c load-failure-cyttsp4_core load-failure-cyttsp4_mt_b \
	register-failure-cyttsp4_i2c register-failure-cyttsp4_core register-failure-cyttsp4_mt_b; do
	fixture
	supervise
	case "$SCENARIO" in
		no-controller) expect_failed 'cyttsp4_i2c_adapter has no bound device'; expect_inserted 'cyttsp4_i2c.ko;' ;;
		core-unbound) expect_failed 'cyttsp4_core has no bound device'; expect_inserted 'cyttsp4_i2c.ko;cyttsp4_core.ko recovery_mode=0;' ;;
		recovery-after-load) expect_failed 'cyttsp4_core recovery_mode is enabled'; expect_inserted 'cyttsp4_i2c.ko;cyttsp4_core.ko recovery_mode=0;' ;;
		mt-unbound) expect_failed 'cyttsp4_mt has no bound device'; expect_inserted "$ALL" ;;
		no-input) expect_failed 'cyttsp4_mt input did not appear'; expect_inserted "$ALL" ;;
		multiple-input) expect_failed 'multiple cyttsp4_mt input devices'; expect_inserted "$ALL" ;;
		load-failure-cyttsp4_i2c|register-failure-cyttsp4_i2c) expect_failed 'cyttsp4_i2c'; expect_inserted 'cyttsp4_i2c.ko;' ;;
		*-cyttsp4_core) expect_failed 'cyttsp4_core'; expect_inserted 'cyttsp4_i2c.ko;cyttsp4_core.ko recovery_mode=0;' ;;
		*-cyttsp4_mt_b) expect_failed 'cyttsp4_mt_b'; expect_inserted "$ALL" ;;
	esac
	# The worker's reason reaches the public status.
	case "$SCENARIO" in load-failure-*) grep -q "^loading ${SCENARIO#load-failure-} failed$" "$ROOT/var/run/dash-touch.status" ;; esac
	grep -q 'mock kernel log' "$ROOT/var/log/dash.log"
	no_retry; count=$((count + 1))
done

# --- a stuck insmod: bounded deadline, the worker group is killed, no retry
SCENARIO=hang-core; fixture
start=$SECONDS
DEADLINE=1 supervise
[ $((SECONDS - start)) -lt 5 ]
expect_failed 'setup deadline exceeded during: loading cyttsp4_core; no retry'
expect_inserted 'cyttsp4_i2c.ko;cyttsp4_core.ko recovery_mode=0;'
pid=$(<"$ROOT/stuck-pid")
for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
! kill -0 "$pid" 2>/dev/null
no_retry; count=$((count + 1))

# --- a worker no signal reaches, which resumes after the deadline: it must
# stop at its next step and can never make the service ready
SCENARIO=survivor-core; fixture
DEADLINE=1 supervise
expect_failed 'setup deadline exceeded during: loading cyttsp4_core'
terminal=$(status)
for _ in $(seq 1 60); do [ ! -e "$ROOT/resumed" ] || break; sleep 0.1; done
[ -e "$ROOT/resumed" ]
sleep 0.5
[ "$(status)" = "$terminal" ]
expect_inserted 'cyttsp4_i2c.ko;cyttsp4_core.ko recovery_mode=0;'
! grep -q 'setup complete' "$ROOT/var/log/dash.log"
no_retry; count=$((count + 1))

# --- runit stop during setup is terminal for the boot
SCENARIO=stop-during-setup; fixture
render
DEADLINE=20 bash "$TMP/run" &
service=$!
for _ in $(seq 1 50); do grep -q 'cyttsp4_core' "$ROOT/calls" && break; sleep 0.1; done
kill -TERM "$service"
wait "$service" || true
[ "$(status)" = "failed
service stopped during setup; no retry" ]
pid=$(<"$ROOT/stuck-pid")
for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
! kill -0 "$pid" 2>/dev/null
no_retry; count=$((count + 1))

# --- an interrupted earlier attempt (status still starting) is not retried
SCENARIO=stale-attempt; fixture
mkdir "$ROOT/var/run/dash-touch-attempt"
printf 'starting\nlaunching setup worker\n' >"$ROOT/var/run/dash-touch.status"
supervise
expect_failed 'setup already attempted this boot; no retry'
expect_inserted ''
count=$((count + 1))

# --- usage
SCENARIO=usage; fixture; render
rc=0; bash "$TMP/run" bogus 2>/dev/null || rc=$?
[ "$rc" = 2 ]
[ ! -e "$ROOT/var/run/dash-touch.status" ]
count=$((count + 1))

printf 'touch-service: %s mocked scenarios passed\n' "$count"
