# Dash OS -- build entry points
#
#   make build            build artifacts/ (ensures pins + env as needed)
#   make pins             fetch the binary inputs from the private pins repo
#   make test             unit tests for the userland renderer
#   make bootstrap        prepare only: pins + the env image
#   make env              provide the build-environment image (digest-pinned)
#   make env-bump         rebuild + republish the env image, re-pin its digest
#   make verify           check the pinned inputs against their SHA256SUMS
#   make fingerprint      print the rootfs content fingerprint
#   make rebuild-check    build twice and prove content reproducibility
#   make shell            interactive build container
#   make clean            remove build outputs
#
# NO BINARIES ARE COMMITTED HERE. Kernels, busybox, runit, the e-ink waveform
# and the stock U-Boot live in the private kindle-dash-pins repo; `make pins`
# materialises them at the paths build.sh already reads.
#
# A release is two SHAs -- this repo and the pins repo, both at the same tag.
# See CHANGELOG.md.

SHELL := /bin/bash
HERE  := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
ART   := $(HERE)/artifacts
RECIPE := $(HERE)/Containerfile
LOCK   := $(HERE)/BUILD-IMAGE.lock
PINS_DIR := $(HERE)/.pins

# The private repo holding every binary input.
PINS_REPO ?= H4rryp0tt3r/kindle-dash-pins
# A release pins both repos at the same tag. Override with `make pins PINS_REF=`.
PINS_REF ?= $(shell git -C $(HERE) describe --tags --exact-match 2>/dev/null || echo main)

# The env image tag is derived from the Containerfile's sha256, so editing the
# Containerfile changes the tag and transparently forces a rebuild.
IMAGE_TAG := $(shell (command -v sha256sum >/dev/null 2>&1 \
	&& sha256sum $(RECIPE) || shasum -a 256 $(RECIPE)) | cut -c1-12)
BUILD_IMAGE := localhost/dash-build:$(IMAGE_TAG)
export DASH_BUILD_IMAGE := $(BUILD_IMAGE)

# Raw (decompressed) forms of the pinned inputs. `pins` materialises the .gz
# twins; `unpack` expands them; the build and the device consume the raw files.
RAW_INPUTS := \
	$(HERE)/base-kernel/main-uImage \
	$(HERE)/base-diag/diag-uImage \
	$(HERE)/third-party/busybox/busybox \
	$(HERE)/third-party/runit/runit

.PHONY: help pins bootstrap env env-bump unpack build test verify fingerprint \
        rebuild-check shell clean current

help:
	@echo "Dash OS -- version $$(cat $(HERE)/VERSION 2>/dev/null || echo none)"
	@echo ""
	@echo "  make build            build artifacts/ (ensures pins + env)"
	@echo "  make pins             fetch binary inputs from $(PINS_REPO) @ $(PINS_REF)"
	@echo "  make test             unit tests for the userland renderer"
	@echo "  make bootstrap        prepare only: pins + the env image"
	@echo "  make env              provide the build-environment image"
	@echo "  make env-bump         rebuild + republish the env image, re-pin its digest"
	@echo "  make verify           check the pinned inputs (SHA256SUMS)"
	@echo "  make fingerprint      print the rootfs content fingerprint"
	@echo "  make rebuild-check    rebuild and prove content reproducibility"
	@echo "  make shell            interactive build container"
	@echo "  make clean            remove build outputs"

# ---------------------------------------------------------------- pins
# Fetch every binary input from the private pins repo and drop it at the paths
# build.sh already reads, so build.sh needs no knowledge of the split.
#
# Auth: DASH_PINS_TOKEN if set (this is what CI uses), else `gh auth token`.
# Plain git cannot authenticate here -- the repo is private and there is no
# credential helper on the dev machine.
pins:
	@echo "== pins: $(PINS_REPO) @ $(PINS_REF)"
	@tok=""; \
	if [ -n "$${DASH_PINS_TOKEN:-}" ]; then tok="$${DASH_PINS_TOKEN}"; \
	elif command -v gh >/dev/null 2>&1 && gh auth token >/dev/null 2>&1; then \
		tok="$$(gh auth token)"; \
	fi; \
	rm -rf "$(PINS_DIR)"; \
	if [ -n "$$tok" ]; then \
		git -c advice.detachedHead=false clone -q --depth 1 --branch "$(PINS_REF)" \
			"https://x-access-token:$$tok@github.com/$(PINS_REPO).git" "$(PINS_DIR)"; \
	else \
		echo "   no token; cloning $(PINS_REPO) unauthenticated (will fail if private)" >&2; \
		git -c advice.detachedHead=false clone -q --depth 1 --branch "$(PINS_REF)" \
			"https://github.com/$(PINS_REPO).git" "$(PINS_DIR)"; \
	fi
	@# Copy only the input directories; the pins repo's own README/.gitignore stay put.
	@tar -C "$(PINS_DIR)" -cf - base-kernel base-diag third-party recovery \
		| tar -C "$(HERE)" -xf -
	@echo "   pins materialised into $(HERE)"
	@$(MAKE) --no-print-directory unpack
	@$(MAKE) --no-print-directory verify

# ---------------------------------------------------------------- unpack
# Each raw input is a file target on its .gz twin, so this is a no-op whenever
# the raw files are already current.
unpack: $(RAW_INPUTS)
define UNPACK
$(1): $(1).gz
	@printf 'unpack %s\n' "$$(notdir $$<)"
	@gzip -dc "$$<" > "$$@"
endef
$(foreach f,$(RAW_INPUTS),$(eval $(call UNPACK,$(f))))

# ---------------------------------------------------------------- env
# The build environment is an artifact with a content digest, not a recipe to
# re-execute: the same Containerfile text can yield different packages once
# Ubuntu's archive moves on. So the image is pinned by digest in
# BUILD-IMAGE.lock and `env-bump` is the only thing that may change it.
#
# A mismatch means the local image is not the pinned one. That is fatal by
# default -- a build from an unpinned toolchain is not a build of the version.
# Override deliberately with ALLOW_IMAGE_DRIFT=1, or fix it properly:
#     make env-bump            (rebuild, republish, re-pin)  -- needs ghcr login
#     podman rmi $(BUILD_IMAGE) && make env    (drop the stale local copy)
env:
	@if podman image exists $(BUILD_IMAGE) >/dev/null 2>&1; then \
		cur=$$($(HERE)/build.sh toolchain-fp $(BUILD_IMAGE)); \
		want=$$(sed -n 's/^toolchain: //p' $(LOCK) 2>/dev/null); \
		if [ -n "$$want" ] && [ "$$cur" != "$$want" ]; then \
			if [ -n "$${ALLOW_IMAGE_DRIFT:-}" ]; then \
				echo "WARNING: $(BUILD_IMAGE) is not the pinned toolchain, continuing anyway"; \
				echo "         local: $$cur"; \
				echo "         lock:  $$want"; \
			else \
				echo "ERROR: $(BUILD_IMAGE) does not match BUILD-IMAGE.lock" >&2; \
				echo "       local: $$cur" >&2; \
				echo "       lock:  $$want" >&2; \
				echo >&2; \
				echo "  The build environment is pinned, so a version is only reproducible" >&2; \
				echo "  against one toolchain. Fix it either way:" >&2; \
				echo >&2; \
				echo "    make env-bump    rebuild, republish and re-pin (needs ghcr login)" >&2; \
				echo "    podman rmi $(BUILD_IMAGE) && make env    drop the stale local image" >&2; \
				echo "    ALLOW_IMAGE_DRIFT=1 make build       proceed anyway, knowing it is unpinned" >&2; \
				exit 1; \
			fi; \
		fi; \
		echo "== env image present: $(BUILD_IMAGE)"; \
	else \
		pub=$$(sed -n 's/^published: //p' $(LOCK) 2>/dev/null); \
		dig=$$(sed -n 's/^digest: //p' $(LOCK) 2>/dev/null); \
		img=$$(sed -n 's/^image: //p' $(LOCK) 2>/dev/null); \
		if [ "$$pub" = "yes" ] && [ -n "$$dig" ] && [ -n "$$img" ]; then \
			echo "== pulling pinned env image $$img@$$dig"; \
			podman pull "$$img@$$dig" >/dev/null || \
				{ echo "pull failed; log in first: podman login ghcr.io" >&2; exit 1; }; \
			podman tag "$$img@$$dig" $(BUILD_IMAGE); \
		else \
			echo "== building env image: $(BUILD_IMAGE)"; \
			podman build -q -t $(BUILD_IMAGE) -f $(RECIPE) $(HERE); \
			$(HERE)/build.sh lock-env $(BUILD_IMAGE) > $(LOCK); \
			echo "   pinned in BUILD-IMAGE.lock (not published; run make env-bump to publish)"; \
		fi; \
	fi

# Deliberate, reviewed act: rebuild the toolchain, publish it, re-pin the digest.
# This is the only supported way BUILD-IMAGE.lock changes.
#
# Registry paths are lowercase -- a GitHub login is not.
env-bump:
	@echo "== rebuilding env image: $(BUILD_IMAGE)"
	@podman build -q -t $(BUILD_IMAGE) -f $(RECIPE) $(HERE)
	@echo "== publishing to ghcr.io (podman login ghcr.io first)"
	@podman push $(BUILD_IMAGE) ghcr.io/h4rryp0tt3r/dash-build:$(IMAGE_TAG)
	@$(HERE)/build.sh lock-env $(BUILD_IMAGE) yes > $(LOCK)
	@cat $(LOCK)
	@echo "== commit BUILD-IMAGE.lock in a PR; that bump is part of a version"

bootstrap: pins env

# ---------------------------------------------------------------- build
# verify is a prerequisite, not a suggestion: these are the bytes that get
# dd-ed to the device, and they arrive from another repository over a network.
# verify is not a separate prerequisite here: `pins` already verifies everything
# it just fetched, and build.sh re-verifies in its own preflight so a direct
# `./build.sh build` cannot skip it. Listing it a third time only prints the same
# seven lines twice.
build: pins unpack env
	@$(HERE)/build.sh build

# ---------------------------------------------------------------- test
# rustc --test runs the renderer's own unit tests. No crates, no Cargo, no
# extra packages in the build image.
test: env
	@$(HERE)/build.sh test "$(HERE)/src"

verify:
	@echo "== pinned inputs"
	@$(HERE)/build.sh verify \
		$(HERE)/base-kernel $(HERE)/base-diag \
		$(HERE)/third-party/busybox $(HERE)/third-party/runit \
		$(HERE)/third-party/eink-firmware $(HERE)/recovery

fingerprint: env
	@$(HERE)/build.sh fingerprint $(ART)/dash-rootfs.img

# Build a second time and prove the image CONTENT is identical, i.e. that the
# tracked inputs fully determine what lands in the artifact. The rootfs image is
# content-reproducible but NOT byte-reproducible: mke2fs 1.47 ignores
# SOURCE_DATE_EPOCH, so timestamps differ. Content must not.
#
# The stronger claim -- that the fingerprint of a tagged release is the one
# recorded in that tag's annotation -- is what CI asserts after the build.
rebuild-check: build
	@echo "== building a second time to compare content"
	@cp $(ART)/dash-rootfs.img /tmp/dash-rebuild-check.img
	@$(HERE)/build.sh build >/dev/null
	@$(HERE)/build.sh fingerprint /tmp/dash-rebuild-check.img \
		$(ART)/dash-rootfs.img | tail -1
	@rm -f /tmp/dash-rebuild-check.img

shell: env
	@podman run --rm -it -v $(HERE):/work -w /work $(BUILD_IMAGE) bash

current:
	@cat $(HERE)/VERSION 2>/dev/null || echo "no VERSION file"

clean:
	@rm -rf $(HERE)/build $(PINS_DIR)
	@rm -f $(ART)/*.build.log
	@echo "removed build outputs (including .pins; run 'make pins' to refetch)"
