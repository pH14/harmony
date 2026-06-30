# Task 56 — SMP-cpuset k3s bring-up: Postgres on k3s, deterministic-twice (frontier)

## HANDOFF (foreman — next phase is yours)

- **Validated state:** branch `task/smp-cpuset-k3s-bringup` on the box `/root/ht49`, tag
  **`task56-k2-pass`** = commit **`7a5bbad`** (the deterministic-twice-validated source). On top:
  `b62c8f3` (diagnostic cleanup, re-confirmed by k1) and `7fd8479` (this doc). **Not pushed.**
- **Gates (box, patched KVM, reverted to stock 1396736 after each):**
  - **k1** functional — `ok` (788s): k3s cluster Ready → postgres pod (10.42.0.2) + client pod
    (10.42.0.3) over the intra-guest CNI → workload `row|20|20|210|…` → GUEST_READY.
  - **k2** deterministic-twice — `ok` (1573s): `state_hash A==B==226437a3f789abce1487dd8e17fd017524fbc4618e08b1a07611e4ad3bfdf0b2`,
    both runs `steps=1411233`, 20 UUIDs+timestamps bit-identical (seed `0x0028c0ffee5eedc0`).
  - **k3** seed-sensitivity —  (1573s), both seeds GUEST_READY, the workload differs by seed (so it is
    seed-driven, not constant): seed_a UUID `2a50ce18…` t `…35.746785` vs seed_b UUID `6ce4b1fc…`
    t `…29.570056`. 0 skid.
  - **0 skid / no `DIAG-SKID49`** on every run.
- **Fix chain:** SMP idle-HLT keystone (ACPI **MADT** in `linux_loader.rs` + **ARAT** contract v4)
  → kubelet rootfs (tmpfs data dirs) → flannel cni0 MTU (node IP on a **veth**) → runc
  `pivot_root` (containerd **NoPivotRoot** drop-in) → **node-IP** off the pod CIDR (10.0.0.2).
- **Files changed:** `consonance/vmm-core/src/linux_loader.rs`; `docs/cpu-msr-contract.toml` +
  `consonance/vmm-core/src/contract/{mod.rs,testdata/canonical-v4.txt}` + `docs/CPU-MSR-CONTRACT.md`
  (ARAT/v4); `guest/linux/k3s-init.sh`; `consonance/vmm-backend/src/kvm_sys.rs` +
  `consonance/vmm-core/src/vmm.rs` (MTF/0005 wiring + cleanup); guest build inputs under
  `guest/linux/`; `consonance/vmm-backend/kvm-patches/patches/0005-*`.
- **0005 (MTF) kernel patch — exact locations on the box:**
  - Repo (provenance): `consonance/vmm-backend/kvm-patches/patches/0005-KVM-VMX-MTF-deterministic-single-step.patch`
    + `0005-NOTES.md`.
  - Built/loaded module source:
    `/root/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm`
    (built `kvm.ko`/`kvm-intel.ko`, loaded by `/root/run-patched-ht49.sh`); uapi/asm headers under
    `…-common/`. The patch is the diff of that tree vs the in-place 0001-0004 stock tree
    `/root/kvm-spike/deb612/linux-6.12.90`.
- **DO NOT START productionization** (clean 0005 patch series / canonical linux-6.18 port / PRs) —
  that is your next phase. SMP slowness (~1.7×/step) is noted, unaddressed (correctness first).

---

**Status: DONE.** Postgres-on-k3s runs on the SMP guest kernel and is **deterministic-twice**
(bit-identical `state_hash`). This is the task-49 frontier goal, reached on the SMP kernel.

## Result (box-validated, patched KVM, reverted to stock 1396736 after each run)

- **k1** `k1_k3s_cluster_postgres_client_streams_patched` — `test result: ok` (788s). k3s cluster
  Ready → postgres pod (10.42.0.2) + client pod (10.42.0.3) over the **intra-guest CNI** → the
  `gen_random_uuid()/clock_timestamp()` workload streamed (`row|20|20|210|…`) → **GUEST_READY**.
- **k2** `k2_k3s_postgres_deterministic_twice_patched` — `test result: ok` (1573s).
  `state_hash A == state_hash B == 226437a3f789abce1487dd8e17fd017524fbc4618e08b1a07611e4ad3bfdf0b2`,
  both runs `steps=1411233`, `terminal=Shutdown`, GUEST_READY, the 20 UUIDs + timestamps
  bit-identical (seed `0x0028c0ffee5eedc0`).
- **0 skid / no `DIAG-SKID49`** on every run — the determinism engine stayed correct throughout.
- The exact validated source is tagged **`task56-k2-pass`** (commit `7a5bbad`).

## The fix chain (one determinism-engine fix, the rest ordinary guest-env layers)

The repo arrived (STEP 0) with the MTF/0005 KVM module, SMP kernel, networking + cgroup/cpuset
work already done on the box; the run died at a terminal idle-HLT (step 98078). The layers:

1. **SMP idle-HLT (THE determinism keystone).** With `CONFIG_SMP=y` the guest hit a tree-RCU idle
   `HLT` that was classified terminal because the LAPIC timer was never armed. Root cause (Linux
   6.18.35): no ACPI MADT ⇒ `__apic_intr_mode_select` returns `APIC_VIRTUAL_WIRE_NO_CONFIG`
   (gated on `acpi_lapic`); under SMP `native_smp_prepare_cpus` hits
   `case APIC_VIRTUAL_WIRE_NO_CONFIG: disable_smp(); return;` and **never calls
   `setup_percpu_clockev()`** → the LAPIC-timer clockevent is never registered → the periodic tick
   stays on the unmodeled PIT → the idle never wakes. Fixes:
   - **`consonance/vmm-core/src/linux_loader.rs`** — write a minimal RSDP→XSDT→MADT (one
     Processor-Local-APIC entry, no IO-APIC) into the legacy BIOS region `0xE0000` (memslot-backed
     but omitted from usable E820, so no E820 split) and point `boot_params.acpi_rsdp_addr`
     (offset 0x070) at it. MADT Local-APIC addr = `0xFEE00000` (= contract xAPIC base / memslot
     hole), boot-CPU APIC ID 0, static bytes / fixed checksums ⇒ part of the deterministic input.
     This flips the mode to `APIC_VIRTUAL_WIRE` so the SMP timer setup runs.
   - **`docs/cpu-msr-contract.toml` (contract v3→v4)** — advertise **ARAT** (CPUID.06H:EAX[2]=1).
     Honest (the V-time LAPIC timer is always-running); `setup_APIC_timer` then clears
     `CLOCK_EVT_FEAT_C3STOP` + raises the lapic clockevent rating to 150 so it's adopted over the
     PIT. Regenerated golden `testdata/canonical-v4.txt` + `contract_hash`. ARAT alone did NOT fix
     it — the **MADT is the load-bearing change**; the two compose.
2. **kubelet cAdvisor rootfs** (`guest/linux/k3s-init.sh`) — cAdvisor can't stat the initramfs
   ramfs root (`device "rootfs"`) → `failed to get rootfs info` aborts ContainerManager. Mount
   **tmpfs** on `/var/lib/kubelet` + `/var/lib/rancher/k3s/agent/containerd` before k3s starts
   (cAdvisor recognizes tmpfs via statfs); the pre-staged airgap images/manifests stay on the root.
3. **flannel `cni0` bridge `EINVAL`** (`k3s-init.sh`) — the node IP was on `lo` (MTU 65536), so
   flannel (host-gw) derived an invalid `cni0` MTU. Put the node IP on a **veth** (`nodenet0`,
   MTU 1500) ⇒ `FLANNEL_MTU=1500` ⇒ cni0 creates cleanly. (A bridge probe confirmed the kernel +
   large MTU are fine, ruling out a `CONFIG_BRIDGE` gap.)
4. **runc `pivot_root: invalid argument`** (`k3s-init.sh`) — the container rootfs is on the
   initramfs ramdisk (root mount has no parent), so runc's default `pivot_root` EINVALs (the
   task-48 issue). Add a containerd drop-in
   `…/agent/etc/containerd/config-v3.toml.d/10-nopivot.toml` setting **`NoPivotRoot = true`** on
   the runc-v2 runtime options (the shim then passes `--no-pivot` = MS_MOVE+chroot). The drop-in
   path was taken from k3s's auto-generated `config.toml` (`imports = […config-v3.toml.d/*.toml]`).
5. **node-IP / pod-IP collision** (`k3s-init.sh`) — `NODE_IP` was `10.42.0.2`, inside the pod CIDR
   `10.42.0.0/16`, colliding with the first pod IP; the kubelet readiness probe hit the node itself
   (`connection refused`) instead of routing across cni0 to the postgres pod. Moved `NODE_IP` to
   **`10.0.0.2`** (out of the pod/service CIDRs).

## Files changed
- `consonance/vmm-core/src/linux_loader.rs` — ACPI MADT + `boot_params.acpi_rsdp_addr`.
- `docs/cpu-msr-contract.toml`, `consonance/vmm-core/src/contract/{mod.rs,testdata/canonical-v4.txt}`,
  `docs/CPU-MSR-CONTRACT.md` — ARAT, contract v4 (regenerated golden + hash).
- `guest/linux/k3s-init.sh` — tmpfs data dirs, veth node interface, NoPivotRoot drop-in, node IP.
- `consonance/vmm-backend/src/kvm_sys.rs`, `consonance/vmm-core/src/vmm.rs` — MTF/0005 wiring
  (preserved from STEP 0) + cleanup of unused imports / diagnostic scaffolding.
- `consonance/vmm-backend/kvm-patches/patches/0005-*.patch` + `0005-NOTES.md` — the captured 0005
  MTF kernel delta (provenance only; productionizing 0005 is out of scope, see Non-goals).
- Guest build inputs: `guest/linux/{config-fragment,build-kernel.sh,build-k3s-image.sh,…}` (SMP
  kernel + k3s rootfs), preserved from STEP 0.

## How the integrator re-runs the gates (box-only)
On the determinism box, in `/root/ht49` (or a fresh checkout of this branch + a rebuilt guest
image and the 0005-patched KVM module — see `kvm-patches/`):
```
# guest image must already be built (guest/build/{bzImage,initramfs-k3s.cpio.gz})
/root/run-patched-ht49.sh 1200 cargo test -p vmm-core --test live_k3s_postgres -- \
    --ignored --nocapture --test-threads=1 k1_k3s_cluster_postgres_client_streams_patched
/root/run-patched-ht49.sh 2400 cargo test -p vmm-core --test live_k3s_postgres -- \
    --ignored --nocapture --test-threads=1 k2_k3s_postgres_deterministic_twice_patched
```
`run-patched-ht49.sh` loads the 0005 KVM module, pins to core 2, runs the gate, and reverts KVM to
stock `1396736`. **Always verify `lsmod | grep '^kvm '` shows `1396736` on a fresh connection
after each run** (run-patched's own revert is belt-and-braces; verify manually). k2 takes ~26 min
(two boots + two `state_hash` computations); give it the larger timeout.

## Deviations considered and rejected
- **vPIT** to give the SMP idle a tick — explicitly off the table (the project dropped vPIT once
  the LAPIC-page-hole made the LAPIC timer work; see the 2026-06-28/29 decisions). The MADT keeps
  determinism on the proven LAPIC-timer path.
- **ARAT alone** — necessary-but-insufficient; the MADT is what makes the SMP kernel set up the
  LAPIC timer at all. Both are kept (they compose).
- **Kernel rebuild for the cni0 `EINVAL`** — ruled out by a bridge probe (kernel + MTU fine); the
  fix was the veth node interface.
- **Full containerd `config-v3.toml.tmpl` replacement** for NoPivotRoot — rejected in favor of a
  drop-in (`config-v3.toml.d/`) that merges into k3s's generated config (version-robust).

## Known limitations / notes for the integrator
- **SMP slowness** (~1.7×/step) is unaddressed (correctness first, per the spec). k2 ≈ 26 min.
- **Contract bump to v4** changes `contract_hash` — a deliberate, reviewed §6 revision (ARAT only).
- **0005 (MTF) productionization** (clean patch series + canonical linux-6.18 port + full gates) is
  a separate later task; here it is only captured/preserved (`kvm-patches/patches/0005-*`).
- A benign `ACPI Warning: AcpiEnable failed` appears in dmesg: the MADT-only table set has no
  FADT/DSDT, so the AML interpreter doesn't init — but static MADT parsing (which sets `acpi_lapic`
  and arms the LAPIC timer) runs first and succeeds, as the passing gates confirm.
- The exact validated state is tagged `task56-k2-pass` (`7a5bbad`); the diagnostic-cleanup commit
  on top was re-confirmed with k1.
- Not pushed (per the task).
