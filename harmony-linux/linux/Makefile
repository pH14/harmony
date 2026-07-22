# Guest Linux image (task 04 Part B). Linux-only; on macOS run inside a
# linux/amd64 container (docs/BUILDING.md). The heavy lifting lives in the
# *.sh scripts next to this Makefile.

.PHONY: kernel initramfs image arm64-kernel arm64-initramfs arm64-image exec-image postgres-image campaign-image docker-image k3s-image game-image maze-image test test-linux clean-artifacts

kernel:
	./build-kernel.sh

initramfs:
	./build-initramfs.sh

image: kernel initramfs

# AA-5(c) artifacts build natively on the pinned Linux/aarch64 Altra box and
# land under harmony-linux/build/arm64/, isolated from the established x86 image.
arm64-kernel:
	./build-arm64-kernel.sh

arm64-initramfs:
	./build-arm64-initramfs.sh

arm64-image: arm64-kernel arm64-initramfs

# Task 81 exec-capable initramfs: static busybox + an interactive root shell on
# ttyS0 (`exec-init.sh`), so the `exec` improvisation channel has a shell to inject
# commands into. Produces harmony-linux/build/initramfs-exec.cpio.gz — used by the
# improvisation box gate (strict output mode is forced on this image). Off-record
# test scaffolding (see exec-init.sh). Depends on `kernel` (like the composite
# `image` target) so the documented single-command build works from a clean
# checkout — the box gate needs both bzImage and this initramfs.
exec-image: kernel
	./build-exec-image.sh

# Task 37 bare-Postgres workload initramfs (root-only; reuses the kernel built by
# `make kernel`). Produces harmony-linux/build/initramfs-postgres.cpio.gz.
postgres-image:
	./build-postgres-image.sh

# Task 60 Postgres-campaign workload initramfs: the bare-Postgres image plus the
# planted-bug supervisor `campaign-super` (root-only; reuses the kernel built by
# `make kernel`). Produces harmony-linux/build/initramfs-campaign.cpio.gz.
campaign-image:
	./build-campaign-image.sh

# Task 38 Postgres-in-Docker workload initramfs (root-only; reuses the kernel
# built by `make kernel`). Produces harmony-linux/build/initramfs-docker.cpio.gz.
docker-image:
	./build-docker-image.sh

# Task 49 Postgres-on-k3s workload initramfs (root-only; reuses the kernel built
# by `make kernel`). Produces harmony-linux/build/initramfs-k3s.cpio.gz.
k3s-image:
	./build-k3s-image.sh

# Task 86 SMB game workload initramfs: static busybox + the commit-pinned
# FCEUmm libretro core (GPL-2.0-or-later, built from the verified tarball) +
# the user-supplied ROM (HARMONY_SMB_ROM=<path>; unset ⇒ loud SKIP, ROM-less
# image). No root needed. Produces harmony-linux/build/initramfs-game.cpio.gz and the
# shared task-87 renderer core harmony-linux/build/fceumm_libretro.so.
game-image:
	./build-game-image.sh

# Task 134 maze workload initramfs: static busybox + the static-musl maze
# agent (harmony-linux/maze-agent — the deterministic gauntlet walk over X/Y
# state registers). No ROM, no root needed. Produces
# harmony-linux/build/initramfs-maze.cpio.gz.
maze-image:
	./build-maze-image.sh

test:
	./run-tests.sh

test-linux: test

clean-artifacts:
	./clean-artifacts.sh
