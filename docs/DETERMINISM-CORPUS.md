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
| **O3** | **Seed-sensitivity** (non-triviality / anti-cheat) | compares a guest-**observable output** digest (`out_*`), **not** `state_hash` (which includes the seed-derived latent entropy state — see task 17 note): RNG-consuming + control-flow-stable payload under two *different* seeds → assert `work_a == work_b` **and** `out_a != out_b`; pure payload → assert `out_a == out_b` | the two failure modes O1 alone can't see: **faked** determinism (RNG wired to a constant → passes O1 trivially) and **seed-leaked** nondeterminism (seed reaching state it shouldn't). Needs a `unison::Subject` observable-output accessor (additive; [question]) |
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

> **Struck and retargeted (task 62).** Tasks 22 (host `BLOCK_WRITE` to real storage) and 23
> (crash-consistency) were struck by the Wave-3 design decision: storage is **RAM-backed inside
> the guest** (brd / loop-over-ext4-image — real ext4, real `fsync`, contents already in the
> hashed/snapshotted guest RAM), not a host `Block` service (see `docs/ROADMAP.md`). The
> "known useful real workload" slot below is **Postgres-on-RAM**, already delivered by tasks
> 36–38/48/49 (bare Postgres → runc → k3s, each deterministic-twice), not the SQLite-over-`Block`
> design this section originally described. The SQLite-freestanding-VFS design is kept below only
> as historical record of the struck approach — it is **not** on the corpus backlog.

The original unblock-without-a-Linux-guest idea: **SQLite needs no OS** — it talks to storage only
through a pluggable VFS (`SQLITE_OS_OTHER` + a registered custom VFS), so a freestanding payload
could map its file ops straight onto a `Block` hypercall service. This is struck (see above); the
corpus's real-workload entry is Postgres-on-RAM instead.

- **C3a — Postgres-on-RAM (retargeted, task 62).** Bare Postgres (task 37) escalating to
  runc/k3s (tasks 48/49), running against RAM-backed ext4 inside the guest. Determinism proof is
  the same O1 shape (same seed twice ⇒ identical `state_hash`) already demonstrated end-to-end by
  those tasks; no `Block`-service hard dependency, no host-side write path. *(Historical: this
  slot originally named "SQLite-with-disk over the `Block` service," a freestanding payload with
  a custom VFS mapping `xRead`/`xWrite`/`xFileSize`/`xTruncate`/`xSync` onto a host `Block`
  hypercall — struck with tasks 22/23.)*
- **C3b — Linux-guest workloads (gated).** Memcached + `memtier_benchmark`, etc. These need a
  guest OS (now available — task 34+). Their **I/O is already mostly determinized by
  construction** (see "Device determinism" below): storage is guest-RAM-backed ext4; the network
  is the per-flow, host-decides/guest-enforces model (task 50). C3b waits on a general Linux
  workload slot opening up on the queue, not on any further device-model ruling.

## Staging & dependency gates

The discipline (from ROADMAP): don't spec a task whose interface depends on an unmade
decision. C1 and the fast C2 tier depend only on merged crates + the existing Part-A pipeline;
C3a (Postgres-on-RAM) is **already delivered** by tasks 36–38/48/49; the gated tail (C3b) waits
on queue space, not a new device-model ruling. Tasks 22/23 are struck (see above).

### Device determinism (why disk/external I/O is mostly already solved)

The plan for disk and external devices is *there are no devices* — everything is the one
**synchronous** hypercall channel (the port-I/O doorbell on stock KVM, `docs/INTEGRATION.md`
§1 — historically sketched as `VMCALL` before task 20; see `PLAN.md` Part B's notes), which
removes both halves of the ReVirt reduction up front:

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
- **Disk write — struck (task 22, retired by task 62).** The original near-term plan was a
  `BLOCK_WRITE` opcode on a host `ServiceId::Block`. Superseded: writable storage is supplied
  **inside the guest** instead — a RAM-backed ext4 (brd/loop), whose contents live in
  already-hashed guest RAM, needing no new hypercall service. See `docs/ROADMAP.md`.
- **Storage fault model — deferred (task 23, D1).** Deterministically lose / reorder / tear
  un-`fsync`'d writes at a crash point → the durability / crash-consistency capability the
  mission ultimately exists for. Real, but genuinely deferred — the guest-RAM-backed approach
  above cannot express it (an instant/no-op `fsync` on RAM has no durable-vs-volatile split), so
  this capability waits on **D1**, a host-side snapshot-store-backed RAM-disk model, tracked in
  `docs/ROADMAP.md`'s deferred register — not task 23 in its original host-`Block`-write shape.
- **Network / external — the per-flow, host-decides/guest-enforces model (task 50).** No
  external IRQ lines; distribution is simulated *inside* the guest as containers on the intra-guest
  CNI, with the host answering a per-flow policy (`NetFlow`) that the guest enforces with its own
  mechanisms (netem/tbf/nftables). See `docs/DISSONANCE.md`'s guest fault model. *(Historical: this
  bullet originally named a host-side fault-injecting bridge and the retired R3/task-11 fault
  schedule and `dissonance/pv-net`, task 26 — both retired by task 50; there is no host-routed
  frame stream.)*

### Current device surface — what exists vs. what a standard system needs

Full findings and the (struck) writable-storage plan: **`docs/BLOCK-DEVICE.md`** — historical
grounding doc for tasks 22/23, both struck; see the notes above and `docs/ROADMAP.md`.

The hypercall surface is deliberately minimal (Antithesis "real device models removed") and is
genuinely thin today. The **complete** set of services is `Console, Entropy, Block, Event`; the
guest's whole callable API is `console_write` / `entropy_fill` / `block_capacity` / `block_read`
/ `event_emit`.

| Primitive | Status | For running a standard system |
|-----------|--------|-------------------------------|
| Console (out) | ✅ output-only (`console_write`) | logs / serial |
| Entropy | ✅ deterministic (`entropy_fill`) | `/dev/random` source |
| Block **read** | ✅ read-only | boot a read-only rootfs (live-CD model) |
| Block **write** | N/A — **struck (task 22)**; solved instead by guest-RAM-backed ext4 (brd/loop), not a host service | persistent storage (DBs, `/var`) — delivered this way by tasks 36–38/48/49 (Postgres-on-RAM through k3s) |
| Clock / TSC | ✅ V-time | all guest clocks |
| Interrupts | ✅ LAPIC timer (V-time) | only async event; push input arrives this way |
| Data **input** (host→guest) | ❌ not in the ABI (`INTEGRATION.md:30`) | interactive stdin; most servers don't need it |
| **Network** | intra-guest ✅ (comes free with the Linux guest, task 34+); external/fault-injected ❌ (deferred) | see below |

**Network is not a host device in this model — by design, not omission.** A distributed system
runs *inside one guest* as containers on the guest's own network stack (loopback/bridge/veth) on
deterministic CPU+RAM, so it comes **free with a Linux guest** — there is no emulated NIC to
build, and no host-side switch (`dissonance/pv-net`, task 26, was retired by task 50). What is
genuinely absent is the *external-net* escape (guest ↔ real world) — deliberately deferred,
outside Wave 4's scope.

**vmm-core bring-up and Linux-guest boot were the near-term blockers, and are now delivered**
(task 34+ through Wave 3's Postgres/k3s escalation). Persistent storage is guest-RAM-backed ext4
(no host `Block`-write dependency, task 22 struck); intra-guest networking comes with the Linux
guest; external/fault-injected networking is deliberately deferred.

### Gating

- C1/C3a goldens and the real-KVM C2 tier are **box-gated** (need `/dev/kvm` + patched KVM);
  the harness logic, manifest, conformance differ, and the fast C2 tier are **Mac-testable**
  against `ToyMachine` / `MockBackend` — same split as every other crate (`CODE-QUALITY.md`).

## Borrow inventory (the "steal from existing projects" answer)

| Source | Borrow | For | Caveat |
|--------|--------|-----|--------|
| **kvm-unit-tests** | freestanding bare-metal CPU/KVM test kernels (tsc, apic, msr, vmx) | C1 instruction sweep — ready-made cases | needs an entry/console shim to our Part-A protocol; their harness assumes QEMU testdev ports |
| **SQLite** amalgamation + `speedtest1.c` + `sqllogictest` *(historical — struck)* | originally: a freestanding workload via a **custom VFS over a `Block` service** (`SQLITE_OS_OTHER`) | *(struck; C3a is now Postgres-on-RAM)* | superseded by tasks 22/23 being struck — see C3 above |
| **Memcached** + `memtier_benchmark` / `mc-crusher` | canonical KV/net workload + load generator | C3b (gated) | needs net + Linux guest |
| **rr** test suite; **Antithesis prior art** (`preestablished/determinism-hypervisor`, `oss-garage/bedrock`, see [[prior-art-det-hypervisors]]) | determinism test ideas & corpora to mine | C1/C2 | adapt to our hypercall protocol |
| **Nyx / kAFL** | *snapshot-reset loop mechanics only* (`RESEARCH.md:81`) | C2 real-KVM tier speed | do **not** host the engine inside Nyx (nested virt breaks PMU/TSC determinism) |

## Deliverable structure

- **`consonance/det-corpus/`** *(task 17)* — host-side oracle runner. Generic over `unison::Subject`/
  `SubjectFactory`; defines the corpus manifest, the O1–O3 oracle runners, the conformance
  differ, and the JSON report. Pure-logic, Mac-testable with `ToyMachine`; pointed at
  `vmm-core::Vmm<B>` at integration. Composes `unison` (this is integration-class, so the
  "no sibling deps" rule of wave-1 parallel crates doesn't apply — it's the layer that *binds*).
- **`guest/payloads/`** *(task 18)* — the C1 micro-payloads, via the documented "add a payload"
  flow; goldens in `guest/golden/`.
- **`guest/workloads/postgres/` + k3s tier** *(tasks 36–38, 48, 49 — delivered)* — the C3a
  real workload: bare Postgres escalating through runc to a single-node k3s cluster, all against
  guest-RAM-backed ext4. Supersedes the struck task 20 (SQLite-over-`Block`).
- **Tasks 22 and 20 are struck** (host `BLOCK_WRITE` device + the SQLite-over-`Block` workload
  that depended on it) — see C3 above and `docs/ROADMAP.md`.
- **storage fault model** *(D1, deferred)* — deterministic lose/reorder/tear of un-`fsync`'d
  writes at a crash point → durability/crash-consistency, the mission's eventual marquee
  capability. Needs a host-side snapshot-store-backed RAM-disk model (D1 in `docs/ROADMAP.md`'s
  deferred register) since guest-RAM-backed ext4 cannot express it. *(Historical: this slot was
  task 23, "sequenced after 22 + the SQLite determinism test" — both struck.)*
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
| 22 | ~~Writable `Block` device~~ | **struck** (task 62) | — | superseded by guest-RAM-backed ext4 (tasks 36–38) |
| 21 | ~~Device-verb conformance sweep for `Block`~~ | **struck** (depended on 22) | — | — |
| 20 | ~~SQLite-with-disk workload~~ | **struck** (depended on 22) | — | superseded by Postgres-on-RAM (36–38/48/49) |
| 36–38, 48, 49 | Postgres-on-RAM → runc → k3s (C3a, delivered) | **merged** | task 04 pipeline, guest-RAM-backed ext4 | real workload, deterministic-twice, O1-equivalent proof already run |
| 23 | Storage fault model (crash-consistency) | **deferred (D1)** | D1 host-side RAM-disk model | seed-scheduled lose/reorder/tear-on-crash; durability assertions |
| — | Linux-guest workloads: Memcached, other net (C3b) | **gated (queue space)** | Linux guest (delivered), net-fault boundary (task 50, delivered) | KV/net workload under O1/O2 |

### Recommended order

*(Historical note: step 2 below — 22 → 21 → 20, the writable-`Block`-device leg — was struck by
task 62; the real workload it targeted, Postgres-on-RAM through k3s, is already delivered by
tasks 36–38/48/49.)*

1. **17** (harness) and **18** (sweep) in parallel — 17 is pure-logic Mac work, 18 is payloads
   + box goldens. Together they make O1/O2/O3 real on the cheapest corpus.
2. **19** (fuzzer) seeded from 18's corpus — fast tier first (Mac), real-KVM tier on the box.
3. C3b (Memcached/other net workloads) whenever queue space opens up — its prerequisites (Linux
   guest, net-fault boundary) are already delivered.
4. Storage fault model (D1, crash-consistency) after task 60's first campaign, per
   `docs/ROADMAP.md`'s deferred register.

## Non-goals

- Replacing `unison` — this builds *on* it; the generic bisector stays domain-free.
- A general guest OS — no longer applicable; the Linux guest is delivered (task 34+). C3b
  workloads wait on queue space, not a guest OS.
- Performance benchmarking — speedtest1 is a *correctness* workload here, timing is irrelevant
  (and V-time-derived anyway).
- Hosting the engine inside Nyx — see borrow table.
