<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# guest-images

This package owns application image recipes, their pins, and their downloads.
Each workload recipe assembles one deterministic OCI layout. The platform
package owns the kernel, the workload-free runtime initramfs, guest mounts,
and process termination separately. A workload image never selects a platform
kernel profile or provides the guest PID 1 script.

The recipes run on Linux and reuse the guest platform's shared build library.
Fetch platform and package inputs, then invoke this package's Makefile:

```
make -C workloads/guest-images fetch
make -C workloads/guest-images postgres-image
make -C workloads/guest-images docker-image
make -C workloads/guest-images k3s-image
make -C workloads/guest-images arm64-postgres-image
```

The recipes write these layouts below the standard build artifact directory:

| Recipe | OCI layout |
| --- | --- |
| `postgres-image` | `build/oci-images/postgres.oci` |
| `campaign-image` | `build/oci-images/campaign.oci` |
| `order-image` | `build/oci-images/order.oci` |
| `uuid-image` | `build/oci-images/uuid.oci` |
| `docker-image` | `build/oci-images/docker.oci` |
| `k3s-image` | `build/oci-images/k3s.oci` |
| `arm64-postgres-image` | `build/aarch64/oci-images/postgres.oci` |

Each layout contains a single deterministic layer and an OCI config whose
entrypoint is the application workload. The outer platform runtime supplies
`/proc`, `/dev`, `/sys`, `/run`, and `/tmp`, runs the entrypoint as the OCI
process, and owns signals and the final VM terminal. PostgreSQL benchmark
variants share `postgres-workload.sh`; the campaign, ordering, and UUID
supervisors remain separate payloads so their fault behavior is preserved.

The Docker and K3s recipes retain nested container software and their
application setup. They require privileges for nested namespaces, cgroups,
network devices, and netfilter rules; the standard platform OCI spec does not
yet qualify those nested paths on every host. The recipes report failures from
the nested runtime instead of selecting an alternate outer launcher. The ARM
PostgreSQL image is built natively with its LSE-only binary scans; a native
ARM host is required for that recipe.

The arm64 platform kernel recipe owns its generic namespace and filesystem
configuration. Workload packages do not select a named platform kernel profile.
