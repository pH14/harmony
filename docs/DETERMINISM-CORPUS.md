# Determinism & Conformance Corpus — verifying the engine is itself correct

The project's one invariant is *same seed ⇒ bit-identical execution*. Every feature is
gated on it, but nothing yet **systematically attacks** it. This document is the plan for the
component that does: a growing corpus of workloads, run through a small set of oracles, that
together answer "is the deterministic engine itself correct?"

It is a design doc in the mold of `R-BACKEND.md` / `R1-DEVICE-MODEL.md`: it fixes the frame
and the dependency gates, and sequences a backlog. Tasks 17–18 are specced now
(`tasks/17-det-corpus-harness.md`, `tasks/18-instruction-sweep.md`); 19–20 and the
Linux-guest workloads are outlined here with their gates and will be specced when unblocked.

## The frame: two orthogonal axes

The mistake to avoid is bundling "what we run" with "what we check." Keep them separate:

- **Oracle axis** — the *property* under test. There are only four (O1–O4 below). They are the
  whole point; the corpus exists to feed them.
- **Corpus axis** — the *workloads* that exercise the engine (C1–C3 below). A corpus item is
  inert; its value is which oracles it can drive and how much engine surface it touches.

The product is a matrix: every corpus item declares which oracles apply, the harness runs
them, and a single JSON report says what passed. Adding an instruction payload, a fuzz seed,
or SQLite is the *same operation* — register a corpus item, point it at oracles.

Crucially, **the hardest oracle already exists.** `unison` is a generic divergence
bisector over a `Machine` trait (`compare_runs` / `bisect_divergence`, work-count localized).
This component is the *domain layer* on top of it: it knows about CPU instructions, the frozen
contract, goldens, and the real VMM. `unison` stays domain-free (its non-goals forbid VMM
integration); `det-corpus` is where that knowledge lives.

## Oracles — the four properties

| ID | Property | Mechanism | Catches |
|----|----------|-----------|---------|
| **O1** | **Determinism** (replay-equivalence) | `compare_runs(F, F, seed, …)` must be `Identical`; on `Diverged`, `bisect_divergence` localizes to the exact work count | the core bug: any nondeterminism that leaks into observable state |
| **O2** | **Conformance** (matches spec, not just itself) | observed serial/state digest == committed golden; trapped-instruction results == frozen `docs/cpu-msr-contract.toml` (CPUID = frozen model, MSR default-deny + allowed set, RDTSC = f(V-time), RNG = contract PRNG stream) | "deterministic but **wrong**" — a constant is perfectly deterministic and perfectly useless |
| **O3** | **Seed-sensitivity** (non-triviality / anti-cheat) | compares a guest-**observable output** digest (`out_*`), **not** `state_hash` (which includes the seed-derived latent entropy state — see task 17 note): RNG-consuming + control-flow-stable payload under two *different* seeds → assert `work_a == work_b` **and** `out_a != out_b`; pure payload → assert `out_a == out_b` | the two failure modes O1 alone can't see: **faked** determinism (RNG wired to a constant → passes O1 trivially) and **seed-leaked** nondeterminism (seed reaching state it shouldn't). Needs a `unison::Machine` observable-output accessor (additive; [question]) |
| **O4** | **Backend-equivalence** (later) | on a TSC/RNG-free payload, `compare_runs(F_kvm, F_patched, …)` must be `Identical` — different backends, same architectural result | the patched-KVM trap apparatus silently changing baseline semantics. `unison` already takes two distinct factories for exactly this |

O1 is necessary but not sufficient — O2 and O3 are what stop "made it deterministic by making
it constant" from passing. O3 is the subtle one and the highest-value cheap check: it exploits
that *TSC = f(work)* (seed-independent) while *RNG = f(seed)* (seed-dependent), so a workload
that consumes RNG without branching on it must keep an identical work count across seeds while
its hash diverges. Both directions are bugs if violated.

## Corpus — the three families

### C1 — Instruction sweep (the deterministic, exhaustive one)
One tiny bare-metal payload per **trapped instruction / MSR class** we've identified, each
exercising it many times and *at boundaries*. The trap surface (RESEARCH.md §3.5, the
contract, R1):

- `RDTSC` / `RDTSCP` — monotonic, == f(V-time); never a raw host TSC
- `RDRAND` / `RDSEED` — contract PRNG stream, CF semantics
- `CPUID` — every frozen leaf/subleaf in `docs/fragments/cpuid-model.md`, exact regs
- `RDPMC` — trapped/denied per contract
- `HLT` — idle-skip: work count freezes, V-time warps to next deadline (`VClock::advance_idle`)
- `MONITOR`/`MWAIT`, `PAUSE` — exit behavior per contract
- MSR read/write — the allowed set returns contract values; **everything else #GP** (default-deny)
- **LAPIC timer interrupt landing** — the hard core (RESEARCH.md §3.4): a timer armed in
  V-time must be injected at the **exact same instruction** across runs. Payload reports
  "instructions retired before first IRQ" via hypercall; O1 compares it across runs, O2 vs
  golden. Sweep deadlines on/around `skid_margin=128` (task 07). This is where determinism is
  *most* likely to break; it gets the most corpus attention.
- PIT / PIC deterministic boot stubs (R1)

These are pure Part-A payloads (no OS) — cheapest to build, highest signal per byte.

### C2 — Generated / fuzzed (the fuzzing one)
Two tiers, because a KVM run is microseconds-of-ioctls, not the nanoseconds libfuzzer wants:

- **Fast, in-process, Mac** — `cargo-fuzz` over (a) `hypercall-proto::decode` (no-panic on
  arbitrary input — Tier-1; round-trip asserted only on canonical frames) and `snapshot-store`
  driven through its **public builder operations** (`begin_base`/`derive`/`write_page`/`seal`/
  `read_page` sequences — no-panic + invariants; it has **no** public delta-byte parser, so do not
  assert a delta `encode(decode(x))`), and (b) the `ToyMachine` interpreter + a generated-program
  model, feeding O1/O3. Runs anywhere, millions of cases.
- **Slower, real-KVM, box-gated** — `Arbitrary` input = (seed, instruction-mix program,
  hypercall-response script, interrupt schedule) → `compare_runs` on the real `Vmm`. Lower
  iteration rate; made viable by fast snapshot-reset (the *one* Nyx mechanic worth lifting —
  dirty-page reset at high rates, `RESEARCH.md:81`; **not** Nyx itself — hosting our patched
  KVM inside Nyx means nested virt, which degrades the very PMU/TSC the engine rests on).

The C1 payloads are the seed corpus for C2.

### C3 — Real workloads (the "known useful workload" one)
The unblock without a Linux guest: **SQLite needs no OS** — it talks to storage only through a
pluggable VFS (`SQLITE_OS_OTHER` + a registered custom VFS), so a freestanding payload can map
its file ops straight onto our `Block` hypercall service. The point of spending SQLite on the
corpus is to **drive a real application across the device boundary**, so it must use a *disk*
DB, not `:memory:` — `:memory:` is malloc'd B-tree pages that never cross the hypercall seam
(already covered by `compute` + the instruction sweep). "In memory" means the **host-side
block backing is RAM** (deterministic, no real disk), not that SQLite bypasses the interface.

- **C3a — SQLite-with-disk over the `Block` service (freestanding payload).** Build the SQLite
  amalgamation + `speedtest1.c` (SQLite's own canonical, self-checking benchmark) as a
  Part-A-style payload with a **custom VFS that maps `xRead`/`xWrite`/`xFileSize`/`xTruncate`/
  `xSync` onto the `Block` hypercall service** (single-vCPU ⇒ locking and `xSync` durability are
  no-ops; rollback-journal mode, no WAL/shm). The host serves it from an **in-RAM block buffer
  that is part of `vm_state` and the `state_hash`**. Runs under O1 (determinism), O2 (golden
  digest of speedtest1's self-check). The near-term payoff is **O1 over a real workload that
  crosses the writable device boundary**: run the same client workload twice at one seed, pause
  at checkpoints (`compare_runs`' cadence), and assert identical VM state at every point — "same
  workload ⇒ identical VM behavior," now through real block reads *and writes*. It also proves
  the abstraction end-to-end: SQLite's 4 KB page (8 sectors) over the 7-sector
  `BLOCK_READ_MAX_SECTORS` cap forces the VFS to chunk I/O — an impedance mismatch `:memory:`
  would never reveal. Crash-consistency/durability (the fault model, task 23) is a *later*
  extension on top, not part of this. **Hard dependency: the writable `Block` device (task 22).**
- **C3b — Linux-guest workloads (gated).** Memcached + `memtier_benchmark`, etc. These need a
  guest OS. Their **I/O is already mostly determinized by construction** (see "Device
  determinism" below): block-read/write is the `Block` service, host-RAM-backed and snapshotted;
  the network is R3's in-guest fault-injecting bridge. So C3b waits on **a guest OS + R3
  (net/fault model)**, not a new device-model ruling.

## Staging & dependency gates

The discipline (from ROADMAP): don't spec a task whose interface depends on an unmade
decision. C1 and the fast C2 tier depend only on merged crates + the existing Part-A pipeline;
C3a additionally needs the writable `Block` device (task 22, frontier); the gated tail (C3b and
the storage fault model, 23) waits on a guest OS + R3 — **not** a new device-model ruling.

### Device determinism (why disk/external I/O is mostly already solved)

The plan for disk and external devices is *there are no devices* — everything is the one
**synchronous** VMCALL channel, which removes both halves of the ReVirt reduction up front:

- **No async timing.** `INTEGRATION.md §1`: single in-flight, vCPU blocked for the exchange —
  no DMA, no completion IRQs, no ordering. R1 omits the IOAPIC for exactly this reason ("no
  real devices ⇒ no IRQ lines"). The *only* async event in the machine is the V-time LAPIC
  timer; devices generate none.
- **Deterministic content.** Each device is a `hypercall-proto::ServiceId` whose host answer is
  a pure function of (image, seed, state), and that state is in the snapshot checklist
  (`INTEGRATION.md §4`), so it branches/restores correctly.

**Writes are not a determinism problem — and read-only is MVP scope, not a principle.** A
write to a virtual disk whose backing is part of the COW-snapshotted VM state is a deterministic
state transition, exactly like the EPT-COW RAM writes Antithesis already does (`RESEARCH.md:49`).
Determinism comes from controlling *external* inputs + async timing, not from forbidding writes;
"side-effect-free channel" means no effect that **escapes the deterministic boundary** (real
packet, real entropy, wall-clock), not "no writes." Antithesis lists **disk I/O** as a
*controlled* nondeterminism source (`RESEARCH.md:60`) — its read-only device is only the **boot
medium** (an AHCI CD-ROM, `RESEARCH.md:42`), not all storage. Our `Block` service is read-only
today purely because *booting only needs reads* (task 01 scoped writes out); it is not a design
stance, and writable storage is **core to the mission** (crash-consistency/durability is the
canonical bug class Antithesis sells to database companies), not a later escalation.

Concretely:
- **Disk read — done.** `ServiceId::Block = 3` is a *read-only*, sector-based, synchronous
  service. Content-addressed image ⇒ identical bytes; sync copy ⇒ no timing leak. Only a
  conformance payload is owed.
- **Disk write — task 22 (near-term): a block device that works.** A `BLOCK_WRITE` opcode on
  `ServiceId::Block`, host-backed by a buffer in `vm_state` + the `state_hash` (deterministic;
  branches/restores correctly). That is the *whole* near-term scope: enough to run a real
  workload across the writable device boundary and let **O1 catch any write-divergence** (disk
  contents are in the hash). **No fault model yet.**
- **Storage fault model — task 23 (deferred, R3-adjacent).** Deterministically lose / reorder /
  tear un-`fsync`'d writes at a crash point → the durability / crash-consistency capability the
  mission ultimately exists for. Real, but **explicitly not now** — sequence it after the block
  device and the SQLite determinism test land, alongside R3's fault scheduler. (When it lands it
  cannot sit on tmpfs: `fsync` is a no-op there, so durability would be untestable.)
- **Network / external — R3.** No external IRQ lines; distribution is simulated *inside* the
  guest as containers on a deterministic fault-injecting bridge, driven by the seed-derived
  fault schedule (task 11). A true `external-net` hypercall is the escape, determinized like
  the rest (prefer a *simulated* peer — a real external service can't be branched). This is
  the existing **R3** ruling; nothing new.

### Current device surface — what exists vs. what a standard system needs

Full findings and the writable-storage plan: **`docs/BLOCK-DEVICE.md`** (grounds tasks 22/23).

The hypercall surface is deliberately minimal (Antithesis "real device models removed") and is
genuinely thin today. The **complete** set of services is `Console, Entropy, Block, Event`; the
guest's whole callable API is `console_write` / `entropy_fill` / `block_capacity` / `block_read`
/ `event_emit`.

| Primitive | Status | For running a standard system |
|-----------|--------|-------------------------------|
| Console (out) | ✅ output-only (`console_write`) | logs / serial |
| Entropy | ✅ deterministic (`entropy_fill`) | `/dev/random` source |
| Block **read** | ✅ read-only | boot a read-only rootfs (live-CD model) |
| Block **write** | ❌ **task 22** | persistent storage (DBs, `/var`); ephemeral writable bits can be a tmpfs overlay |
| Clock / TSC | ✅ V-time | all guest clocks |
| Interrupts | ✅ LAPIC timer (V-time) | only async event; push input arrives this way |
| Data **input** (host→guest) | ❌ not in the ABI (`INTEGRATION.md:30`) | interactive stdin; most servers don't need it |
| **Network** | ❌ **no service at all** | see below |

**Network is not a host device in this model — by design, not omission.** A distributed system
runs *inside one guest* as containers on a virtual bridge (`RESEARCH.md:37`); that intra-guest
network is the **guest kernel's own stack** (loopback/bridge/veth) on deterministic CPU+RAM, so
it comes **free with a Linux guest** — there is no emulated NIC to build. What is genuinely
absent is (a) the *external-net* escape (guest ↔ real world) and (b) the *fault-injecting* bridge
(delay/drop/partition) — both are the **R3** ruling, deliberately deferred.

**So the near-term blockers to running a standard system are vmm-core bring-up (frontier — the
thing that actually boots Linux) and writable block (task 22), not network.** A read-only rootfs
+ tmpfs-for-writable-bits boots a real system on today's design; persistent storage needs 22;
intra-guest networking needs only the Linux guest; external/fault-injected networking is R3.

### Gating

- C1/C3a goldens and the real-KVM C2 tier are **box-gated** (need `/dev/kvm` + patched KVM);
  the harness logic, manifest, conformance differ, and the fast C2 tier are **Mac-testable**
  against `ToyMachine` / `MockBackend` — same split as every other crate (`CODE-QUALITY.md`).

## Borrow inventory (the "steal from existing projects" answer)

| Source | Borrow | For | Caveat |
|--------|--------|-----|--------|
| **kvm-unit-tests** | freestanding bare-metal CPU/KVM test kernels (tsc, apic, msr, vmx) | C1 instruction sweep — ready-made cases | needs an entry/console shim to our Part-A protocol; their harness assumes QEMU testdev ports |
| **SQLite** amalgamation + `speedtest1.c` + `sqllogictest` | a real, self-checking workload that runs freestanding via a **custom VFS over the `Block` service** (`SQLITE_OS_OTHER`) | C3a | C toolchain + libc shim; needs a writable `Block` path; pin the amalgamation version + sha256 like `versions.lock` |
| **Memcached** + `memtier_benchmark` / `mc-crusher` | canonical KV/net workload + load generator | C3b (gated) | needs net + Linux guest |
| **rr** test suite; **Antithesis prior art** (`preestablished/determinism-hypervisor`, `oss-garage/bedrock`, see [[prior-art-det-hypervisors]]) | determinism test ideas & corpora to mine | C1/C2 | adapt to our hypercall protocol |
| **Nyx / kAFL** | *snapshot-reset loop mechanics only* (`RESEARCH.md:81`) | C2 real-KVM tier speed | do **not** host the engine inside Nyx (nested virt breaks PMU/TSC determinism) |

## Deliverable structure

- **`consonance/det-corpus/`** *(task 17)* — host-side oracle runner. Generic over `unison::Machine`/
  `MachineFactory`; defines the corpus manifest, the O1–O3 oracle runners, the conformance
  differ, and the JSON report. Pure-logic, Mac-testable with `ToyMachine`; pointed at
  `vmm-core::Vmm<B>` at integration. Composes `unison` (this is integration-class, so the
  "no sibling deps" rule of wave-1 parallel crates doesn't apply — it's the layer that *binds*).
- **`guest/payloads/`** *(task 18)* — the C1 micro-payloads, via the documented "add a payload"
  flow; goldens in `guest/golden/`.
- **`guest/workloads/sqlite/`** *(task 20)* — the C3a freestanding SQLite payload + libc shim +
  the custom VFS over the `Block` service.
- **writable `Block` device** *(task 22)* — a `BLOCK_WRITE` opcode on `ServiceId::Block` (a
  hypercall-proto contract addition → integrator/foreman, like the contract bump) + a host-side
  block backing in `vm_state` + the `state_hash`. A block device that *works*; dependency of
  task 20. **No fault model** — that is task 23.
- **storage fault model** *(task 23, deferred / R3-adjacent)* — deterministic lose/reorder/tear
  of un-`fsync`'d writes at a crash point → durability/crash-consistency, the mission's eventual
  marquee capability. Sequenced after 22 + the SQLite determinism test, alongside R3.
- **`fuzz/`** *(task 19)* — `cargo-fuzz` targets for the C2-fast tier.
- **`docs/corpus-manifest.toml`** — the registry (one entry per corpus item): name, kind,
  source path, applicable oracles, golden ref, RNG-tag. A golden-style artifact like
  `cpu-msr-contract.toml`; reviewing a diff to it is how "we added/changed a test" is audited.

## Sequenced backlog

| # | Task | Class | Depends on | Output |
|---|------|-------|-----------|--------|
| 17 | `det-corpus` harness (oracle runner + manifest) | **delegable-now** (generic over `Machine`, ToyMachine-tested) | unison (merged) | runner crate + manifest schema + JSON report |
| 18 | Instruction-sweep payloads (C1) | **delegable-now** (Part A) + box for goldens | task 04 pipeline, contract 06, lapic 13, R1 | one payload per trapped insn/MSR + goldens + conformance table |
| 19 | Determinism fuzzer (C2) | partly delegable (fast tier, Mac); box for real-KVM tier | 17, 18 (seed corpus), `arbitrary` | `cargo-fuzz` targets + corpus + CI wiring |
| 22 | **Writable `Block` device** (`BLOCK_WRITE` + snapshotted backing) | contract addition (integrator) + host-side (frontier) | hypercall-proto contract, vmm-core `Block` service | `BLOCK_WRITE` opcode + backing in `vm_state`/`state_hash` (no fault model) |
| 21 | Device-verb conformance sweep (C1-analog for `Block`) | **delegable-now**ish (shape) + box for goldens | 17, 22 (for write round-trip) | block-read/write payloads: read==image, write→read round-trips & survives snapshot |
| 20 | **SQLite-with-disk** workload (C3a) — VFS over `Block` | delegable-ish (freestanding C) + box for det gate | task 04 pipeline, 18 (libc/console shim), 17, **22** | sqlite+speedtest1 payload + custom VFS + golden + **O1 (pause-and-compare)** + O2 gates |
| 23 | Storage fault model (crash-consistency) | **deferred** / gated | 22, R3 fault sched | seed-scheduled lose/reorder/tear-on-crash; durability assertions |
| — | Linux-guest workloads: Memcached, other net (C3b) | **gated (guest OS + R3)** / frontier | Part B Linux, R3 net/fault | KV/net workload under O1/O2 |

### Recommended order
1. **17** (harness) and **18** (sweep) in parallel — 17 is pure-logic Mac work, 18 is payloads
   + box goldens. Together they make O1/O2/O3 real on the cheapest corpus.
2. **22** (writable `Block` device) → **21** (device-verb sweep) → **20** (SQLite-with-disk):
   build a block device that *works*, prove it with the cheap conformance sweep, then drive the
   *large* real workload across it and run **O1** (pause at checkpoints, compare two runs) —
   "same workload ⇒ identical VM behavior" through real block writes. No fault model in this leg.
3. **19** (fuzzer) seeded from 18's corpus — fast tier first (Mac), real-KVM tier on the box.
4. Land **R3** (net/fault) + a guest OS with the user; then spec the storage fault model (**23**,
   crash-consistency) and C3b.

## Non-goals

- Replacing `unison` — this builds *on* it; the generic bisector stays domain-free.
- A general guest OS — C3a deliberately *exercises* the device boundary (`Block` service,
  host-RAM-backed) but needs no guest OS; C3b waits on a guest OS + R3.
- Performance benchmarking — speedtest1 is a *correctness* workload here, timing is irrelevant
  (and V-time-derived anyway).
- Hosting the engine inside Nyx — see borrow table.
