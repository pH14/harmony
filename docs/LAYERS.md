# LAYERS — the capability-layering ruling

> **Status: RULED (Paul, 2026-07-06). R-L3 AMENDED same day** after a licensing/architecture
> review of the Antithesis SDKs (see R-L3's amendment note). Companion to `docs/GLOSSARY.md`
> (the naming authority) and `docs/ARCH-BOUNDARY.md` (the ISA seam); **amends
> `tasks/43-harmony-linux-tier.md`** (the payload carve-out and the SDK rulings below postdate
> its surface list). Like the glossary: binding on new code when ratified; physical moves ride
> their natural work (task 43's window, issue #74's rename window) — no big-bang.

## The claim

Dissonance's capabilities layer as **exploration → perturbation → judgment**, and the current
tree under-expresses the first seam: the searcher is useful — and must be validated — **with
zero faults**, and the guest SDK is a **product-level contract** (harmony's, with per-guest-world
backends), not an internal of consonance or dissonance.

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

| Layer | What | Lives today | Fault-free useful? |
|---|---|---|---|
| **L0 machine** | deterministic substrate: branch/run/snapshot/hash, the `Moment` axis, the supply seams, `StopReason` as machine contract | `consonance/` | n/a |
| **L1 exploration** | the searcher: spine six + engine + archive + sensors/cells, over the **supply** vocabulary (Entropy, Payload) | `dissonance/explorer` + sensor crates | **yes — by construction** (see R-L1 evidence) |
| **L2 perturbation** | the fault vocabulary + enforcement: fault classes, `HostFault`, flow, tactic arms | `dissonance/environment` (fault tier), `flow`, `tactics-regime` | opt-in per campaign; `FaultPolicy::none()` is the opt-out |
| **L3 judgment** | properties, oracles, triage, resolution | `link`/`matcher` plugins, `docs/RESOLUTION.md` | yes (crash/property oracles need no fault) |
| **cross-cutting** | the app-facing SDK surface (the Antithesis SDK, adopted) over per-guest-world transports | external MIT SDKs + `guest/sdk` (demoted — see R-L3) | yes |

## Rulings

### R-L1 — Exploration is a first-class, independently-gated capability

The spine is already fault-blind by construction — `Machine` is branch/run/snapshot/hash;
the explorer never parses the reproducer (schema-blind, opaque bytes); `Archive`/`Selector`/
`CellFn`/`Sensor` carry no fault vocabulary; the catalog's **supply classes** (Entropy, Payload)
are exploration-tier, distinct from the fault classes. A campaign under `FaultPolicy::none()` is
a coverage-guided, snapshot-branching fuzzer over the payload/entropy channels with perfect
reproducibility — and it can find **real bugs** (payload-driven crashes need no fault). The
tactic portfolio already names this configuration: the `quiet` arm (`docs/EXPLORATION.md`).

**Ruled:** exploration gets its own gate, decoupled from faults — the Metroid discipline.
Shape: *cells discovered / depth reached vs. a random-seed baseline, zero faults
(`FaultPolicy::none()`, buggify off), on a real guest, driving the real spine engine
(`explorer::Explorer` + `SocketMachine`)*. This gate is deliberately also the **task-70
on-ramp**: it exercises the composed engine against real hardware — which has never happened
(the box-proven campaign ran conductor's hand-rolled loop) — while removing fault *enforcement*
(the half-finished part: net 61b, block/process unenforced) from the equation entirely.

Spec as **task 84** (next free in the active band): a game-shaped benchmark workload — a small
deterministic game or maze under the Linux guest, SDK state registers as position markers (the
`sdk-demo` pattern grown up) — plus the gate above. Two sub-questions the spec must answer, not
this doc: the workload choice, and the baseline definition (pure random seeds vs. frontier-off).

**Explicit non-move:** `explorer` stays under `dissonance/`. A fault-free searcher is still
adversarial in mission (find the state that breaks; some states need no fault). This ruling
corrects framing and gates, not the crate family.

### R-L2 — The property catalog is spine vocabulary; channels are transports

The semantics of `always` / `sometimes` / `reachable` / `never` already exist **twice**, once
per channel: SDK-declared (`link`'s `PointKind`, `AlwaysViolation`, `LinkSensor`) and
config-declared over scraped records (`matcher`'s `Role` router — a declared `never` over log
records *is* an always-assertion, no SDK involved). The catalog / never-fired report is already
"unified across link and scrape" as a report format. `docs/GLOSSARY.md` merged the enums
(`PointKind` + `Role` → one spine `Role`).

**Ruled — the promotion this implies:** the **property catalog** — the declared point set with
roles, plus fired-set accounting — becomes spine vocabulary (owned by `explorer`, rule 2:
interfaces live in the consumer). The SDK is **one transport** for declaring and firing points;
scrape/config is another; a future instrument tier would be a third. The searcher understands
`assert_sometimes` abstractly; no spine or engine code may reference the SDK, its wire format,
or any channel specifically.

Corollary (validated against the Metroid workload): Antithesis's `SOMETIMES_EACH(x, y)` is an
*app-declared cell function*. Under the thin-SDK ruling (hooks + transport only; the host owns
every interpretation) harmony expresses the same thing host-side — the app emits
`state_set(x)`/`state_set(y)`, the campaign's `CellFn` keys cells on those channels — retunable
without recompiling the guest. **The thin-SDK ruling is reaffirmed**; app-declared cells are
rejected. The genuine gap the Metroid post exposes is elsewhere: their "prefer more missiles,
all things equal" is **multi-objective preference inside the archive** (value-weighted
best-per-cell domination), which the spine's `Archive`/`Reward` does not have. Logged as a
task-70 design input, not ruled here.

### R-L3 — The app-facing SDK: adopt the Antithesis SDK surface (AMENDED 2026-07-06)

> **Amendment note.** The original R-L3 (same day, earlier) ruled a custom top-level
> `harmony-sdk` as the app-facing interface, over an interface-owned wire and per-guest-world
> backends ("the SDK triplet"). A same-day review of the Antithesis SDKs' licensing and
> internals (Paul, 2026-07-06) superseded the interface half: harmony has **no verb their
> surface cannot express today**, their licensing permits direct adoption, and their internal
> architecture practically invites a compatible backend. The wire and backend thirds of the
> triplet survive in amended form below. Original text is in git history (PR #75).

**Ruled:**

1. **The app-facing interface is the Antithesis SDK surface, consumed unmodified.** Eight
   languages (Go, Java, C, C++, JavaScript, Python, Rust, .NET); **MIT-licensed** (verified on
   the Rust and Go repos; AGPL-3-compatible for harmony and for guest workloads). A custom
   `harmony-sdk` is **deferred** until harmony has a verb their surface cannot express. The
   audit grounding the deferral: `Moment`-stamping is host-side (no interface delta); their
   compile-time assertion catalog (`linkme` distributed slices) covers catalog-at-init and
   never-fired; their numeric-guidance family (`assert_sometimes_greater_than` …) covers the
   IJON state-register use. Parked candidate for a future native verb: identified decision
   sites (the buggify / structured-choice family — see item 6).

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
   `$ANTITHESIS_OUTPUT_DIR/sdk.jsonl`, so `dissonance/sdk-link` grows **one** Antithesis-JSON
   decoder serving all device traffic (shim or native writers). The fallback-file path is
   explicitly **not** built: it was never zero-touch (an app must deliberately implement the
   protocol; harmony's zero-touch channel is the scrape tier), and the device subsumes it — a
   no-SDK app writes the same JSON to `/dev/harmony`. Escape hatch if a fallback-protocol
   workload ever materializes: `mknod` the device at `$ANTITHESIS_OUTPUT_DIR/sdk.jsonl` —
   packaging, not architecture.

5. **The existing `guest/sdk` `no_std` crate demotes to internal wire plumbing.** Its
   byte-deterministic Event wire stays for the bare-metal corpus payloads and guest-resident
   agents (merged, box-gated code); application traffic uses the Antithesis JSON over the
   device. Consolidating the two wires is deferred to natural work.

6. **The steering ladder** — how the search gets smarter without SDK changes: (i)
   **attribution**, now, via the driver stamp; (ii) **empirical probing** — fork a decision,
   answer it k ways, observe divergence: an engine capability the seams already anticipate
   (`Machine::run(resolve)`, `environment::Outcome::NeedsHost`), Antithesis-proven, needs no
   vocabulary; (iii) **structure at the call** — a `Choice { n }` `DecisionClass` answered by
   index, giving one-replay semantic mutation instead of k-replay probing. (iii) is **parked
   with a named trigger**: the mutation axis extending to supply answers.

R-L2 is untouched and is what makes this amendment cheap: channels are transports, and the
spine understands properties abstractly. The thin-SDK ruling is reaffirmed a second time —
adopting their surface adds no checkers or policy to the guest. Task 43's naming convention
holds: the driver and shim are `harmony-linux` deliverables, per-guest-world by construction.

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
3. **`StopReason::Assertion` stays machine-contract.** Fail-stop surfacing is the engine's job
   (L0); *meaning* — "a violation is a bug" — is assigned by an L3 oracle plugin. This is
   correct layering today; do not "fix" it.

## What this unblocks / changes

- **Task 84 (new)**: the fault-free exploration gate + game-shaped benchmark — also the task-70
  on-ramp (first real-hardware run of the composed spine engine).
- **Task 70 design inputs**: multi-objective archive preference (R-L2 corollary); the loop merge
  carries campaign vocabulary per the glossary.
- **Task 43 (amended)**: corpus carve-out; surface extended to flow-agent, the SDK demotion,
  and the two new harmony-linux deliverables (the `/dev/harmony` driver + the voidstar shim).
- **`sdk-link`**: one Antithesis-JSON decoder, shared by shim and native device traffic.
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
