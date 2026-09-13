# Guest Linux image. Linux-only; on macOS run inside a
# linux/amd64 container (CONTRIBUTING.md). The heavy lifting lives in the
# *.sh scripts next to this Makefile.

.PHONY: kernel initramfs image arm64-kernel arm64-initramfs arm64-image oci-runtime-image arm64-oci-image exec-image go-runtime-image test test-linux clean-artifacts

kernel:
	./build-kernel.sh

.PHONY: go-runtime-image
go-runtime-image:
	./build-go-runtime-image.sh

initramfs:
	./build-initramfs.sh

image: kernel initramfs

# AA-5(c) artifacts build natively on the validated Linux/aarch64 host and
# land under consonance/harmony-linux/build/aarch64/, isolated from x86.
arm64-kernel:
	./build-arm64-kernel.sh

arm64-initramfs:
	./build-arm64-initramfs.sh

arm64-image: arm64-kernel arm64-initramfs

# Workload-free OCI runtime artifacts. Each target requires the platform-owned
# init and supervisor paths documented by build-oci-runtime-initramfs.sh.
oci-runtime-image: kernel
	./build-oci-runtime-initramfs.sh x86_64

arm64-oci-image: arm64-kernel
	./build-oci-runtime-initramfs.sh aarch64

# Depends on `kernel` so the documented single-command build works from a clean
# checkout: the box gate needs both bzImage and this initramfs.
exec-image: kernel
	./build-exec-image.sh

test:
	./run-tests.sh

test-linux: test

clean-artifacts:
	./clean-artifacts.sh
