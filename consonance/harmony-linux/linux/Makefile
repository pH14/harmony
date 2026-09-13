# Guest Linux image. Linux-only; on macOS run inside a
# linux/amd64 container (CONTRIBUTING.md). The heavy lifting lives in the
# *.sh scripts next to this Makefile.

.PHONY: kernel initramfs image arm64-kernel arm64-initramfs arm64-image arm64-nova-kernel arm64-nova-image arm64-postgres-kernel arm64-postgres-image arm64-oci-image exec-image postgres-image campaign-image docker-image k3s-image game-image nova-game-image test test-linux clean-artifacts

kernel:
	./build-kernel.sh

.PHONY: go-runtime-image
go-runtime-image:
	./build-go-runtime-image.sh

initramfs:
	./build-initramfs.sh

image: kernel initramfs

# AA-5(c) artifacts build natively on the validated Linux/aarch64 host and
# land under consonance/harmony-linux/build/arm64/, isolated from the established x86 image.
arm64-kernel:
	./build-arm64-kernel.sh

arm64-initramfs:
	./build-arm64-initramfs.sh

arm64-image: arm64-kernel arm64-initramfs

arm64-nova-kernel:
	ARM64_KERNEL_PROFILE=nova ./build-arm64-kernel.sh

arm64-nova-image: arm64-nova-kernel
	../../../workloads/nes-guest/legacy/build-arm64-nova-image.sh

arm64-postgres-kernel:
	ARM64_KERNEL_PROFILE=postgres ./build-arm64-kernel.sh

arm64-postgres-image: arm64-postgres-kernel
	../../../workloads/guest-images/build-arm64-postgres-image.sh

# OCI-runner initramfs for `harmony oci run`: static busybox only, no /init
# (the CLI injects the bundle + init as a second initramfs segment).
arm64-oci-image:
	./build-arm64-oci-image.sh

# Depends on `kernel` so the documented single-command build works from a clean
# checkout: the box gate needs both bzImage and this initramfs.
exec-image: kernel
	./build-exec-image.sh

postgres-image:
	../../../workloads/guest-images/build-postgres-image.sh

campaign-image:
	./build-campaign-image.sh

docker-image:
	../../../workloads/guest-images/build-docker-image.sh

k3s-image:
	../../../workloads/guest-images/build-k3s-image.sh

game-image:
	../../../workloads/nes-guest/legacy/build-game-image.sh

nova-game-image:
	../../../workloads/nes-guest/legacy/build-nova-game-image.sh

test:
	./run-tests.sh

test-linux: test

clean-artifacts:
	./clean-artifacts.sh
