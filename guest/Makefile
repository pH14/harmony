# Task 04 entry points. Part A (bare-metal payloads) runs on macOS or Linux
# with QEMU; Part B (minimal Linux guest image) builds only on Linux — on
# macOS run it in a linux/amd64 container, see docs/BUILDING.md.

UNAME_S := $(shell uname -s)

.PHONY: test test-payloads test-linux fetch kernel initramfs image postgres-image clean

test: test-payloads test-linux

test-payloads:
	payloads/run-tests.sh

test-linux:
ifeq ($(UNAME_S),Linux)
	$(MAKE) -C linux test
else
	@echo 'FAIL: Part B (guest Linux image) builds and tests only on Linux.' >&2
	@echo '      Run it in a linux/amd64 container — see docs/BUILDING.md (from the repo root):' >&2
	@echo '        docker run --rm -it --platform linux/amd64 -v "$$PWD":/work -w /work debian:stable bash' >&2
	@echo '        # inside: apt-get update && apt-get install -y <deps from guest/README.md>' >&2
	@echo '        make -C guest test-linux' >&2
	@exit 1
endif

# Download + hash-verify the pinned kernel and busybox tarballs into guest/dl/.
# Works on any host; afterwards no network access is needed.
fetch:
	scripts/fetch.sh

kernel initramfs image postgres-image:
ifeq ($(UNAME_S),Linux)
	$(MAKE) -C linux $@
else
	@echo "FAIL: 'make $@' needs Linux — see docs/BUILDING.md for the container invocation." >&2
	@exit 1
endif

clean:
	rm -rf payloads/target build
