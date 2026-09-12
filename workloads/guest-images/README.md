<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# guest-images

This package owns application guest image recipes, their pins, and their
downloads. Each `build-*.sh` assembles one initramfs; the init scripts,
supervisors, and workload kernel fragments are the payloads it installs into
the guest root. The platform package owns the kernel and the workload-free OCI
runtime separately.

The recipes run on Linux and reuse the guest platform's shared build library.
Fetch platform and package inputs, then invoke this package's Makefile:

```
make -C workloads/guest-images fetch
make -C workloads/guest-images postgres-image
make -C workloads/guest-images docker-image
make -C workloads/guest-images k3s-image
make -C workloads/guest-images arm64-postgres-image
```

`arm64-postgres-config-fragment` is consumed by the arm64 workload image
recipe. The platform kernel is built through its standard runtime profile;
workload packages do not select a named platform kernel profile.
