#!/bin/bash
# Dash OS build tool -- one multi-call script.
#
#   build.sh [build]                    assemble artifacts/ from overlay/ + src/ + pins
#   build.sh clean                      remove build outputs (keeps artifacts/)
#   build.sh userland  <src> <out>      cross-compile *.rs (rustc)       (in the image)
#   build.sh test      <srcdir>         run the userland unit tests      (in the image)
#   build.sh rootfs    <stage> <img> [mb]   package an ext3 rootfs       (in the image)
#   build.sh check-float <dir>...       refuse VFPv4-only binaries       (in the image)
#   build.sh fingerprint <img> [img2]   content fingerprint of a rootfs  (in the image)
#   build.sh lock-env  <image>          write BUILD-IMAGE.lock
#   build.sh hashes    <dir> <name>...  write <dir>/SHA256SUMS
#   build.sh verify    <dir>...         check each <dir>/SHA256SUMS
#
# `make` drives this (make build / verify / fingerprint); running it directly
# works too once the build image exists (`make env`).
#
# The build (default) is split so the authored tree (overlay/) never contains
# binaries and the rootfs image is always assembled the same way:
#   1. stage    overlay/ + src/ (compiled) + third-party/  ->  build/rootfs/
#   2. package  build/rootfs/  ->  artifacts/dash-rootfs.img  (ext3, dash-root)
#   3. stage    base-kernel/ + base-diag/  ->  artifacts/
#   4. record   artifacts/SHA256SUMS
#
# Built images are NOT committed (.gitignore): the pinned inputs + overlay/ +
# src/ are the version; these bytes are just what this machine packaged from it.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ART=$HERE/artifacts
STAGE=$HERE/build/rootfs
USRLAND=$HERE/build/userland

MAIN_KERNEL=main-uImage
DIAG_KERNEL=diag-uImage
ROOTFS=dash-rootfs.img

# The single build image. `make` exports DASH_BUILD_IMAGE; fall back to the same
# tag the Makefile derives, so direct invocation also works.
if [ -z "${DASH_BUILD_IMAGE:-}" ]; then
	tag=$( (command -v sha256sum >/dev/null 2>&1 \
		&& sha256sum "$HERE/Containerfile" \
		|| shasum -a 256 "$HERE/Containerfile") | cut -c1-12 )
	export DASH_BUILD_IMAGE="localhost/dash-build:$tag"
fi

say()   { printf '\n\033[1m== %s\033[0m\n' "$*"; }
sha256() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

usage() {
	cat <<'EOF'
Dash OS build tool.

  build.sh [build]                     assemble artifacts/
  build.sh clean                       remove build outputs
  build.sh userland  <src> <out>       cross-compile *.rs (in the image)
  build.sh test      <srcdir>          run the userland unit tests (in the image)
  build.sh rootfs    <stage> <img> [mb] package an ext3 rootfs (in the image)
  build.sh check-float <dir>...        refuse VFPv4-only binaries (in the image)
  build.sh fingerprint <img> [img2]    content fingerprint (in the image)
  build.sh lock-env  <image>           write BUILD-IMAGE.lock
  build.sh hashes    <dir> <name>...   write <dir>/SHA256SUMS
  build.sh verify    <dir>...          check each <dir>/SHA256SUMS
EOF
}

# ============================================================== userland
# Cross-compile every *.rs in <srcdir> into a static ARM binary in <outdir>.
# Static is not optional: the rootfs has no libc and no dynamic loader.
cmd_userland() {
	local SRC=${1:?usage: build.sh userland <srcdir> <outdir>}
	local OUT=${2:?usage: build.sh userland <srcdir> <outdir>}
	SRC=$(cd "$SRC" && pwd)
	mkdir -p "$OUT"
	OUT=$(cd "$OUT" && pwd)

	command -v podman >/dev/null || { echo "podman required" >&2; exit 1; }
	podman image exists "$DASH_BUILD_IMAGE" 2>/dev/null || {
		echo "build image missing: $DASH_BUILD_IMAGE (run: make env)" >&2; exit 1; }

	# Flags are pinned, not stylistic:
	#   -O                                  the size/speed point we test
	#   --target arm-unknown-linux-gnueabi  soft-float armel: the only float ABI
	#                                        this CPU has (golden rule 14)
	#   -C target-feature=+crt-static       mandatory: no libc, no dynamic loader
	#                                        in the rootfs (golden rule 15)
	#   -C panic=abort                      a one-shot renderer has nothing to
	#                                        unwind, so drop the unwinder
	# Output bytes depend on the pinned rustc version, so they change when the
	# build image changes; content is what rebuild-check proves.
	export DASH_RUSTFLAGS=${DASH_RUSTFLAGS:--O --edition 2021 --target arm-unknown-linux-gnueabi -C linker=arm-linux-gnueabi-gcc -C target-feature=+crt-static -C panic=abort -C codegen-units=1 -C strip=symbols}

	local RFILE RSRC
	RFILE=$(cd "$SRC" && ls *.rs 2>/dev/null)
	RSRC=$(printf '%s\n' "$RFILE" | grep -c . || true)
	[ "$RSRC" -gt 0 ] || { echo "no .rs files in $SRC" >&2; exit 1; }

	echo "=== userland: $RSRC source file(s) -> $OUT ==="

	COPYFILE_DISABLE=1 tar -C "$SRC" -cf - --no-xattrs . \
	| podman run -i --rm --pull never "$DASH_BUILD_IMAGE" bash -c '
set -euo pipefail
RF='"$(printf '%q' "$DASH_RUSTFLAGS")"'
mkdir -p /s/src /out
COPYFILE_DISABLE=1 tar -C /s/src -xf -
export PATH=/usr/bin:/usr/local/bin:$PATH
rustc --version >&2
for r in /s/src/*.rs; do
	b=$(basename "$r" .rs)
	rustc $RF -o "/out/$b" "$r"
	echo "built $b" >&2
done
# The container is throwaway, so the binaries travel back over stdout.
COPYFILE_DISABLE=1 tar -C /out -cf - .
' 2>"$OUT/build-userland.log" | COPYFILE_DISABLE=1 tar -C "$OUT" -xf - || {
		echo "userland build FAILED; see $OUT/build-userland.log" >&2
		tail -30 "$OUT/build-userland.log" >&2
		exit 1
	}

	grep -q "^built " "$OUT/build-userland.log" || {
		echo "userland build produced no output" >&2; exit 1; }

	local r b
	for r in $RFILE; do
		b=${r%.rs}
		[ -s "$OUT/$b" ] || { echo "MISSING output for $r" >&2; exit 1; }
		# A static ARM binary starts with the ELF magic (e_machine 0x28 = ARM).
		[ "$(od -A n -t x1 -N 20 "$OUT/$b" | tr -d ' \n' | cut -c1-8)" = "7f454c46" ] \
			|| { echo "not an ELF: $OUT/$b" >&2; exit 1; }
		printf '%s  %s  (%s bytes)\n' "$(sha256 "$OUT/$b")" "$b" \
			"$(wc -c <"$OUT/$b" | tr -d ' ')"
	done
}

# ============================================================== rootfs
# Turn an assembled <stagedir> into a 64 MiB ext3 image labelled dash-root.
#
# The label is load-bearing: the installer probes the superblock at offset
# 0x478 to decide whether Dash is installed. Determinism: the fs UUID and our
# file mtimes are pinned, but mke2fs 1.47 ignores SOURCE_DATE_EPOCH, so the
# image is content-reproducible, not byte-reproducible.
cmd_rootfs() {
	local STAGE=${1:?usage: build.sh rootfs <stagedir> <imgpath> [size_mb]}
	local IMG=${2:?usage: build.sh rootfs <stagedir> <imgpath> [size_mb]}
	local SIZE_MB=${3:-64}

	STAGE=$(cd "$STAGE" && pwd)
	mkdir -p "$(dirname "$IMG")"
	IMG=$(cd "$(dirname "$IMG")" && pwd)/$(basename "$IMG")

	local MKE2FS_UUID=d4a50001-0000-4000-8000-000000000001
	command -v podman >/dev/null || { echo "podman required" >&2; exit 1; }
	[ -f "$STAGE/etc/runit/1" ] || {
		echo "staged tree looks wrong: $STAGE/etc/runit/1 missing" >&2; exit 1; }

	# Pin source mtimes (2026-01-01T00:00:00Z) so mke2fs -d stamps our files
	# consistently. mke2fs's own inodes still get the wall clock.
	#
	# Two things learned here, both by removing a `|| true`:
	#   * `-t YYYYMMDDhhmm` is the portable form. `@epoch` is a GNU extension;
	#     BSD touch (macOS, the dev host) rejects it, and the error used to be
	#     swallowed, so the mtimes were NEVER actually pinned here.
	#   * TZ=UTC so the result does not depend on the builder's timezone.
	# NOT `|| true` on purpose: a swallowed error here silently costs
	# reproducibility, which is the exact thing this line exists to protect.
	TZ=UTC find "$STAGE" -exec touch -h -t 202601010000 {} +

	echo "=== rootfs: $STAGE -> $IMG (${SIZE_MB} MiB ext3) ==="

	COPYFILE_DISABLE=1 tar -C "$STAGE" -cf - --no-xattrs . \
	| podman run -i --rm --pull never "$DASH_BUILD_IMAGE" bash -c '
set -euo pipefail
mkdir -p /s/rootfs /out
COPYFILE_DISABLE=1 tar -C /s/rootfs -xf -
chown -R 0:0 /s/rootfs
chmod 755 /s/rootfs
# Everything in these directories is code the boot path executes, so force the
# exec bit on ALL of it rather than trusting the authored file modes. Whichever
# path was missed, the symptom is identical (a blank panel), so this is a
# directory rule, not a whitelist.
for d in bin sbin etc/runit; do
	[ -d "/s/rootfs/$d" ] || continue
	find "/s/rootfs/$d" -type f -exec chmod 755 {} +
done
find /s/rootfs/service -type f -name run -exec chmod 755 {} + 2>/dev/null || true
SIZE_MB='"$SIZE_MB"'
dd if=/dev/zero of=/out/rootfs.img bs=1M count=$SIZE_MB 2>/dev/null
mke2fs -q -F -t ext3 -m 0 -L dash-root -U '"$MKE2FS_UUID"' \
	-d /s/rootfs /out/rootfs.img 2>&1 | tail -3

# ---- verify the image we are about to ship, not the tree we hoped for -------
# A staging-tree check cannot catch a file whose exec bit was lost on the way
# in, and that is the class of bug that has hurt most, so assert on the artifact.
rm -rf /s/verify && mkdir -p /s/verify
debugfs -R "rdump / /s/verify" /out/rootfs.img >/dev/null 2>&1
fail=0
for f in bin/busybox bin/screen sbin/runit \
	 etc/runit/1 etc/runit/2 etc/runit/3; do
	if [ ! -f "/s/verify/$f" ]; then
		echo "FAIL: missing from image: /$f" >&2; fail=1
	elif [ "$(stat -c %a "/s/verify/$f")" != "755" ]; then
		echo "FAIL: /$f mode $(stat -c %a "/s/verify/$f"), want 755" >&2; fail=1
	fi
done
for bad in runsvdir runsv sv; do
	if [ -e "/s/verify/sbin/$bad" ]; then
		echo "FAIL: /sbin/$bad present -- it is VFPv4 and crashes on this CPU" >&2
		fail=1
	fi
done
nsvc=0
for d in /s/verify/service/*/; do
	[ -d "$d" ] || continue
	r="$d/run"
	nsvc=$((nsvc + 1))
	if [ ! -f "$r" ]; then
		echo "FAIL: service dir without a run script: ${d#/s/verify}/" >&2; fail=1
	elif [ "$(stat -c %a "$r")" != "755" ]; then
		echo "FAIL: ${r#/s/verify} mode $(stat -c %a "$r"), want 755" >&2; fail=1
	fi
done
[ "$nsvc" -ge 1 ] || { echo "FAIL: no services in image" >&2; fail=1; }
echo "services in image: $nsvc" >&2
for d in proc sys dev tmp var/run var/log; do
	[ -d "/s/verify/$d" ] || { echo "FAIL: missing mount point /$d" >&2; fail=1; }
done
lbl=$(dd if=/out/rootfs.img bs=1 skip=$((1024 + 120)) count=16 2>/dev/null | tr -d "\0")
[ "$lbl" = "dash-root" ] || { echo "FAIL: label is \"$lbl\", want dash-root" >&2; fail=1; }

# The image is about to be dd-ed onto a device, so prove the filesystem is
# actually consistent before shipping it. mke2fs can exit 0 and still leave
# something e2fsck wants to repair, and a rootfs that needs repair on first
# mount is a bad boot with no useful diagnostics.
if ! e2fsck -fn /out/rootfs.img >/tmp/dash-fsck.log 2>&1; then
	echo "FAIL: e2fsck rejected the image:" >&2
	tail -6 /tmp/dash-fsck.log >&2
	fail=1
fi

[ $fail -eq 0 ] || exit 1
echo "ROOTFS_OK" >&2
cat /out/rootfs.img
' >"$IMG.new" 2>"$IMG.build.log" || {
		echo "rootfs build FAILED; see $IMG.build.log" >&2
		tail -20 "$IMG.build.log" >&2
		exit 1
	}

	grep -q ROOTFS_OK "$IMG.build.log" || {
		echo "rootfs build did not report success" >&2; exit 1; }

	mv -f "$IMG.new" "$IMG"
	echo "built $IMG ($(wc -c <"$IMG" | tr -d ' ') bytes)"
	echo "sha256 $(sha256 "$IMG")"
}

# ============================================================== test
# Run the userland unit tests on the build image's HOST rustc. No Cargo, no
# crates, no extra packages: `rustc --test` is enough, and it links against the
# image's own glibc.
#
# The tests cover the pure logic only (record reading, fill length, font lookup,
# geometry). Anything that needs /dev/fb0 is not a unit test.
cmd_test() {
	local SRC=${1:?usage: build.sh test <srcdir>}
	SRC=$(cd "$SRC" && pwd)
	command -v podman >/dev/null || { echo "podman required" >&2; exit 1; }
	mkdir -p "$HERE/build"

	echo "=== test: $SRC/*.rs ==="
	COPYFILE_DISABLE=1 tar -C "$SRC" -cf - --no-xattrs . \
	| podman run -i --rm --pull never "$DASH_BUILD_IMAGE" bash -c '
set -euo pipefail
mkdir -p /s/src /out
COPYFILE_DISABLE=1 tar -C /s/src -xf -
export PATH=/usr/bin:/usr/local/bin:$PATH
rustc --version >&2
n=0
for r in /s/src/*.rs; do
	b=$(basename "$r" .rs)
	if grep -q "^#\[cfg(test)\]" "$r"; then
		echo "--- $b" >&2
		rustc --test -O --edition 2021 -o "/out/$b.test" "$r"
		"/out/$b.test" --test-threads=1
		n=$((n + 1))
	else
		echo "--- $b (no #[cfg(test)] module; nothing to run)" >&2
	fi
done
[ "$n" -gt 0 ] || { echo "no test module found in any source file" >&2; exit 1; }
echo "TEST_OK $n module(s)" >&2
' 2>&1 | tee "$HERE/build/test.log" | grep -E '^(test |---|running|test result|TEST_OK|error|warning)'
	grep -q '^TEST_OK' "$HERE/build/test.log" \
		|| { echo "test run did not report success" >&2; exit 1; }
	echo "test: ok (see $HERE/build/test.log)"
}

# ============================================================== lock-env
# Write BUILD-IMAGE.lock for a built env image.
#
# The build environment is an artifact with a content digest, not a recipe that
# is re-executed and trusted: the same Containerfile text yields different
# packages once Ubuntu's archive moves on. `make env` compares the local image
# against `id` and refuses to continue on a mismatch; `digest` is what makes it
# pullable by anyone else (a locally built image has no manifest digest until
# it is pushed, so that line may be absent).
cmd_lock_env() {
	local IMG=${1:?usage: build.sh lock-env <image> [published]}
	local PUBLISHED=${2:-no}
	command -v podman >/dev/null || { echo "podman required" >&2; exit 1; }
	local id digest image tag
	id=$(podman image inspect --format '{{.Id}}' "$IMG")
	digest=$(podman image inspect --format '{{.Digest}}' "$IMG" 2>/dev/null || true)
	tag=${IMG##*:}
	image=$(sed -n 's/^image: //p' "$HERE/BUILD-IMAGE.lock" 2>/dev/null || true)
	if [ -z "$image" ]; then
		# Registry paths must be lowercase; a GitHub login is not.
		image="ghcr.io/$(printf '%s' "${DASH_PINS_OWNER:-H4rryp0tt3r}" | tr 'A-Z' 'a-z')/dash-build"
	fi
	printf 'image: %s\n' "$image"
	printf 'tag: %s\n' "$tag"
	# podman reports a Digest even for an image that only ever existed on this
	# machine, so the digest alone cannot say "someone else can pull this".
	# `published` is the honest flag, and it is what gates the pull.
	printf 'published: %s\n' "$PUBLISHED"
	if [ -n "$digest" ] && [ "$digest" != "<no value>" ]; then
		printf 'digest: %s\n' "$digest"
	fi
	printf 'id: %s\n' "$id"
}

# ============================================================== check-float
# Refuse to ship a binary this CPU cannot execute.
#
# `runsvdir`/`runsv` from the stock runit were built for VFPv4 with fused
# multiply-add; the kernel showed `runsvdir: undefined instruction ... vfma.f64`
# and the supervisor died ~0.7 s in, taking the service tree with it. The only
# blocked instructions are vfma/vfms/vfnma/vfnms: VFPv2 and VFPv3 (including the
# non-fused vmla/vmls/vnmla/vnmls/vcvt) all run on this Cortex-A9.
cmd_check_float() {
	[ $# -ge 1 ] || { echo "usage: build.sh check-float <dir> [<dir>...]" >&2; exit 1; }
	command -v podman >/dev/null || { echo "podman required" >&2; exit 1; }

	local d
	for d in "$@"; do
		[ -d "$d" ] || { echo "not a directory: $d" >&2; exit 1; }
	done

	TMP=$(mktemp -d)
	trap 'rm -rf "${TMP:-}"' EXIT
	# Prefix each tree with its basename so the container can report a full path.
	for d in "$@"; do
		mkdir -p "$TMP/$(basename "$d")"
		( cd "$d" && COPYFILE_DISABLE=1 tar -cf - --no-xattrs . ) \
			| tar -C "$TMP/$(basename "$d")" -xf -
	done

	tar -C "$TMP" -cf - . | podman run -i --rm --pull never "$DASH_BUILD_IMAGE" bash -c '
set -euo pipefail
mkdir -p /scan && tar -C /scan -xf -
cd /scan
rc=0
while IFS= read -r f; do
	[ -f "$f" ] || continue
	magic=$(head -c 4 "$f" 2>/dev/null | od -A n -t x1 | tr -d " \n")
	[ "$magic" = "7f454c46" ] || continue
	bad=$(arm-linux-gnueabi-objdump -d "$f" 2>/dev/null \
		| grep -oE "\bvfma\.|\bvfms\.|\bvfnma\.|\bvfnms\." | head -1 || true)
	if [ -n "$bad" ]; then
		echo "FAIL: $f uses VFPv4 instruction \"$bad\" (CPU is VFPv3-D16)" >&2
		arm-linux-gnueabi-objdump -d "$f" 2>/dev/null \
			| grep -E "\bvfma\.|\bvfms\.|\bvfnma\.|\bvfnms\." | head -3 >&2
		rc=1
	fi
done < <(find . -type f | sort)
exit $rc
' || { echo "check-float: FAILED -- a binary cannot run on this CPU" >&2; exit 1; }

	echo "check-float: ok (no VFPv4-only instructions)"
}

# ============================================================== fingerprint
# Content identity of a rootfs image: every path, type, mode and content hash,
# in a stable order. Two images with equal fingerprints are interchangeable.
cmd_fingerprint() {
	local IMG=${1:?usage: build.sh fingerprint <image.img> [<image2.img>]}
	local IMG2=${2:-}
	local _abspath; _abspath() { echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; }
	IMG=$(_abspath "$IMG")
	[ -f "$IMG" ] || { echo "no such image: $IMG" >&2; exit 1; }

	local _fp
	_fp() {
		local img=$1
		podman run -i --rm --pull never "$DASH_BUILD_IMAGE" bash -c '
set -euo pipefail
cat >/tmp/f.img
label=$(dd if=/tmp/f.img bs=1 skip=$((1024 + 120)) count=16 2>/dev/null | tr -d "\0")
echo "label          $label"
echo "blocks         $(dumpe2fs -h /tmp/f.img 2>/dev/null | awk -F: "/Block count/{print \$2}" | tr -d " ")"
echo "block-size     $(dumpe2fs -h /tmp/f.img 2>/dev/null | awk -F: "/Block size/{print \$2}" | tr -d " ")"
echo "inode-count    $(dumpe2fs -h /tmp/f.img 2>/dev/null | awk -F: "/Inode count/{print \$2}" | tr -d " ")"
echo "features       $(dumpe2fs -h /tmp/f.img 2>/dev/null | sed -n "s/^Filesystem features:[[:space:]]*//p")"
echo "state          $(dumpe2fs -h /tmp/f.img 2>/dev/null | sed -n "s/^Filesystem state:[[:space:]]*//p")"
echo "--- entries (path, type, mode, sha256) ---"
rm -rf /tmp/tree && mkdir -p /tmp/tree
debugfs -R "rdump / /tmp/tree" /tmp/f.img >/dev/null 2>&1
cd /tmp/tree
find . -mindepth 1 | sed -e "s|^\./||" | LC_ALL=C sort | while read -r f; do
	[ -n "$f" ] || continue
	if [ -d "$f" ]; then
		printf "%-7s %-5s /%s\n" "dir" "$(stat -c %a "$f")" "$f"
	else
		printf "%-7s %-5s %s  /%s\n" "file" "$(stat -c %a "$f")" \
			"$(sha256sum "$f" | cut -d" " -f1)" "$f"
	fi
done
' <"$img"
	}

	local A; A=$(_fp "$IMG")
	echo "=== fingerprint: $IMG ==="
	echo "$A"

	if [ -n "$IMG2" ]; then
		IMG2=$(_abspath "$IMG2")
		local B; B=$(_fp "$IMG2")
		echo
		echo "=== fingerprint: $IMG2 ==="
		echo "$B"
		echo
		if [ "$A" = "$B" ]; then
			echo "RESULT: CONTENT-IDENTICAL (any byte differences are timestamps only)"
		else
			echo "RESULT: CONTENT DIFFERS"
			diff <(echo "$A") <(echo "$B") || true
			exit 1
		fi
	fi
}

# ============================================================== hashes
cmd_hashes() {
	local DIR=${1:?usage: build.sh hashes <dir> <name>...}
	shift
	[ $# -gt 0 ] || { echo "give at least one file name" >&2; exit 1; }
	[ -d "$DIR" ] || { echo "no such directory: $DIR" >&2; exit 1; }
	(
		cd "$DIR"
		: >SHA256SUMS
		local n
		for n in "$@"; do printf '%s  %s\n' "$(sha256 "$n")" "$n" >>SHA256SUMS; done
	)
	echo "wrote $DIR/SHA256SUMS"
	cat "$DIR/SHA256SUMS"
}

# ============================================================== verify
cmd_verify() {
	[ $# -ge 1 ] || { echo "usage: build.sh verify <dir>..." >&2; exit 1; }
	local rc=0 DIR want name got
	for DIR in "$@"; do
		if [ ! -f "$DIR/SHA256SUMS" ]; then
			echo "no SHA256SUMS in $DIR" >&2; rc=1; continue
		fi
		while read -r want name; do
			[ -n "${name:-}" ] || continue
			if [ ! -f "$DIR/$name" ]; then
				echo "MISSING  $name" >&2; rc=1; continue
			fi
			got=$(sha256 "$DIR/$name")
			if [ "$got" = "$want" ]; then
				echo "ok       $name"
			else
				echo "MISMATCH $name" >&2
				echo "  want $want" >&2
				echo "  got  $got" >&2
				rc=1
			fi
		done <"$DIR/SHA256SUMS"
	done
	return $rc
}

# ============================================================== build
cmd_build() {
	say "preflight"
	command -v podman >/dev/null || { echo "podman required" >&2; exit 1; }
	podman image exists "$DASH_BUILD_IMAGE" 2>/dev/null || {
		echo "missing build image: $DASH_BUILD_IMAGE (run: make env)" >&2; exit 1; }
	echo "image ok: $DASH_BUILD_IMAGE"
	[ -f "$HERE/base-kernel/$MAIN_KERNEL" ] || {
		echo "missing $HERE/base-kernel/$MAIN_KERNEL (run: make unpack)" >&2; exit 1; }
	[ -f "$HERE/base-diag/$DIAG_KERNEL" ] || {
		echo "missing $HERE/base-diag/$DIAG_KERNEL (run: make unpack)" >&2; exit 1; }

	# The pins arrive from another repository over a network, so check them before
	# anything consumes them. `make build` already depends on `verify`; this is the
	# belt to that braces, so a direct `./build.sh build` cannot skip it either.
	say "verify pinned inputs"
	cmd_verify "$HERE/base-kernel" "$HERE/base-diag" \
		"$HERE/third-party/busybox" "$HERE/third-party/runit" \
		"$HERE/third-party/eink-firmware" >/dev/null

	say "stage rootfs tree"
	rm -rf "$STAGE" "$USRLAND"
	mkdir -p "$STAGE" "$USRLAND"

	# Authored content: the runit stages, the dashboard service and /etc.
	COPYFILE_DISABLE=1 tar -C "$HERE/overlay" -cf - --no-xattrs . | COPYFILE_DISABLE=1 tar -C "$STAGE" -xf -

	# VERSION is the single source of the version number. /etc/dash-release is
	# GENERATED here, never authored: a hand-maintained copy is a second thing
	# to forget, and the panel reads this file, so a stale copy makes the device
	# report the wrong version while looking perfectly healthy.
	mkdir -p "$STAGE/etc"
	printf '%s\n' "$(cat "$HERE/VERSION")" > "$STAGE/etc/dash-release"

	# The authored tree contains only what we author (no bin/, sbin/ or
	# lib/firmware/imx/), so create those directories here -- before installing
	# into them. The mount points MUST also exist before stage 1 mounts on them:
	# `mount -t proc proc /proc` fails ENOENT if /proc is absent.
	mkdir -p "$STAGE/bin" "$STAGE/sbin" "$STAGE/lib/firmware/imx"
	mkdir -p "$STAGE/proc" "$STAGE/sys" "$STAGE/dev" "$STAGE/tmp"
	mkdir -p "$STAGE/var" "$STAGE/var/run" "$STAGE/var/log" "$STAGE/root" "$STAGE/mnt"

	# Our own code, cross-compiled here.
	cmd_userland "$HERE/src" "$USRLAND"
	install -m 0755 "$USRLAND/screen" "$STAGE/bin/screen"

	# Pinned third-party binaries.
	install -m 0755 "$HERE/third-party/busybox/busybox" "$STAGE/bin/busybox"

	# runit: ONLY `/sbin/runit` (PID 1) is used standalone -- it is clean of VFPv4,
	# which is why stage 1 was always reached. Stage 2 uses busybox's own (VFPv2)
	# runsvdir/runsv applets; the detached runit runsvdir/runsv/sv are not shipped.
	install -m 0755 "$HERE/third-party/runit/runit" "$STAGE/sbin/runit"

	# Third-party uninstall guard: if these reappear in the staged tree, the build
	# must not silently ship them again.
	local bad
	for bad in runsvdir runsv sv; do
		if [ -e "$STAGE/sbin/$bad" ]; then
			echo "REFUSING to build: /sbin/$bad is the VFPv4 build that crashes on this CPU" >&2
			exit 1
		fi
	done

	# The e-ink waveform is REQUIRED and already gzip; installed as-is. The
	# few-hundred-byte placeholder freezes the panel white, it does not degrade.
	install -m 0644 "$HERE/third-party/eink-firmware/epdc_E60_V220.fw" \
		"$STAGE/lib/firmware/imx/epdc_E60_V220.fw"
	install -m 0644 "$HERE/third-party/eink-firmware/default.fw.gz" \
		"$STAGE/lib/firmware/imx/default.fw.gz"

	# Pin directory modes: mkdir inherits the caller's umask, and a rootfs whose
	# /bin is 0777 is not reproducible. Regular files under /etc to 0644 (-type f:
	# /etc/runit is a directory, and 0644 on it makes it untraversable).
	find "$STAGE" -type d -exec chmod 0755 {} +
	find "$STAGE/etc" -maxdepth 1 -type f -exec chmod 0644 {} +

	echo "staged $(find "$STAGE" -type f | wc -l | tr -d ' ') files into $STAGE"

	# Safety net: macOS junk. ._* are AppleDouble members BSD tar synthesises for
	# files with an xattr (COPYFILE_DISABLE suppresses them); .DS_Store is a REAL
	# file, so it lands in the image unless caught here.
	local junk
	junk=$(find "$STAGE" \( -name '._*' -o -name '.DS_Store' \) 2>/dev/null)
	if [ -n "$junk" ]; then
		echo "staged tree contains macOS junk files:" >&2
		echo "$junk" >&2
		echo "delete them from overlay/ and rebuild" >&2
		exit 1
	fi

	say "package rootfs image"
	mkdir -p "$ART"
	cmd_rootfs "$STAGE" "$ART/$ROOTFS" 64

	# Guard the failure mode that has cost the most time: a binary built for a
	# newer FPU than this CPU has. Runs on the staged tree, before packaging.
	say "check for VFPv4-only instructions (CPU is VFPv3-D16)"
	cmd_check_float "$STAGE/bin" "$STAGE/sbin"

	say "stage kernels"
	install -m 0644 "$HERE/base-kernel/$MAIN_KERNEL" "$ART/$MAIN_KERNEL"
	install -m 0644 "$HERE/base-diag/$DIAG_KERNEL"   "$ART/$DIAG_KERNEL"

	say "record SHA256SUMS (hashes of the RAW bytes -- these are what we dd)"
	cmd_hashes "$ART" "$MAIN_KERNEL" "$DIAG_KERNEL" "$ROOTFS"

	say "done"
	cat <<EOF
artifacts in $ART

  $MAIN_KERNEL    main kernel slot  (0x41000), EPDC built-in
  $DIAG_KERNEL    diags/install slot (0xE41000)
  $ROOTFS     p1 rootfs, ext3 label dash-root

These images are NOT committed (see .gitignore). The rootfs hash changes on
every build anyway (mke2fs stamps wall-clock times), so the version is defined by
the pinned inputs + overlay/ + src/, not by these bytes.

Next: see docs/RUNBOOK.md for the install runbook.
EOF
}

# ============================================================== dispatch
case "${1:-build}" in
	build)       shift; cmd_build "$@";;
	clean)       rm -rf "$HERE/build"; rm -f "$ART"/*.build.log; echo "removed build outputs";;
	userland)    shift; cmd_userland "$@";;
	test)        shift; cmd_test "$@";;
	rootfs)      shift; cmd_rootfs "$@";;
	check-float) shift; cmd_check_float "$@";;
	fingerprint) shift; cmd_fingerprint "$@";;
	lock-env)    shift; cmd_lock_env "$@";;
	hashes)      shift; cmd_hashes "$@";;
	verify)      shift; cmd_verify "$@";;
	-h|--help|help) usage;;
	*) echo "unknown command: $1" >&2; usage >&2; exit 1;;
esac
