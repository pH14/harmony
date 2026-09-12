<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# guest-images

Image recipes for the PostgreSQL, Docker, and k3s guest workloads. Each
`build-*.sh` script assembles one initramfs; the `*-init.sh` scripts and the
kernel config fragment are the payloads those scripts install into the guest
root.

The scripts run on Linux and reuse the guest platform's shared build library,
kernel patches, and pinned versions under `consonance/harmony-linux/linux/`.
Each one resolves that directory and changes into it before sourcing
`lib-build.sh`, so every platform-relative path inside behaves as it does for
the platform's own builders. `$workload_dir` points back here, for the payload
files the recipe installs.

Build them through the platform Makefile, which is the documented entry point:

```
make -C consonance/harmony-linux/linux postgres-image
make -C consonance/harmony-linux/linux docker-image
make -C consonance/harmony-linux/linux k3s-image
make -C consonance/harmony-linux/linux arm64-postgres-image
```

`arm64-postgres-config-fragment` is read by the platform kernel builder when
`ARM64_KERNEL_PROFILE=postgres` selects the container-capable arm64 kernel.
