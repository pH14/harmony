# LAYERS — the capability-layering ruling

> **Status: RULED (Paul, 2026-07-06). R-L1/R-L2/R-L3 AMENDED 2026-07-12** after the Dissonance
> observation-contract review (see the amendment notes). Companion to `docs/GLOSSARY.md`
> (the naming authority) and `docs/ARCH-BOUNDARY.md` (the ISA seam); **amends
> `tasks/43-harmony-linux-tier.md`** (the payload carve-out and the SDK rulings below postdate
> its surface list). Like the glossary: binding on new code when ratified; physical moves ride
> their natural work (task 43's window, issue #74's rename window) — no big-bang.

## The claim

Dissonance's capabilities layer as **exploration → perturbation → judgment**, and the current
tree under-expresses the first seam: the searcher is useful — and must be validated — **with
zero faults**, and the guest SDK is a **cross-cutting product contract** (the adopted external
surface plus per-guest-world transport and normalized host schema), not an internal of consonance
or dissonance.

Industry data point (grounds the exploration claim):
[Antithesis, "Testing Exploration via Metroid" (2025)](https://antithesis.com/blog/2025/metroid/)
— they validate their exploration engine on a game with **no fault injection anywhere**: position
tuples fed to a `SOMETIMES_EACH` assertion ("equalizes exploration for each unique value of the
tuple"), entropy purely from stochastic input decisions, multi-objective preference on top ("all
things being equal, prefer states with more missiles"). Two transferable lessons: (1) exploration
quality is measurable, and worth measuring, decoupled from fault quality; (2) the assert
vocabulary is not primarily judgment machinery — it is the **workload-to-searcher guidance
interface**, and it works faults-off.

## The layers

| Layer | What | Current precursor / target owner | Fault-free useful? |
|---|---|---|---|
| **L0 machine** | deterministic substrate: branch/run/snapshot/hash, the `Moment` axis, the supply seams, `StopReason` as machine contract | `consonance/` | n/a |
| **L1 exploration** | the generic Explorer control loop plus Differential-materialized observations, cells, and archive view, over the **supply** vocabulary (Entropy, Payload) | current `dissonance/explorer`; target `explorer` + `sdk-events` + Differential plane (`hm-bbx`, not implemented) | **yes — by construction** (see R-L1 evidence) |
| **L2 perturbation** | the fault vocabulary + enforcement: fault classes, `HostFault`, flow, tactic arms | `dissonance/environment` (fault tier), `flow`, `tactics-regime` | opt-in per campaign; `FaultPolicy::none()` is the opt-out |
| **L3 judgment** | properties, oracles, triage, resolution | current `link`/`matcher`; target Explorer oracles + Differential reporting + Resolution | yes (crash/property oracles need no fault) |
| **cross-cutting** | the app-facing SDK surface (the Antithesis SDK, adopted) over per-guest-world transports | external MIT SDKs and current internal `guest/sdk`; `/dev/harmony` adapter remains target work | yes |

## Rulings

### R-L1 — Exploration is a first-class, independently-gated capability

> **Amendment (2026-07-12).** The independent fault-free gate, real workload, deterministic
> controls, and signal-versus-random measurement survive. The legacy task-84 composition through
> `LinkSensor`/feature channels and its role as a task-70 on-ramp do not. The production gate now
> follows `docs/DISSONANCE-STRATEGY.md`: normalized SDK evidence, Differential-materialized cells at
> actual seals, the generic `Explorer`, and a simple selector before any advanced selector work.
> Task 84 is retained as experimental history and trial-discipline input; its implementation path
> is blocked on the rewritten cooperative vertical (`hm-bbx` / `hm-cs5`).

The control loop is fault-blind by construction: `Machine` branches/runs/snapshots/hashes;
`Selector` chooses Entries; `CellFn` projects completed observations; none needs fault vocabulary.
Supply classes such as Entropy and Payload remain distinct from faults. A campaign under
`FaultPolicy::none()` can find payload-driven bugs and must be independently useful.

**Ruled:** exploration gets its own gate, decoupled from faults. The live gate uses the generic
`Explorer` + `SocketMachine`, normalized cooperative evidence, Differential-materialized cells at
actual seals, a simple selector, and equal-budget pure-random/frontier-off controls. It reports
cells and depth as diagnostics and held bug/progress behavior as product evidence. Advanced
selection is downstream of this gate.

Task 84 is the historical maze and trial-discipline specification. Bead `hm-cs5`, after `hm-bbx`,
owns the rewritten implementation. Fault enforcement work remains out of scope.

**Explicit non-move:** `explorer` stays under `dissonance/`. A fault-free searcher is still
adversarial in mission (find the state that breaks; some states need no fault). This ruling
corrects framing and gates, not the crate family.

### R-L2 — Evidence declarations are data; campaign roles are projections (AMENDED 2026-07-12)

The earlier ruling merged SDK `PointKind` with matcher `Role` and promoted a generic property
catalog into the spine. That conflated source-declared evidence with campaign interpretation and is
superseded by `docs/GLOSSARY.md` plus `docs/DISSONANCE-STRATEGY.md`.

**Ruled:** SDK declarations normalize into persisted `SdkSchema`; scrape and instrument sources
retain their own versioned declarations. Matcher/campaign `Role` remains a query or projection over
that evidence and cannot redefine its base temporal update semantics. The unified never-fired view
is derived reporting—declared identities minus matching occurrences—not a reason to merge source
enums or make a decoder own policy. The generic Explorer consumes materialized observations and
cells; it does not depend on an SDK wire, JSON schema, or transport-specific kind.

The current serial-console scrape declares source-local, stop-granular ordering. It is full-run
evidence only—not an exact seal-relative cell source or a participant in cross-source sequence
queries—until capture-time stamps and a snapshot cursor exist. The precise promotion contract lives
in `docs/DISSONANCE-STRATEGY.md`.

Corollary (validated against the Metroid workload): Antithesis's `SOMETIMES_EACH(x, y)` is an
*app-declared cell function*. Under the thin-SDK ruling (hooks + transport only; the host owns
campaign interpretation), Harmony normalizes the app's observations and lets the campaign's
`CellFn` project `(x, y)` host-side—retunable without recompiling the guest. **The thin-SDK ruling
is reaffirmed**; app-declared cells are rejected. The genuine gap the Metroid post exposes is
elsewhere: "prefer more missiles, all things equal" is deterministic quality domination within a
cell. It belongs to Differential archive occupancy, not to a source schema or advanced selector.

### R-L3 — The app-facing SDK: adopt the Antithesis SDK surface (AMENDED 2026-07-12)

> **Amendment note.** The original R-L3 (same day, earlier) ruled a custom top-level
> `harmony-sdk` as the app-facing interface, over an interface-owned wire and per-guest-world
> backends ("the SDK triplet"). A same-day review of the Antithesis SDKs' licensing and
> internals (Paul, 2026-07-06) superseded the interface half: their licensing permits direct
> adoption, and their internal architecture practically invites a compatible backend. The
> 2026-07-12 observation-contract review narrows the earlier sufficiency claim: their surface is a
> strong external base, but numeric properties are not automatically persistent state and
> structured choice remains a possible extension. The wire and backend thirds of the triplet
> survive in amended form below. Original text is in git history (PR #75).

**Ruled:**

1. **The default app-facing interface is the Antithesis SDK surface, consumed unmodified.** Eight
   languages (Go, Java, C, C++, JavaScript, Python, Rust, .NET); **MIT-licensed** (verified on
   the Rust and Go repos; AGPL-3-compatible for harmony and for guest workloads). `Moment` stamping
   is host-side; their compile-time assertion declarations cover property identity and never-fired
   expectations; the assertion message aggregates sites into one property (see the official
   [assert module](https://antithesis.com/docs/generated/sdk/rust/antithesis_sdk/assert/)). Ordinary
   evaluations are occurrence/property evidence and do not fail-stop (see
   the official [assertion semantics](https://antithesis.com/docs/properties_assertions/assertions/)).
   Numeric-guidance verbs explicitly optimize an extremum and may filter reports to new watermarks
   (see the generated Rust
   [guidance implementation](https://antithesis.com/docs/generated/sdk/rust/src/antithesis_sdk/assert/guidance.rs.html)),
   so Harmony may normalize the declared maximum/minimum but cannot reinterpret that stream as
   arbitrary current `set` state. A versioned workload instrumentation declaration may supply
   another contract only for an emission path that actually reports every required update. A
   Harmony extension is justified when that is inadequate. Persistent state registers and
   identified decision sites (buggify / structured choice—see item 6) are the concrete triggers
   now under evaluation.
   Raw numeric JSON is preserved. It remains report-only until `sdk-events` can normalize it into a
   bounded exact representation with a deterministic total order; host `f64` comparison is never
   state-affecting.

2. **The canonical guest transport is a char device (`/dev/harmony`) in the harmony-linux
   kernel.** A small driver rides the existing `0x0CA1` doorbell — **zero consonance change**.
   `write()` = emit; write-then-`read()` = request/response. The driver stamps caller
   attribution into each frame (userspace RIP from `pt_regs`, pid/comm; ASLR caveat: steering
   tables key on module+offset, symbolized offline against the pinned image). This dissolves
   the proven privilege blocker — the kata-config kernel ships no `IOPL`/`DEVPORT`, so guest
   processes cannot do port I/O, and the flow-agent's root-only `/dev/mem` convention (which
   task 73 had planned to extend to apps) is **superseded for applications**. Unprivileged,
   language-agnostic (any language opens/writes/reads a file), synchronous (crash-robust; the
   hypercall fires inside `write()`, so attribution is `Moment`-precise).

3. **Compat adapter: harmony ships a clean-room `libvoidstar.so`** in harmony-linux images.
   The SDKs detect their environment by `dlopen` at a fixed path and route everything through
   ~5 symbols; the shim maps `fuzz_json_data` → device write, `fuzz_get_random` → device
   write+read (answered from the seeded **supply** stream at its `Moment`; SDK-side
   `random_choice` modulo is deterministic guest code), `fuzz_flush` → no-op, and **stubs the
   coverage pair** (`init_coverage_module` / `notify_coverage`) unless the instrument tier ever
   activates. Precedent: bedrock's `guest/libvoidstar.c` (a coverage-only voidstar shim over
   its own feedback buffer, with a second LLVM `trace-pc-guard` frontend). **License
   discipline: bedrock is GPL-2.0 — never copy its code; implement from the MIT SDK source.**
   **Version-skew risk owned:** the voidstar ABI is Antithesis's undocumented internal
   contract — pin SDK versions in workload images; MIT fork rights are the backstop.

4. **Decode is owned once, and no JSONL file watcher is built.** `fuzz_json_data` carries the
   same JSON that Antithesis's documented fallback protocol writes to
   `$ANTITHESIS_OUTPUT_DIR/sdk.jsonl`, so `dissonance/sdk-events` grows **one** Antithesis-JSON
   decoder serving all device traffic (shim or native writers). The fallback-file path is
   explicitly **not** built: it was never zero-touch (an app must deliberately implement the
   protocol; harmony's zero-touch channel is the scrape tier), and the device subsumes it — a
   no-SDK app writes the same JSON to `/dev/harmony`. Escape hatch if a fallback-protocol
   workload ever materializes: `mknod` the device at `$ANTITHESIS_OUTPUT_DIR/sdk.jsonl` —
   packaging, not architecture.

5. **The existing `guest/sdk` `no_std` crate demotes to internal wire plumbing.** Its
   byte-deterministic Event wire stays for the bare-metal corpus payloads and guest-resident
   agents (merged, box-gated code); application traffic uses the Antithesis JSON over the
   device. `sdk-events` owns both decoders and normalizes both formats into persisted `SdkSchema`
   and ordered `SdkEvent` data. Binary v1's catalog does not declare value shape or a fixed base
   operation. Fired events may validate a candidate interpretation, but a never-fired state point
   remains explicitly unresolved and cannot enter temporal reduction. Conflicting per-event
   operations are malformed evidence. The first production state-register vertical therefore
   requires a versioned binary declaration (wire v2) or an equally explicit workload
   instrumentation declaration; it may not infer state semantics from silence.

6. **The steering ladder** — how the search gets smarter without SDK changes: (i)
   **attribution**, now, via the driver stamp; (ii) **empirical probing** — fork a decision,
   answer it k ways, observe divergence: an engine capability the seams already anticipate
   (`Machine::run(resolve)`, `environment::Outcome::NeedsHost`), Antithesis-proven, needs no
   vocabulary; (iii) **structure at the call** — a `Choice { n }` `DecisionClass` answered by
   index, giving one-replay semantic mutation instead of k-replay probing. (iii) is **parked
   with a named trigger**: the mutation axis extending to supply answers.

R-L2's 2026-07-12 amendment supplies the boundary: source declarations are data, campaign roles
are projections, and the generic Explorer consumes materialized observations and cells rather than
transport-specific properties. The thin-SDK ruling is reaffirmed—adopting the Antithesis surface
adds no cell policy to the guest. Task 43's naming convention holds: the driver and shim are
`harmony-linux` deliverables, per-guest-world by construction.

### R-L4 — `guest/` → `harmony-linux/`, with the corpus carve-out (amends task 43)

Task 43 already specs the move and the consonance-workload audit; it remains the vehicle. Two
amendments, ruled 2026-07-06:

1. **`guest/payloads` + `guest/golden` do NOT move into `harmony-linux/`** — they move to
   consonance's test surface. Rationale: they are the C1 determinism corpus — bare-metal,
   not Linux, and validation of the *engine*, not part of any swappable guest world. This
   *strengthens* task 43's own principle: a `harmony-<env>` is one of many possible guest
   worlds; the conformance corpus is not swappable, so it cannot live inside one. (Task 43's
   Deliverable A currently says "keep internal layout: payloads/ linux/ golden/ …" — superseded
   on this point.)
2. **Task 43's surface list predates tasks 61/73** and must extend to `guest/flow-agent`
   (→ `harmony-linux/`, it is the exemplar guest-world artifact) and `guest/sdk`
   (→ demoted to internal wire plumbing per R-L3 item 5); the `/dev/harmony` driver and the
   `libvoidstar.so` shim (R-L3 items 2–3) are **new** harmony-linux deliverables.

The known landmine stands: `MANIFEST.sha256`-hashed build inputs (`lib-build.sh` paths) force a
rebaseline if renamed — task 90's ruling (fold the stale-string cleanup into task 43's
rebaseline if one is scheduled, else document-as-deliberately-stale) applies unchanged.

## Constraints — what must NOT be relayered

1. **The one-reproducer invariant.** Supply and faults share the single `Moment`-keyed artifact
   and the one hashed stream (the task-61 net_decide ruling). Layering the *vocabulary* must
   never fork the *artifact*: a fault-free campaign opts out via **policy**
   (`FaultPolicy::none()`), never via a schema split.
2. **The stream-separation proofs.** Supply vs. fault PRNG domains live in one `SeededEnv` so
   the byte-identical gates (task 73: buggify on/off leaves the supply stream untouched) stay
   provable in one place.
3. **`StopReason::Assertion` stays a machine contract where the transport is fail-stop.** The
   current binary path may surface that terminal; `TerminalOracle` assigns meaning. Antithesis JSON
   assertions may instead be completed-trace evidence and are judged by an SDK-assertion Oracle.
   Decoding evidence never gains live stop authority by accident.

## What this unblocks / changes

- **Task 84 (historical specification)**: its fault-free workload and trial discipline feed the
  rewritten cooperative gate `hm-cs5`; its LinkSensor composition and task-70 on-ramp are
  superseded.
- **Future selector experiments**: follow the simple cooperative baseline and remain separate from
  quality domination, Portfolio allocation, and STADS reporting/stopping.
- **Task 43 (amended)**: corpus carve-out; surface extended to flow-agent, the SDK demotion,
  and the two new harmony-linux deliverables (the `/dev/harmony` driver + the voidstar shim).
- **`sdk-events`**: one Antithesis-JSON decoder shared by shim and native device traffic, plus the
  retained internal binary decoder; both normalize into the same persisted observation contract.
- **The instrument tier's cheap mechanism, if ever activated**: the shim's coverage pair over a
  guest-side buffer (bedrock's `libfeedback` pattern, which also fronts LLVM `trace-pc-guard`)
  — named as a future the seams permit, not a work item.
- **Vocabulary note** (glossary addendum): the loops' history is Timeline/Multiverse →
  Modulation/Progression (task 94) → rollout/step (glossary). "Timeline" now names the
  **data-noun** (one execution history) — any surviving loop-sense use of the word is legacy.

## Non-goals

- Moving `explorer` (or any crate) out of `dissonance/` — ruled against in R-L1.
- App-declared cell functions / any fattening of the SDK — ruled against in R-L2.
- A physical restructure PR from this doc — moves ride task 43, issue #74, and each crate's
  natural work, per the ARCH-BOUNDARY precedent and the glossary's sequencing rule.
- Authoring per-language SDKs — superseded by adopting the Antithesis surface (R-L3).
- A JSONL file watcher — ruled out in R-L3 item 4 (the device subsumes the fallback protocol).
- The structured-decide verb and the shim's in-process coverage buffer — parked with named
  triggers (R-L3 items 3 and 6), not work items.
- `harmony-freebsd` and the instrument tier — named as futures the seams must permit, not work
  items.
