# syntax=docker/dockerfile:1
#
# The one Dash OS build environment. Everything that cross-compiles userland,
# packages the rootfs, fingerprints it, or checks FPU usage runs in this image;
# the host only needs podman + make.
#
# Built by `make env`. The image tag is derived from this file's sha256, so any
# edit here transparently forces a rebuild on the next `make build`.

FROM docker.io/library/ubuntu@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254

# arm-linux-gnueabi-*  GCC 13 cross for armel (soft-float). No C is compiled any
#   more, but this stays as the LINKER DRIVER for
#   rustc --target arm-unknown-linux-gnueabi: it supplies the cross crt objects
#   and -lc that the static link needs.
#   libc6-dev-armel-cross is pulled in by gcc-arm-linux-gnueabi, but it is the
#   thing that supplies crt1.o / -lc, so it is named explicitly. The cross
#   glibc's minimum kernel is <= 2.6.32, so its static binaries run on the
#   device's stock 3.0.35 kernel (verified under qemu with a faked uname).
# rustup + rustc     the userland compiler. The toolchain is VERSION-PINNED
#   (see docs/pinned-inputs.md). arm-unknown-linux-gnueabi is soft-float, which
#   is the only float ABI this CPU has (golden rule 14); build.sh check-float
#   still proves the output rather than trusting the target triple.
# gcc + libc6-dev     HOST toolchain, and the only thing that can link a test
#   binary. `rustc --test` builds src/*.rs for x86_64-unknown-linux-gnu and
#   needs a native `cc` plus the native crt objects (Scrt1.o, crti.o) and libc;
#   the armel cross toolchain supplies neither. libc6-dev is named explicitly
#   because gcc only RECOMMENDS it and this image installs with
#   --no-install-recommends. Unit tests run on the host, so this is what makes
#   `make test` work without Cargo, crates or qemu.
# e2fsprogs            mke2fs -d, debugfs, dumpe2fs, e2fsck (ext3, no loop dev)
# dosfstools           mkfs.vfat, for the p4 userstore if we ever image it
ARG RUST_TOOLCHAIN=1.83.0
# The rustup proxies read the toolchain from RUSTUP_HOME, so this has to be a
# real ENV: `make build` runs a fresh container that inherits it. Without it the
# proxy looks in ~/.rustup, finds nothing, and errors out.
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo

# Deliberately two layers, not one: the apt layer is cheap to rebuild, the rustup
# layer costs a ~1 GB download. Keeping them separate means changing a package
# name does not re-download the toolchain.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        gcc \
        libc6-dev \
        gcc-arm-linux-gnueabi \
        binutils-arm-linux-gnueabi \
        libc6-dev-armel-cross \
        e2fsprogs \
        dosfstools \
        coreutils \
        findutils \
        tar \
        gzip \
        curl \
        ca-certificates; \
    rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    curl -sSf https://sh.rustup.rs | sh -s -- \
        -y --profile minimal --no-modify-path \
        --default-toolchain "$RUST_TOOLCHAIN" \
        --target arm-unknown-linux-gnueabi; \
    ln -sf /opt/cargo/bin/* /usr/local/bin/; \
    rustc --version; \
    rustup target list --installed
