# Task 18 — instruction-sweep payloads (C1): implementation notes

Ten C1 micro-payloads (one per trapped instruction / MSR class), the
contract-derived conformance tables that back them, and their corpus-manifest
registration. Coverage of the trap surface and per-payload behaviour are in
[`README.md`](README.md); this file records design decisions, limitations, and
what the integrator must know.

## What landed

- **10 payloads**: `insn-{rdtsc,rng,cpuid,rdpmc,hlt,mwait}`,
  `msr-{allowed,denied}`, `irq-landing`, `pit-pic-stub` — each via the documented
  "add a payload" flow, registered in `payloads/Cargo.toml`,
  `payloads/run-tests.sh` and `guest/golden/`.
- **`common` additions** (reusable, per the task's "put reusable bits in
  common"): `report` (report-channel emitter, `OUT 0x0CA2`; task 28 — QEMU no-op),
  `probe` (fault-catch
  helpers over the existing IDT stubs), `apic` (xAPIC MMIO + an LAPIC-timer IDT
  stub), `io::outl`. The boot shim now also identity-maps the **4th GiB** so
  `irq-landing` can reach the xAPIC MMIO page at `0xFEE00000`.
- **`contract-data`**: a shared host+guest crate (like `compute-core`) whose
  `build.rs` parses `docs/cpu-msr-contract.toml` and generates the frozen CPUID
  model, the allow-fixed / allow-stateful MSR sets, and a merged index map; host
  tests assert the tables trace to the contract.
- **`docs/corpus-manifest.toml`**: the 10 items as `Micro` entries with their
  oracles + goldens.

## Key design decisions (and rejected alternatives)

### The QEMU-safe / box-only split
The stock-QEMU Part-A gate runs every payload with **no hypercall handler and
the host's real CPUID/MSR/TSC/entropy** — none of which match the deterministic
contract. So each payload asserts in its serial banner **only what is true in
every environment** (monotonic TSC, CPUID stability, advertised-RNG-works,
fault-and-resume, allow-stateful round-trip, an IRQ lands) and emits the
trap-dependent values (exact frozen CPUID, RDTSC = f(V-time), MSR default-deny,
seeded RNG stream, retired-count sweep) through `common::report` for the box
oracle. This is the spec's load-bearing "QEMU validates the banner only"
requirement.

### `report()` is a documented no-op (box transport deferred)
> **Resolved by task 28 (corpus box-integration).** `report()` now emits the two
> `OUT 0x0CA2` writes over the dedicated **report channel** (`REPORT_PORT`,
> distinct from the `0x0CA1` doorbell — `docs/INTEGRATION.md` §1.1); the box
> captures the stream into the O2 `observable_digest` (goldens in
> `guest/golden/<name>.digest`, gate run by `consonance/vmm-core/tests/box_corpus.rs`).
> Under stock QEMU the writes are still discarded, so the Part-A gate stays
> byte-identical. The original task-18 rationale is kept below for history.

Payloads call `common::report::report(v)` at every point where the box oracle
will check a value, documenting *which* values and *in what order* — but the
function does nothing today. The box report channel is a corpus
**box-integration** concern (gate 4), not this PR. In particular it cannot ride
the stock-KVM doorbell port `0x0CA1` (#44): a write there is `OUT EAX = request
frame length` and the host then reads the request page at `REQ_GPA`, so raw value
writes would look like malformed/oversized hypercall requests on the KVM lane.
Choosing the real transport (a distinct port or a proper hypercall frame) and
capturing the O2 digest goldens belongs to the integration task. The no-op keeps
the Part-A gate trivially green and the two-run output byte-identical.
*(Earlier this raw-wrote `0x0CA1`; removed in the #48 integration pass after the
doorbell ABI collision was flagged.)*

### Conformance values are generated from the contract (gate 3)
`contract-data/build.rs` parses the contract's strict grammar with **std only**
(no `toml` crate — staying within the conventions whitelist) and emits the
expected tables, so a contract bump (v4) regenerates them and surfaces as a
payload/golden diff. Host tests (`cargo test -p contract-data`, wired into
`run-tests.sh`) spot-check the generated values against known frozen constants —
"generated **and** test-asserted-equal", both arms of the gate.

**`cpuid-model.md` is non-normative and stale.** Its header says the TOML wins,
and post `det-skx-v1` → `det-cfl-v1` re-baseline it still describes the old SKX
model (e.g. leaf-1 EAX `0x00050654` vs the contract's `0x000906ec`). Expected
values are therefore derived from `docs/cpu-msr-contract.toml` exclusively.

### `irq-landing` uses a bounded poll, not HLT
Retired-count-before-IRQ is well-defined for a deterministic spin; a bounded
`pause` poll lets a never-firing timer fail cleanly instead of hanging, and is
the primitive the box sweep measures. HLT-based idle-skip is exercised separately
by `insn-hlt`. The deadline sweep brackets `skid_margin=128` (64/127/128/129/…).

### `insn-mwait` can never hang
MWAIT can block on real silicon. The frozen model hides MONITOR (so on the box
and default QEMU both #UD and are skipped); if a permissive QEMU *does* advertise
MONITOR, the payload arms a monitor on a scratch line and stores to it before
MWAIT, so a supported MWAIT sees a triggered monitor and returns immediately.

### Boot-shim change (shared `common`)
Mapping the 4th GiB is additive — existing payloads never touch `0xC0000000+`,
and the gate confirms all five pre-existing payloads still pass twice
byte-identically. 2 MiB pages (not 1 GiB) avoid depending on PDPE1GB under the
default QEMU TCG CPU.

## Gate status

| Gate | Status |
|------|--------|
| Part-A: build, run **twice** byte-identical under QEMU, goldens committed, `make -C guest test-payloads` green | ✅ (15 payloads, macOS QEMU 11.0.1) |
| 1 — trap-surface coverage checklist | ✅ `README.md` |
| 2 — manifest registration (`micro` + oracles) | ✅ `det-corpus validate --manifest docs/corpus-manifest.toml` passes (10 items, round-trips) |
| 3 — conformance values derive from the contract | ✅ `contract-data` generated + host-tested |
| 5 — trap-dependent payloads build + protocol-valid under QEMU/macOS | ✅ |
| 4 — **box** determinism (O1) + O2 conformance goldens + `irq-landing` sweep | 🔧 **built by task 28** (report channel `0x0CA2` + VMM-backed `det-corpus` Machine + O2 goldens + `consonance/vmm-core/tests/box_corpus.rs`); the live box run/goldens are blessed on the box at review |

## What the integrator must know

1. **Manifest loads under the merged harness.** After #48 merged, the manifest was
   rewritten to det-corpus's string-token form (`kind = "micro"`,
   `oracles = ["determinism", "seed_sensitivity:pure"|":rng"]`);
   `cargo run -p det-corpus -- validate --manifest docs/corpus-manifest.toml`
   passes (10 items, round-trips). Each item declares only the two oracles the
   shape lane supports — **O1 determinism** and **O3 seed_sensitivity** (rng for
   `insn-rng`, pure otherwise).

2. **O2/Conformance — built by task 28 (corpus box-integration).** What task 18
   left for the follow-up is now done: the report transport is the dedicated
   `0x0CA2` channel (not `0x0CA1`, owned by #44); a VMM-backed `det-corpus`
   `Machine` (`vmm_core::corpus`) loads each payload through the
   `PatchedKvmBackend`, runs `check_determinism` (O1) + captures the
   `observable_digest` (O2). `"conformance"` + a `golden = guest/golden/<name>.digest`
   are added to the **six** payloads that run to a clean PASS on vmm-core's current
   event loop (insn-rdtsc, insn-rng, insn-cpuid, insn-rdpmc, msr-allowed,
   msr-denied). Four payloads are **O2-deferred** — they can't reach a clean PASS
   today: insn-hlt, irq-landing, pit-pic-stub need PIT/LAPIC-timer interrupt
   injection + LAPIC MMIO + idle-skip (a bare `HLT` is terminal there, LAPIC MMIO /
   port `0x61` are unmodeled), and **insn-mwait** exits `DebugExit 1` (MONITOR/MWAIT
   are unmodeled on the event loop — see `### insn-mwait can never hang`, which is
   the QEMU shape, not the patched event loop). All four keep O1/O3 only and gain
   conformance once vmm-core lands V-time timers + IRQ injection (+ MONITOR/MWAIT).
   The O2 goldens are V-time/seeded-PRNG-derived, so they are blessed on the box
   (`DETCORPUS_BLESS=1`, see `consonance/vmm-core/tests/box_corpus.rs`); they are
   **distinct** from the run-tests.sh **serial-shape** goldens
   (`guest/golden/<name>.txt`).

3. **No new crate dependencies.** `contract-data` build-time parsing is std-only;
   the payloads depend only on `common` (+ `contract-data` for the three
   contract-backed ones).

## Known limitations

- Coverage is the 10 task-specified classes; uniform-#UD instructions, XSETBV,
  emulate-vtime MSR reads, named-deny-gp MSRs and the wider device-verb surface
  are named omissions in `README.md` (the last is task 21).
- `insn-cpuid` probes one concrete (leaf, subleaf) per contract row, not every
  subleaf of the wildcard/range zero-fill regions (those are uniform zero by the
  default rule; exhaustive box coverage is the VMM's `KVM_SET_CPUID2` table).
- These are bare-metal payloads: not built/tested under Miri (the target is
  `x86_64-unknown-none`, full of `asm!`), consistent with the rest of
  `guest/payloads/` and the Part-A gate.
