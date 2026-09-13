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
application setup. K3s also builds pinned iptables 1.8.11 from source with a
musl static compiler and packages the `iptables`, `iptables-restore`, and
related legacy aliases from one self-contained executable; the nftables backend
and dynamic extension closure are disabled. Set
`HARMONY_K3S_IPTABLES_CC` when the host's `musl-gcc` is not on `PATH`.

The nested recipes require the platform's delegated cgroup-v2 contract: an
empty namespace root, the workload at `/runtime`, and a writable cgroup mount.
Docker's nested spec uses the absolute `/pg-container` child path and does not
set controller resources, so it does not require a particular controller set.
K3s validates and uses `cpu`, `cpuset`, and `memory`, and its agent requires
`pids`; K3s creates its own pod and container leaves below the same delegated
root. Both workloads fail before launch when their required contract is absent,
and retain the privileges needed for nested namespaces, cgroups, network
devices, and netfilter rules. The ARM PostgreSQL image is built natively with
its LSE-only binary scans; a native ARM host is required for that recipe.

The K3s recipe builds the pinned upstream source through the Antithesis Go
instrumentor before the normal K3s packaging step embeds the server and its
tool modes in the single launcher. The generated coverage metadata and
`libvoidstar.so` are part of the workload image. The embedded server and helper
binaries are dynamically linked against the packaged musl closure so the SDK
can load the bridge. Unconfigured instrumented processes yield through
`/dev/harmony` at the bridge's fixed callback cadence, providing deterministic
scheduling points during exit-free Go execution.

The instrumented scope includes the K3s server and its Kubernetes, containerd,
Kine, etcd, CNI, gRPC, and Go support packages. The build requires coverage
metadata from Kubernetes, containerd, and Kine before packaging. Its Alpine
builder package closure, cloned helper commits, downloaded charts, Go base
image, K3s source, and Go modules are pinned or hash checked. A local patch to
the pinned Antithesis toolexec maps temporary rewrite directories through Go's
`-trimpath`; the K3s source patch applies the same rule to its temporary CNI
build. Two clean compilation-cache builds must produce the same server and CNI
hashes.

The arm64 platform kernel recipe owns its generic namespace and filesystem
configuration. Workload packages do not select a named platform kernel profile.

The scheduled Workload backends acceptance lane runs each nested recipe twice
through the canonical x86 runtime and compares complete serial logs, application
and runtime exit statuses, and readiness evidence. The Docker recipe launches
the official PostgreSQL image with Docker's bundled `runc`; the K3s recipe also
checks pod-to-PostgreSQL traffic through the guest CNI. The lane requires an
exact qualified platform artifact and preserves image hashes and run records.
Run that lane independently on a proposed branch with:

```sh
gh workflow run nova-consonance-experiment.yml --ref YOUR_BRANCH -f suite=nested-runtime
```
