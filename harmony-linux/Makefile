# harmony-linux entry points. Linux images build only on Linux; the portable
# acceptance payloads live under consonance/acceptance-suite (R-L4).

UNAME_S := $(shell uname -s)

.PHONY: test test-linux test-libvoidstar libvoidstar fetch kernel initramfs image arm64-kernel arm64-initramfs arm64-image postgres-image campaign-image docker-image game-image clean

test: test-libvoidstar test-linux

libvoidstar:
	$(MAKE) -C libvoidstar all

test-libvoidstar:
	$(MAKE) -C libvoidstar check

test-linux:
ifeq ($(UNAME_S),Linux)
	$(MAKE) -C linux test
else
	@echo 'FAIL: harmony-linux images build and test only on Linux.' >&2
	@echo '      Run it in a linux/amd64 container — see docs/BUILDING.md (from the repo root):' >&2
	@echo '        docker run --rm -it --platform linux/amd64 -v "$$PWD":/work -w /work debian:stable bash' >&2
	@echo '        # inside: apt-get update && apt-get install -y <deps from harmony-linux/README.md>' >&2
	@echo '        make -C harmony-linux test-linux' >&2
	@exit 1
endif

# Download + hash-verify the pinned kernel and busybox tarballs into harmony-linux/dl/.
# Works on any host; afterwards no network access is needed.
fetch:
	scripts/fetch.sh

kernel initramfs image arm64-kernel arm64-initramfs arm64-image postgres-image campaign-image docker-image game-image maze-image:
ifeq ($(UNAME_S),Linux)
	$(MAKE) -C linux $@
else
	@echo "FAIL: 'make $@' needs Linux — see docs/BUILDING.md for the container invocation." >&2
	@exit 1
endif

clean:
	$(MAKE) -C libvoidstar clean
	rm -rf build
