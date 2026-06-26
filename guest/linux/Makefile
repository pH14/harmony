# Guest Linux image (task 04 Part B). Linux-only; on macOS run inside a
# linux/amd64 container (docs/BUILDING.md). The heavy lifting lives in the
# *.sh scripts next to this Makefile.

.PHONY: kernel initramfs image test test-linux clean-artifacts

kernel:
	./build-kernel.sh

initramfs:
	./build-initramfs.sh

image: kernel initramfs

test:
	./run-tests.sh

test-linux: test

clean-artifacts:
	./clean-artifacts.sh
