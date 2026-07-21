# Task 04 — `guest/`: bare-metal test payloads + reproducible minimal Linux guest

> **Historical path note:** task 43 later split this task's artifacts between
> `consonance/acceptance-suite/{payloads,golden}` and `harmony-linux/linux`.

Read `tasks/00-CONVENTIONS.md` first. Touch only `guest/`. This task is **not** part of the
cargo workspace (`guest/` is excluded); it has its own build entry points and gates.

## Environment

- Part A (payloads): macOS or Linux. Requires Rust + `x86_64-unknown-none` target +
  `qemu-system-x86_64` (Homebrew/apt; TCG is slow on Apple Silicon — that's fine).
- Part B (Linux image): Linux, amd64 — a linux/amd64 container on macOS works
  (`docs/BUILDING.md` has the exact invocation).
- Does not require: `/dev/kvm`, Intel CPU, root (container tooling aside).

All shell scripts must be macOS/Linux-portable (`timeout` vs `gtimeout` handled in one
helper; no GNU-only flags) and `shellcheck`-clean. Gates must never skip silently on an
unsupported host — fail fast with a message saying where to run them.

## Context

The deterministic hypervisor needs guests to run. Long before full Linux boots under it, the
vCPU/V-time/injection work needs tiny, fully-understood **bare-metal payloads** whose correct
output is known; later phases need a **minimal, reproducibly-built Linux image** whose kernel
config avoids nondeterministic features. Both are independent of the hypervisor itself and
testable under stock QEMU, which is exactly what this task delivers. QEMU is only a smoke
environment here — payload output must therefore be **timing-independent** (QEMU's timing
differs run to run; our hypervisor's won't, but the goldens must pass under both).

## Part A — bare-metal payloads (`consonance/acceptance-suite/payloads/`)

### Boot & I/O contract (normative)

- Payloads are **Multiboot v1** ELF executables: bootable by `qemu-system-x86_64 -kernel`,
  entered in 32-bit protected mode per the Multiboot spec. Each payload contains a common
  startup shim that builds identity-mapped page tables (1 GiB is plenty, 2 MiB pages) and
  enters 64-bit long mode before calling `payload_main()`. (See OSDev wiki: "Setting Up Long
  Mode", "Multiboot".)
- Console: 8250 UART at port `0x3F8`, polled writes only (no UART interrupts).
- Exit: write `u8` code to port `0xF4` (QEMU `isa-debug-exit`; QEMU's process exit code
  becomes `(code << 1) | 1`). Code `0` = payload PASS. A payload that can't proceed prints
  a `FAIL` line then exits with code `1`.
- Output protocol: first line `PAYLOAD <name> START`, last line `PAYLOAD <name> PASS` (or
  `FAIL <reason>`), free-form deterministic lines between. **No line may contain timing-,
  address-, or environment-dependent values** (no raw TSC values, no interrupt counts, no
  pointers).

Implementation: Rust `#![no_std]` with target `x86_64-unknown-none` plus a small assembly
shim (`global_asm!` or a `.s` file) for the Multiboot header and the 32→64-bit climb, linked
with a custom linker script at 1 MiB. Crate-per-payload with a shared `consonance/acceptance-suite/payloads/common`
library crate (UART, exit, the shim) — all inside an independent cargo workspace at
`consonance/acceptance-suite/payloads/Cargo.toml`. `unsafe` is unavoidable here and permitted throughout; keep it
in `common` where possible.

### Payloads

1. **`hello`** — prints START/PASS. Proves shim, UART, exit path.
2. **`compute`** — runs a deterministic integer workload: xorshift64\* PRNG (documented
   constants, seed `0x5EED_5EED_5EED_5EED`) driving 10 000 000 iterations of mixed
   add/xor/rotate over an 8-register state plus reads/writes over a 1 MiB scratch buffer;
   prints the final 64-bit digest as fixed-width hex. The expected digest is computed by a
   host-side Rust test (same algorithm, same constants — put the algorithm in a tiny
   `#![no_std]`-compatible shared module compiled in both worlds) and is part of the golden
   output. This payload is the workhorse for "same work ⇒ same state" checks on the real
   hypervisor later.
3. **`clocks`** — exercises time sources without printing their values: RDTSC twice ⇒ asserts
   monotonic non-decreasing; executes CPUID then RDTSC (serialization pattern); reads PIT
   port 0x40 once (must not fault). Prints only `OK <check-name>` lines and PASS/FAIL.
4. **`interrupts`** — installs a 64-bit IDT; self-tests `int 0x40` (software interrupt
   increments a counter); unmasks the legacy PIT (mode 2, divisor for ~100 Hz), enables
   interrupts, halts in a `hlt` loop until ≥ 5 timer ticks observed, masks PIT. Prints
   `OK swint`, `OK timer` — never the tick count or any timing detail. This is the payload
   the precise-injection work will replay millions of times.
5. **`features`** — exercises the nondeterministic-instruction surface the hypervisor will
   trap, with **environment-independent output** (the same goldens must pass under QEMU TCG
   and, later, under our hypervisor):
   - executes CPUID (a fixed set of leaves) twice ⇒ asserts byte-identical results, prints
     `OK cpuid-stable` (never the values);
   - if CPUID advertises RDRAND: executes it, retrying per spec until CF=1, asserting it
     doesn't fault; prints `OK rdrand` either way (advertised-and-works, or not advertised
     — only advertised-but-broken is a FAIL). Same pattern for RDSEED;
   - with CR4.PCE left clear, executes RDPMC under a #GP-safe handler (reuse the IDT code
     from `interrupts`) and asserts it faults ⇒ `OK rdpmc-gp` (architecturally guaranteed,
     so environment-independent).
   MSR read/write probes are deferred until the VMM exists (they'd be QEMU-model-dependent).

### Part A gates

`consonance/acceptance-suite/payloads/run-tests.sh` (also wired into
`consonance/acceptance-suite/Makefile` as `make test-payloads`):
builds all payloads, then for each: runs
`qemu-system-x86_64 -m 256 -nographic -no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04 -serial mon:stdio -kernel <payload>`
with a 60 s timeout, captures serial output, asserts (a) QEMU exit code corresponds to
payload code 0, (b) output matches `consonance/acceptance-suite/golden/<name>.txt` **exactly** (byte equality).
Goldens are committed. Run the whole suite twice in the gate script — under TCG the two runs
must already produce identical output (that's what timing-independence means).

## Part B — minimal Linux guest (`harmony-linux/linux/`)

- `make kernel`: download a pinned kernel (pick the latest LTS **at task start and commit
  the pin immediately** — exact version + sha256 in `harmony-linux/linux/versions.lock`; verify the
  hash before building), build `bzImage` with `harmony-linux/linux/config-fragment` applied on top
  of `make tinyconfig`.
- Config fragment (starting point — comment each line with rationale): 64BIT=y, !SMP,
  PRINTK=y + 8250 serial console, !RANDOM_TRUST_CPU (no RDRAND seeding — entropy must come
  from our channel), !HW_RANDOM, NO_HZ_IDLE off → periodic ticks HZ=100 (predictable timer
  behavior), !TRANSPARENT_HUGEPAGE, !KSM, !NUMA, !CPU_FREQ, !HIBERNATION/!SUSPEND, !MODULES
  (everything built in), DEVTMPFS, BLK_DEV_INITRD, EXT4=n (initramfs only), TSC as the only
  clocksource where possible. It will be iterated later against the determinism harness;
  correctness bar for this task is that it **boots**.
- `make initramfs`: BusyBox (pinned + hash-verified) static build, `/init` script that
  mounts proc/sys, prints `GUEST_READY`, then `poweroff -f`.
- `make image` builds both; **reproducibility is required**, and it's harder than it looks —
  expect iteration. Known levers, all mandatory: `KBUILD_BUILD_TIMESTAMP`,
  `KBUILD_BUILD_USER`/`KBUILD_BUILD_HOST`, `SOURCE_DATE_EPOCH`, `LOCALVERSION=` (empty, and
  `CONFIG_LOCALVERSION_AUTO=n` so git state can't leak in), build out-of-tree with a fixed
  `O=` path so absolute paths don't differ between builds, cpio with sorted entries, owner
  0:0, fixed mtimes, `gzip -n`. The reproducibility gate only compares two builds on the
  same machine/toolchain; cross-machine reproducibility additionally needs the pinned
  container from `docs/BUILDING.md` — note whichever level you achieved in
  `IMPLEMENTATION.md`.

### Part B gates

`make test-linux`:
1. Boot test: `qemu-system-x86_64 -m 512 -nographic -no-reboot -kernel bzImage -initrd initramfs.cpio.gz -append "console=ttyS0 panic=-1"`,
   assert `GUEST_READY` appears within 120 s and QEMU exits.
2. Reproducibility test: `make clean-artifacts && make image` twice (without re-downloading);
   `sha256sum` of `bzImage` and `initramfs.cpio.gz` identical across the two builds; emit
   `harmony-linux/linux/MANIFEST.sha256`.

## Acceptance gates (whole task)

`make -C harmony-linux test` runs Part A everywhere, and Part B when the host is Linux; on a
non-Linux host it must fail fast for Part B with a clear "run this in a linux/amd64
container — see docs/BUILDING.md" message, never skip silently. CI-friendly: no network
access needed after `make -C harmony-linux fetch` (downloads to `harmony-linux/dl/`, hash-checked). Document
host-package prerequisites (qemu, flex, bison, libelf-dev, bc, cpio…) in `harmony-linux/README.md`,
plus exact instructions to add a new payload. Shell scripts pass `shellcheck`.

## Non-goals

Running under the real hypervisor or KVM at all; hypercall integration in payloads (Task 01's
guest client gets wired into a payload in a later integration task); containers/rootfs beyond
BusyBox; guest kernel *patches* (config only, for now); ARM or 32-bit anything.
