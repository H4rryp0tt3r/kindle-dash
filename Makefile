# Dash OS -- build entry points
#
#   make build            build artifacts/ (ensures inputs + env as needed)
#   make bootstrap        prepare only: unpack inputs + build the env image
#   make env              build the build-environment container image
#   make unpack           expand pinned .gz inputs (idempotent)
#   make verify           check pinned inputs against their SHA256SUMS
#   make fingerprint      print the rootfs content fingerprint
#   make rebuild-check    build twice and prove content reproducibility
#   make shell            interactive build container
#   make clean            remove build outputs
#
# A version is a tagged commit: `git checkout <version> && make build` reproduces it.
# Built images are NOT committed -- see .gitignore.

SHELL := /bin/bash
HERE  := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
ART   := $(HERE)/artifacts
RECIPE := $(HERE)/Containerfile
LOCK   := $(HERE)/BUILD-IMAGE.lock

# The env image tag is derived from the Containerfile's sha256, so editing the
# Containerfile changes the tag and transparently forces a rebuild.
IMAGE_TAG := $(shell (command -v sha256sum >/dev/null 2>&1 \
	&& sha256sum $(RECIPE) || shasum -a 256 $(RECIPE)) | cut -c1-12)
BUILD_IMAGE := localhost/dash-build:$(IMAGE_TAG)
export DASH_BUILD_IMAGE := $(BUILD_IMAGE)

# Raw (decompressed) forms of the pinned inputs. `unpack` materializes them; the
# build and flashing steps consume them directly.
RAW_INPUTS := \
	$(HERE)/base-kernel/main-uImage \
	$(HERE)/base-diag/diag-uImage \
	$(HERE)/third-party/busybox/busybox \
	$(HERE)/third-party/runit/runit

.PHONY: help bootstrap env unpack build verify fingerprint rebuild-check shell clean current

help:
	@echo "Dash OS -- version $$(cat $(HERE)/VERSION 2>/dev/null || echo none)"
	@echo ""
	@echo "  make build            build artifacts/ (ensures inputs + env)"
	@echo "  make bootstrap        prepare only: unpack inputs + build the env image"
	@echo "  make env              build the build-environment image"
	@echo "  make unpack           expand pinned .gz inputs (idempotent)"
	@echo "  make verify           verify the pinned inputs (SHA256SUMS)"
	@echo "  make fingerprint      print the rootfs content fingerprint"
	@echo "  make rebuild-check    rebuild and prove content reproducibility"
	@echo "  make shell            interactive build container"
	@echo "  make clean            remove build outputs"

# ---------------------------------------------------------------- unpack
# Each raw input is a file target on its committed .gz twin, so this is a no-op
# whenever the raw files are already current.
unpack: $(RAW_INPUTS)
define UNPACK
$(1): $(1).gz
	@printf 'unpack %s\n' "$$(notdir $$<)"
	@gzip -dc "$$<" > "$$@"
endef
$(foreach f,$(RAW_INPUTS),$(eval $(call UNPACK,$(f))))

# ---------------------------------------------------------------- env
# Idempotent: the tag encodes the Containerfile hash, so this only builds when
# the recipe is new.
env:
	@if podman image exists $(BUILD_IMAGE) >/dev/null 2>&1; then \
		cur=$$(podman image inspect --format '{{.Id}}' $(BUILD_IMAGE)); \
		if [ -f $(LOCK) ] && [ "$$cur" != "$$(cat $(LOCK))" ]; then \
			echo "WARNING: local $(BUILD_IMAGE) differs from committed BUILD-IMAGE.lock"; \
			echo "         local: $$cur"; \
			echo "         lock:  $$(cat $(LOCK))"; \
		fi; \
		echo "== env image present: $(BUILD_IMAGE)"; \
	else \
		echo "== building env image: $(BUILD_IMAGE)"; \
		podman build -t $(BUILD_IMAGE) -f $(RECIPE) $(HERE); \
		podman image inspect --format '{{.Id}}' $(BUILD_IMAGE) > $(LOCK); \
	fi

bootstrap: unpack env

# ---------------------------------------------------------------- build
build: unpack env
	@$(HERE)/build.sh build

verify:
	@echo "== pinned inputs"
	@$(HERE)/build.sh verify \
		$(HERE)/base-kernel $(HERE)/base-diag \
		$(HERE)/third-party/busybox $(HERE)/third-party/runit \
		$(HERE)/third-party/eink-firmware

fingerprint: env
	@$(HERE)/build.sh fingerprint $(ART)/dash-rootfs.img

# Build a second time and prove the image CONTENT is identical, i.e. that the
# tracked inputs fully determine what lands in the artifact. The rootfs image is
# content-reproducible but NOT byte-reproducible: mke2fs 1.47 ignores
# SOURCE_DATE_EPOCH and no local image has e2fsprogs >= 1.48, so timestamps
# differ. Content must not.
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
	@rm -rf $(HERE)/build
	@rm -f $(ART)/*.build.log
	@echo "removed build outputs"
