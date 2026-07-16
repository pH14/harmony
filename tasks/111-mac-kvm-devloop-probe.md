# tasks/111 — Mac nested-KVM dev-loop probe (hm-8l3)

**Bead:** `hm-8l3` (P3) · **Budget:** ~1–2 hours · **Repo changes:** NONE (evidence-only task)

## Question

Can an aarch64 Linux VM on this Mac (Apple Silicon) expose a working `/dev/kvm`, so the
ARM backend skeleton (`hm-cbt`, the D-list lane) gets a local ioctl dev loop — or is
QEMU TCG the only local oracle until the Ampere Altra box arrives?

Context: the ARM backend will target KVM/arm64. The M-series Mac is itself aarch64, so
*pure logic* runs natively; the open question is specifically the **KVM ioctl loop**
(device open, `KVM_CREATE_VM`, `KVM_CREATE_VCPU`, a trivial `KVM_RUN` that faults/exits
cleanly). Apple's nested-virtualization support gates this: it requires an M3-or-later
chip AND macOS 15+ AND a VMM that opts in (`VZGenericPlatformConfiguration.isNestedVirtualizationEnabled`
or equivalent). On older chips/macOS the answer is a clean REFUSE and TCG stays the oracle.

## Method (suggested, not binding)

1. Record the host facts first: chip generation (`sysctl machdep.cpu.brand_string`),
   macOS version, and whether `VZGenericPlatformConfiguration` reports nested support
   (a ~10-line Swift or `vz`-CLI check, or an existing tool — lima ≥1.0 exposes
   `vmType: vz` with `nestedVirtualization: true`).
2. If the host supports it: bring up an aarch64 Linux guest with nested virt enabled
   (lima or UTM/vz — operator's choice; pin the image/version in the note), then inside
   the guest check `/dev/kvm` exists and run the minimal ioctl sequence (a ~40-line C
   file or `kvm-ok`). A trivial `KVM_RUN` reaching a predictable exit = GO.
3. If the host refuses (pre-M3 chip or macOS < 15): STOP there — record REFUSE with the
   host facts. Do not chase workarounds; TCG is the sanctioned fallback either way.

## Deliverables (bead-only — no PR, no repo files)

1. `bd close hm-8l3` with the GO/refuse verdict + the evidence inline: host facts, the
   exact stack used (tool + versions + image), and the ioctl transcript or the refusal
   point. An honest REFUSE is a full success for this task.
2. `bd comment` (or note) on `hm-cbt` with the one-paragraph consequence: "local ioctl
   dev loop available via <stack>" or "TCG-only until Altra (`hm-7pb`)".

## Ground rules

- No changes to the harmony repo working tree; nothing gets committed or pushed.
- Do not touch the determinism box; this is entirely Mac-local.
- Keep any VM/tooling installs user-local (brew/lima are fine); note what was installed
  so it can be reverted.
- If a VM bring-up wedges past ~30 min of fiddling, record what wedged and REFUSE —
  the probe's value is a fast honest answer, not a heroic bring-up.
