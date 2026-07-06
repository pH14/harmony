# LAYERS — the capability-layering ruling

> **Status: RULED (Paul, 2026-07-06).** Drafted and ratified 2026-07-06 in a design session.
> Companion to `docs/GLOSSARY.md` (the naming authority) and `docs/ARCH-BOUNDARY.md` (the ISA
> seam); **amends `tasks/43-harmony-linux-tier.md`** (the payload carve-out and the SDK triplet
> below postdate its surface list). Like the glossary: binding on new code when ratified;
> physical moves ride their natural work (task 43's window, issue #74's rename window) — no
> big-bang.

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
| **cross-cutting** | the guest SDK: harmony's contract with applications, per-guest-world backends | `guest/sdk` (misplaced — see R-L3) | yes |

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

### R-L3 — The SDK triplet: interface / wire / backend

The guest SDK is harmony's contract with **applications** — code people instrument — and its
current form (`guest/sdk`: one `no_std` crate, hooks + transport, generic over
`hypercall_proto::Transport`) conflates three things with different owners:

1. **`harmony-sdk`** (app-facing, top-level): the verbs — `assert_always` / `assert_sometimes` /
   `assert_reachable` / `assert_unreachable`, state registers, lifecycle. `std`-friendly,
   eventually multi-language, with a **no-op default backend** (the Antithesis pattern: apps
   ship with asserts compiled in, inert outside a harmony world). Applications depend on this
   and nothing else.
2. **The wire convention** (interface-owned, `no_std` core): event-id namespaces and payload
   encodings — the contract every backend speaks and `dissonance/sdk-link` decodes. Today it is
   owned by `guest/sdk/src/wire.rs` with link mirroring constants (conventions rule 2); it moves
   with the interface, because it is exactly what a `harmony-freebsd` backend would also have to
   implement.
3. **Per-guest-world backends**: `harmony-linux` implements the doorbell transport (the
   `/dev/mem` mapping the flow-agent already uses); a future `harmony-<env>` implements its own.
   The bare-metal corpus payloads consume the `no_std` wire core **directly** — they are engine
   corpus, not applications (see R-L4).

This fits task 43's own naming convention — "the `harmony-` prefix is the signal: *pluggable
guest world, not core engine*" — extended one step: `harmony-sdk` is the product-surface crate,
prefix-consistent. The thin-SDK ruling carries over unchanged: the interface is identity +
observation; the host owns every interpretation; no checkers in the guest.

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
   (→ split per R-L3).

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
- **Task 43 (amended)**: corpus carve-out; surface extended to flow-agent + the SDK split.
- **Vocabulary note** (glossary addendum): the loops' history is Timeline/Multiverse →
  Modulation/Progression (task 94) → rollout/step (glossary). "Timeline" now names the
  **data-noun** (one execution history) — any surviving loop-sense use of the word is legacy.

## Non-goals

- Moving `explorer` (or any crate) out of `dissonance/` — ruled against in R-L1.
- App-declared cell functions / any fattening of the SDK — ruled against in R-L2.
- A physical restructure PR from this doc — moves ride task 43, issue #74, and each crate's
  natural work, per the ARCH-BOUNDARY precedent and the glossary's sequencing rule.
- Multi-language SDK bindings, `harmony-freebsd`, the instrument tier — named as futures the
  seams must permit, not work items.
