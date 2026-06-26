# Task 30 — boot real Linux in consonance (direct 64-bit boot protocol)

> **TOP PRIORITY · ACTIVE NOW · runs in parallel with the dissonance wave.** This is the
> milestone that *proves consonance*: until a real Linux userspace application runs inside the
> VMM, the determinism we have proven holds only over bare-metal payloads, not the thing the
> platform exists to test. Integrator directive (2026-06-25): "we absolutely need to show Linux
> can run; this is maximally important." Different code from the dissonance crates (a new loader
> in `consonance/vmm-core`), its own worker — no contention.

Read `tasks/00-CONVENTIONS.md`, `docs/BRINGUP.md`, `tasks/15-vmm-core-skeleton.md`, and
`docs/INTEGRATION.md` first. The guest artifacts and the determinism substrate already exist; this
task is the **boot path** that connects them.

## Goal

Boot the committed-by-manifest Linux guest — **Linux 6.18.35 + static busybox 1.38.0 initramfs**
(`guest/linux/`, deterministic config: no KASLR/SMP/dynticks/HW-RNG) — inside `consonance`'s VMM,
run its userspace `/init`, and observe **`GUEST_READY`** on the serial console (`guest/linux/init.sh`
prints it, then powers off). First on **stock KVM** (prove the boot), then **bit-identically twice**
on the patched backend (prove determinism).

## Approach (integrator-decided, 2026-06-25): direct 64-bit boot protocol

Hand control to the kernel's **64-bit entry** directly — the Firecracker / cloud-hypervisor model.
**No 16-bit real-mode / bzImage setup-code emulation.** The loader:

1. **Loads the kernel image** into guest RAM. Locate the 64-bit entry per the x86 boot protocol:
   parse the bzImage `setup_header` (`boot_params` offset `0x1f1`; check `boot_flag==0xAA55`,
   `header=="HdrS"`, `version>=0x020c` for the 64-bit `xloadflags` `XLF_KERNEL_64`), load the
   protected-mode kernel (the part after `(setup_sects+1)*512`) at `pref_address`/`0x100000`, and
   take the 64-bit entry at `load_addr + 0x200`. (A `vmlinux`/PVH ELF entry is an acceptable
   alternative if simpler to locate deterministically — document whichever you use.)
2. **Loads `initramfs.cpio.gz`** into guest RAM high (e.g. GPA `0x0800_0000` for 256 MiB RAM,
   below `max_initrd`), and records its GPA+len in `boot_params.hdr.ramdisk_image`/`ramdisk_size`.
3. **Builds `boot_params`** (the zero-page) at a fixed GPA: a one-entry (or minimal) **E820 map**
   over guest RAM (`e820_table`/`e820_entries`), the **command line** (`cmd_line_ptr` →
   e.g. `console=ttyS0 panic=-1 reboot=t`), the setup_header copied/filled, `type_of_loader`.
4. **Sets the long-mode entry state** (`consonance/vmm-core/src/entry.rs` is the current Multiboot
   analog — add a Linux variant): `CR0.PG|PE`, `CR4.PAE`, `EFER.LME|LMA`, an identity-mapped page
   table, a flat 64-bit GDT (`__BOOT_CS`/`__BOOT_DS` per the protocol), `RSI = boot_params GPA`,
   `RIP = 64-bit entry`, `RFLAGS.IF=0`.
5. **Dispatches by image type** in `consonance/vmm-core/src/bringup.rs` (Multiboot magic vs Linux
   `HdrS`/ELF) to the existing `multiboot::load` or the new `linux_loader::load`.

## Phasing (deliver incrementally; the milestone is Phase A)

- **Phase A — boot to `GUEST_READY` on stock KVM (THE milestone gate).** Write
  `consonance/vmm-core/src/linux_loader.rs` + the Linux entry-state path; load kernel+initramfs+
  boot_params; run on the **stock** `KvmBackend`. Get the kernel to decompress, reach userspace,
  and emit `GUEST_READY` on `0x3F8`. **This is what proves "Linux runs in consonance."**
- **Phase B — interrupts/timer if the boot needs them (expected).** The 8250 console is polled
  (already modeled), but a periodic-tick kernel will likely need a timer + interrupt delivery to
  reach userspace. The `consonance/lapic` crate (task 13) is a built userspace-xAPIC state machine
  but is **not wired into the run loop** (`vmm.rs:~618` treats MMIO as a contract violation). Wire
  xAPIC MMIO (`0xFEE0_0000`) → `lapic::Lapic`, drive the LAPIC timer off **V-time** (deterministic),
  and inject the timer vector. Scope to the minimum the kernel actually requires to reach `/init`
  — discover empirically, don't gold-plate.
- **Phase C — deterministic twice on the patched backend.** Run the same boot on
  `PatchedKvmBackend` (RDTSC/RDRAND → V-time/seeded entropy) and assert **bit-identical** serial
  capture + `state_hash` across two same-seed runs (the project's core property). Linux reads
  RDTSC/RDRAND early, so stock boot is nondeterministic by construction — determinism is *defined*
  to require the patched path. Reuse the M2/M-twice harness shape.

## Public API / shape (worker defines; sketch)

- `consonance/vmm-core/src/linux_loader.rs`: `pub fn load(image: &[u8], initramfs: &[u8], ram_bytes: u64, cmdline: &str, mem: &mut GuestMem) -> Result<LinuxImage, LinuxLoadError>` returning the
  64-bit entry, the `boot_params` GPA, and the loaded ranges; plus the `boot_params`/`setup_header`/
  `e820entry` `#[repr(C)]` structs (exact field offsets per the x86 boot protocol — pin them with a
  layout test). Total over untrusted image bytes must be **panic-free** (conventions rule 4): a
  malformed/too-short bzImage is a loud `LinuxLoadError`, never a panic or OOB read.
- Entry-state setup beside `entry.rs`'s Multiboot path; image-type dispatch in `bringup.rs`.
- Keep the `Backend`-generic seam: the loader writes guest memory through the existing region/mem
  abstraction so it is unit-testable on macOS against the mock backend; the **live boot** is box-only.

## Acceptance gates

1. **Layout pinned.** A unit test asserts the `boot_params`/`setup_header` field offsets against the
   x86 boot-protocol constants (e.g. `hdr` at `0x1f1`, `ramdisk_image` at `0x218`, `cmd_line_ptr` at
   `0x228`, `e820_entries` at `0x1e8`, `e820_table` at `0x2d0`) — drift fails here.
2. **Loader is total over untrusted bytes.** Property/fuzz the loader on arbitrary/truncated image
   bytes: never panics, never OOB; a non-`HdrS`/short image is a clean `LinuxLoadError`. (mac-testable)
3. **Phase A — live boot gate (box-only, `#[ignore]`, Linux/KVM).** Boot the real
   `guest/linux/bzImage` + `initramfs.cpio.gz` on **stock KVM**; assert the serial capture contains
   **`GUEST_READY`** and the guest powers off cleanly (no triple-fault/hang within a bounded
   wall-clock + V-time budget). This gate IS the milestone. Document the box run command (mirrors
   `box_corpus.rs`).
4. **Phase C — determinism gate (box-only, patched backend).** Two same-seed boots on
   `PatchedKvmBackend` produce **identical** serial capture and `state_hash`. (Deferred-OK if Phase A
   lands first as its own PR — but C is required to call the milestone *done*.)
5. Standard gates green (build/clippy/fmt/deny/coverage/public-api on the new module's mac-testable
   logic); no determinism leak introduced into the existing M1/M2/P6 paths (the new loader path must
   not perturb `Vmm::state_hash` for non-Linux images).

## Non-goals (defer)

virtio / a block device (initramfs-only rootfs — no disk needed); networking (R3); multi-vCPU;
ACPI beyond what `poweroff` needs; snapshot/restore of a *running Linux* (a later milestone — Phase A
proves boot, not mid-Linux snapshotting); a full device model. Do not emulate the 16-bit real-mode
bzImage setup path (integrator chose direct 64-bit). Keep the kernel artifacts as-built by
`guest/linux/` — do not re-tune the kernel config here (if a missing `CONFIG_*` blocks boot, raise it
to the integrator rather than editing the config silently).
