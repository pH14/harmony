# Task 49 — Postgres on lightweight Kubernetes, client pod → server pod, intra-guest networking

> **Integrator directive (2026-06-26).** The rung after runc+Postgres (task 48): run Postgres in a
> **lightweight Kubernetes distribution** inside the deterministic guest, with a **client pod making
> calls to the Postgres pod from a separate pod** — and prove it **deterministic-twice**. Networking
> stays **intra-guest** (pod-to-pod over the in-guest CNI); it does **not** route through the
> hypervisor host, so **pv-net is explicitly out of scope** — this is one guest VM running a
> single-node cluster, not multiple VMs bridged across the host. This is the determinism stress test
> at full stack height: kubelet + containerd + API server + scheduler + datastore + CNI are all
> Go/multi-goroutine services that busy-spin and depend on preemption, plus a dense web of timers,
> watches, leases, and UID/timestamp generation — every one of which must collapse onto V-time and
> the seeded CRNG, twice, bit-identically.
>
> **Depends on:** task 47 (the deterministic preemption timer — `run_until` Phase 2; PR #15) **and**
> task 48 (runc runs the Postgres OCI container, deterministic-twice — task 47's documented gate-3
> frontier). **DO NOT AUTO-SPAWN** until both are merged: k8s is strictly heavier than bare runc and
> cannot make progress without the preemption primitive, and it builds directly on the container-runtime
> path task 48 establishes. **Box-only** (patched KVM + Intel PMU); pin per `docs/BOX-PINNING.md`;
> self-serve box gates via git; ALWAYS revert KVM to stock + verify.

Read `tasks/00-CONVENTIONS.md`, `tasks/47-deterministic-preemption-timer.md` +
`consonance/vmm-core/IMPLEMENTATION.md` (the preemption primitive + its gate-3 frontier notes), task 48
(runc+Postgres), and `harmony-linux/linux/` (the image-build + workload path: `build-postgres-image.sh`,
`pg-init.sh`, the task-42 UUID/time workload) before writing anything.

## Topology (what to build)

A **single** guest VM, single-vCPU, running a **single-node** lightweight Kubernetes cluster with two
workloads:

- **`postgres` pod** — the task-37/38 Postgres image (the task-42 `gen_random_uuid()` + time workload
  schema), exposed in-cluster as a `Service` (ClusterIP) or reached by pod IP.
- **`client` pod** — a separate pod that connects to the Postgres pod **intra-guest** (pod-to-pod over
  the cluster CNI), runs the workload (insert/select loop streaming UUIDs + timestamps + a running
  aggregate), and writes the result to a place the harness captures (stdout → pod log → `ttyS0`, the
  task-37 serial path).

**Networking is intra-guest only.** Pod-to-pod traffic transits the guest kernel's CNI (bridge + veth +
netns) — all inside the one VM, driven by V-time. **Do not** use or build pv-net (that is the
multi-VM-across-host L2 switch; not needed and out of scope). DNS-based Service discovery (CoreDNS) is
fine intra-guest; the client may also target the ClusterIP/pod IP directly to minimize moving parts.

## Distribution

Use a **lightweight single-binary distro** — **k3s** is the recommended default (single Go binary,
bundled containerd, **sqlite**-backed datastore rather than etcd, flannel CNI). Trim what the gate
doesn't need (`--disable traefik,servicelb,metrics-server`; keep CoreDNS only if you use DNS service
discovery). k0s/microk8s are acceptable if you justify the choice in `IMPLEMENTATION.md`. The datastore
(sqlite) lives on the RAM-backed ext4 (tasks 36–38), so its writes are deterministic VM-memory writes.

## Determinism (the whole point — and where it gets hard)

- **Preemption is load-bearing.** k3s runs *many* Go processes (apiserver, scheduler,
  controller-manager, kubelet, kube-proxy, containerd, CoreDNS, the CNI) — far more busy-spinning,
  multi-goroutine concurrency than bare runc. Without task 47's `run_until` preemption they starve. This
  is the primitive's hardest real-world exercise.
- **Time → V-time.** k8s reads wall-clock everywhere (resource timestamps, lease/lease-renewal,
  event times, backoff timers, readiness/liveness probe schedules). All must route through V-time;
  any escape is a determinism leak — report it, don't widen a tolerance.
- **UIDs/randomness → seeded CRNG.** k8s mints UIDs and tokens constantly (object UIDs, SA tokens,
  the Postgres `gen_random_uuid()` workload). All must land on the seeded CRNG (task 37's path):
  identical across two same-seed runs, different across seeds.
- **Readiness without preemption-free busy-poll.** Like Postgres in tasks 37/38, gate the cluster /
  pod readiness cooperatively (or via the now-working timer-driven probes), not a host busy-poll.
- **sqlite datastore** on RAM-ext4 → deterministic; confirm no `O_DIRECT`/fsync path escapes to
  nondeterminism.

## Acceptance gates (box)

1. **Cluster up:** k3s reaches a Ready single-node cluster on the deterministic patched backend; quote
   the boot→Ready V-time and that it is identical across two same-seed runs.
2. **Both pods Running:** `postgres` and `client` pods schedule + reach Running; the Postgres pod
   accepts connections.
3. **Intra-guest client→server call:** the client pod connects to the Postgres pod **over the
   in-guest CNI** (no host networking; pv-net unused — state how you verified the path stayed
   intra-guest), runs the UUID/time workload, and streams it to `ttyS0`.
4. **Deterministic-twice:** two same-seed patched runs are **bit-identical** — serial (incl. the
   streamed UUIDs/timestamps) **and** `state_hash`. Quote the equal digests + a sample UUID/timestamp.
5. **Seed-sensitivity:** a different seed yields different UUIDs / interleaving (quote both).
6. **No regression:** M1/M2/P6 + acceptance-suite + unison goldens byte-identical; standard gates green;
   revert KVM to stock `1396736` + verify.

**If the Go-heavy k8s stack surfaces a genuinely NEW blocker beyond preemption** (something the task-47
primitive does not resolve — a syscall the backend doesn't determinize, a nondeterministic readiness
path, a CNI timing dependency), implement as far as the primitive carries, prove the gates that are
reachable, and **document the precise next blocker as the frontier** — do not fake or relax a gate.

## Non-goals

pv-net / host-routed networking / multiple VMs (explicitly excluded — networking is intra-guest).
Multi-node clusters, HA control plane, real etcd. Ingress/LoadBalancer (ClusterIP/pod-IP only).
Changing the determinization mechanism (RDTSC/RDRAND/V-time are unchanged — this *exercises* them).
No CPU/MSR contract or `state_hash` schema change.
