# Guest Linux image (task 04 Part B). Linux-only; on macOS run inside a
# linux/amd64 container (docs/BUILDING.md). The heavy lifting lives in the
# *.sh scripts next to this Makefile.

.PHONY: kernel initramfs image postgres-image docker-image k3s-image test test-linux clean-artifacts

kernel:
	./build-kernel.sh

initramfs:
	./build-initramfs.sh

image: kernel initramfs

# Task 37 bare-Postgres workload initramfs (root-only; reuses the kernel built by
# `make kernel`). Produces guest/build/initramfs-postgres.cpio.gz.
postgres-image:
	./build-postgres-image.sh

# Task 38 Postgres-in-Docker workload initramfs (root-only; reuses the kernel
# built by `make kernel`). Produces guest/build/initramfs-docker.cpio.gz.
docker-image:
	./build-docker-image.sh

# Task 49 Postgres-on-k3s workload initramfs (root-only; reuses the kernel built
# by `make kernel`). Produces guest/build/initramfs-k3s.cpio.gz.
k3s-image:
	./build-k3s-image.sh

test:
	./run-tests.sh

test-linux: test

clean-artifacts:
	./clean-artifacts.sh
