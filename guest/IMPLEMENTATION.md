# Task 04 implementation notes

## What this is

`guest/` holds the two QEMU-testable deliverables: five bare-metal Multiboot
payloads with byte-exact committed goldens (`payloads/`, `golden/`), and a
pinned, reproducibly built minimal Linux guest image (`linux/`). See
`guest/README.md` for entry points, prerequisites, and the payload-authoring
guide. Nothing here depends on any other crate in the repo.

## Decisions an integrator should know about

- **Multiboot loading uses the address-override fields** (header flag bit 16,
  the "a.out kludge"), not ELF loading: QEMU's multiboot ELF path rejects
  ELF64 images outright ("Cannot load x86-64 image, give a 32bit one"), and
  producing a 32-bit ELF from a Rust x86_64-unknown-none build needs an
  objcopy conversion that llvm tooling doesn't reliably support. The override
  fields make the loader treat the file as flat; `payloads/linker.ld` forces
  a single PT_LOAD segment so file bytes really are flat. This path is
  ancient, stable QEMU behavior and works on every version tested or
  documented. The hypervisor's own loader can read the same five fields.

- **Golden comparison starts at the payload's `PAYLOAD <name> START`
  banner**, which must also be the first `PAYLOAD ` bytes of the captured
  stream (PR #8 review). SeaBIOS prints version banners and iPXE prints PMM
  *addresses* to the serial console before the payload runs; those bytes are
  environment-dependent by nature and cannot be part of a golden. Everything
  from the banner onward is compared with `cmp` (byte equality), and the
  whole suite runs twice per gate invocation. Residual limitation: stray
  non-`PAYLOAD ` bytes a buggy payload printed before START are
  indistinguishable from firmware noise without pinning firmware version
  strings.

- **RDSEED advertisement is gated on the max basic leaf** (PR #8 review,
  blocking): per the SDM, CPUID above CPUID.0:EAX returns the *highest*
  basic leaf's data, so the leaf-7 EBX[18] read only counts when
  `max_basic_leaf >= 7` — otherwise a small frozen-CPUID model could
  misreport RDSEED as advertised, fault, and FAIL. Leaf 1 (RDRAND) needs no
  guard.

- **`features` accepts any caught fault for the RDPMC probe** (it asserts
  "faulted and resumed", counting #GP and #UD together, and prints the
  spec-mandated `OK rdpmc-gp` line). Architecturally RDPMC with an invalid
  selector raises #GP, but QEMU TCG leaves RDPMC unimplemented and raises
  #UD instead. Pinning #GP would have made the golden TCG-version-dependent;
  the environment-independent fact is that it faults and never returns data.

- **`interrupts` runs at CPL0 with the PIT through the remapped PIC**; ticks
  are counted by a naked-fn stub (`extern "x86-interrupt"` is unstable, so
  stubs are hand-rolled; alignment math is commented in `common/src/idt.rs`).

- **Part B build trees live at `/tmp/hypervizor-guest-build`** (override:
  `GUEST_BUILD_ROOT`), not under the repo: the fixed absolute path is itself
  a reproducibility lever (O= must not differ between builds), and the repo
  may be bind-mounted from case-insensitive APFS, which a kernel tree cannot
  be extracted onto. Final artifacts land in `guest/build/` on the repo side.

- **`CONFIG_ACPI=y` was added beyond the spec's fragment list** so that
  `poweroff -f` actually powers off the QEMU machine — without ACPI the
  kernel parks in a halt loop and the boot gate could only time out.
  `CONFIG_X86_PM_TIMER` is explicitly off so ACPI doesn't reintroduce a
  second clocksource.

- **Two spec'd fragment symbols don't exist as written in kernel 6.18** and
  are handled as documented in `linux/config-fragment`:
  `CONFIG_RANDOM_TRUST_CPU` was removed in 6.2 (RDRAND crediting is the
  `random.trust_cpu=` boot parameter now, and the hypervisor traps RDRAND
  anyway); `CONFIG_HPET_TIMER` is `def_bool y` on x86-64 with no prompt, so
  the HPET must be excluded at runtime (`-machine hpet=off`, or simply never
  modeled by the hypervisor). `build-kernel.sh` asserts every other
  determinism-critical option survived the merge.

- **Part B gate order is repro-then-boot** (the spec lists boot first): the
  reproducibility test rebuilds from clean twice anyway, so running it first
  saves a full kernel build and means the boot test exercises exactly the
  bytes recorded in `MANIFEST.sha256`.

- **The boot gate passes `-machine hpet=off` and `random.trust_cpu=off`**
  (PR #8 review) on top of the spec's `console=ttyS0 panic=-1`: these apply
  the two runtime mitigations the config-fragment can only document
  (HPET_TIMER is unconfigurable on x86-64; RDRAND crediting became a boot
  parameter in 6.2), so the gate boots the time/entropy surface the
  fragment claims rather than asserting it aspirationally. Observed
  consequence under nested TCG: with the HPET gone and `X86_PM_TIMER` off,
  the kernel's PIT-based TSC calibration can fail (TCG timing jitter), TSC
  is marked unstable, and the ~1 s boot completes on jiffies — i.e. no
  hardware clocksource besides the TSC is even reachable, which is the
  fragment's intent. Under the deterministic hypervisor the TSC frequency
  arrives via controlled CPUID/MSR surfaces, not calibration loops.
  (`-machine hpet=off` needs QEMU >= 8.0; the documented debian:stable
  container ships 10.x.)

## Reproducibility level achieved

Same-machine, same-toolchain: two clean rebuilds inside one debian:stable
(trixie, gcc 14) linux/amd64 container produce bit-identical `bzImage` and
`initramfs.cpio.gz`; `linux/MANIFEST.sha256` records the hashes. The
committed manifest is informational — it is valid for that container
toolchain only; cross-machine reproducibility additionally needs the pinned
container image from docs/BUILDING.md (not part of this task's gate).

## Environment / tooling notes

- Installed via Homebrew on the dev Mac, per docs/BUILDING.md: `qemu`
  (11.0.1), `coreutils` (gtimeout). Additionally installed `shellcheck`
  (0.11.0) — it is required by this task's gates and listed in BUILDING.md's
  Linux package list, but was missing from the macOS section; flagging per
  the "say so instead of installing" hygiene rule (it is lint-only, not a
  build dependency).
- The Part B container needs `xz-utils` and `bzip2` on top of the
  BUILDING.md Debian list (kernel tarball is .tar.xz, busybox is .tar.bz2);
  guest/README.md carries the full list.
- Part A was verified on macOS/AArch64 under QEMU 11.0.1 TCG (both gate runs
  byte-identical); Part B inside a debian:stable linux/amd64 container under
  Docker Desktop (QEMU 10.0.8 for the boot gate).

## Known limitations

- The payload exit protocol cannot distinguish "QEMU died" from "payload
  exited 2" by exit status alone (isa-debug-exit encodes `(code<<1)|1`); the
  gate therefore also requires the golden match, which subsumes PASS-line
  presence.
- `compute` finished in about a second under Apple Silicon TCG in testing,
  so the 60 s per-payload timeout holds with a large margin; on a slower
  emulation host it is the first payload that would approach it.
- The guest kernel build is verified with the debian:stable toolchain only;
  other gcc versions will build fine but hash differently (see
  reproducibility note).
- BusyBox is built from `defconfig` + `CONFIG_STATIC=y`, so the binary
  carries many unused applets (~1 MiB more than a minimal selection). Kept:
  defconfig is the upstream-tested path; trimming applets buys size, not
  determinism.
