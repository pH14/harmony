<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# guest-images

Image recipes for the PostgreSQL, Docker, and k3s guest workloads. Each
`build-*.sh` assembles one initramfs; the `*-init.sh` scripts and the kernel
config fragment are the payloads it installs into the guest root.

They run on Linux and reuse the guest platform's shared build library, kernel
patches, and pinned versions under `consonance/harmony-linux/linux/`. Build them
through the platform Makefile:

```
make -C consonance/harmony-linux/linux postgres-image
make -C consonance/harmony-linux/linux docker-image
make -C consonance/harmony-linux/linux k3s-image
make -C consonance/harmony-linux/linux arm64-postgres-image
```

`arm64-postgres-config-fragment` is read by the platform kernel builder when
`ARM64_KERNEL_PROFILE=postgres` selects the container-capable arm64 kernel.
