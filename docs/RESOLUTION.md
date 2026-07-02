# Resolution — the epoch loop of dissonance

This is the design ruling for **resolution**: the judgment layer of dissonance — the loop that
investigates what the search finds and re-instruments the search between campaigns.
`docs/DISSONANCE.md` rules the *permutation surface* (two control planes, one `Moment`-keyed
`Environment`, the two mechanical loops); `docs/EXPLORATION.md` rules the *mechanical search*
behind the Progression's seams (Sensor → Cell → Archive → Selector). This doc rules the loop
**above both**: where an agent — usually an LLM, sometimes a human acting through one — reads
campaign artifacts, interrogates moments of a deterministic execution, and restructures the
instruments before the next campaign.

> **Naming (task 94).** This doc uses the post-rename loop names: **Modulation** (inner, formerly
> Variation/Timeline) and **Progression** (outer, formerly Theme/Multiverse).

> **Design provenance.** The external evidence behind this ruling — Antithesis's multiverse
> debugging and Findings report, Pernosco, WinDbg TTD, the human-in-the-loop fuzzing literature —
> is summarized in the appendix, verified against primary sources 2026-07-01.

## Placement: part of dissonance, not a peer system

Resolution is **dissonance's epoch loop**, not a third system beside consonance and dissonance.
The criterion is the split the project already runs on: consonance owns *mechanism*, dissonance
owns *policy* — and judgment is policy, one level up. The three loops (below) are one nested
structure; Modulation and Progression are indisputably dissonance, and resolution is that
structure's outermost shell. The music agrees: resolution is not a third voice — it is the event
in the dissonance→consonance relationship, the movement toward consonance.

Glossary entry (mirrored in `docs/DISSONANCE.md` §Naming):

- **resolution** — the epoch loop of dissonance: the judgment layer (an agent; a human through
  it) that investigates findings and re-instruments the search between campaigns. Crates in
  `dissonance/` (first: `dissonance/resolution`, task 82).

**Resolution is an outer loop around the explorer, not its peer.** It configures a campaign,
launches it, consumes its artifacts, restructures the instruments, and launches the next; the
explorer never commands resolution. "Peer" survives only as a statement about *plumbing*, and
that statement is load-bearing twice:

1. **The explorer is not a gatekeeper.** When resolution investigates a moment it forks and reads
   *directly* against the control-transport server (task 58) — the same verb socket the explorer
   drives — never by tunneling through explorer code.
2. **The explorer is wrapped as a black box.** No agent callbacks thread into dissonance's
   internals. A campaign is a batch job; the archive, findings, and campaign config are on-disk
   artifacts resolution reads and edits *between* runs. The supervisory seam is
   **artifact-shaped, not RPC-shaped** — the task-58 socket stays the only live protocol in the
   system, and resolution v1 needs zero new server infrastructure on the explorer side.

## The three loops

| | **Modulation** (inner) | **Progression** (outer) | **Resolution** (epoch) |
|---|---|---|---|
| **Unit** | one decision/perturbation | one run (an `Environment`) | one campaign |
| **Cadence** | µs per decision, millions/run | ms–s per branch, thousands/hour | minutes–hours, dozens/day |
| **Policy** | seeded PRNG + overrides (the `Tactic` — open-loop) | archive weights, selector/bandit — mechanical | agent judgment |
| **Owns** | the *vocabulary* (`Action`) | the *search* over opaque `Environment`s | the *instruments*: oracles, signals, catalogs, campaign configs |
| **Adapts** | never mid-run | between runs | between campaigns |

**The self-similar invariant (load-bearing): each loop is sealed while it runs and is revised
only from the loop above, between iterations.** `docs/EXPLORATION.md` invariant 1 — the Tactic
is open-loop; all feedback-driven adaptation happens between runs — is this law one level down.
Resolution extends it one level up: **no instrumentation change lands mid-campaign.** This is
what keeps the system deterministic and analyzable while still being adaptive.

**Where the LLM sits.** The LLM is the outermost loop's revision policy — never a step inside
the sealed loops. The economics force it (an LLM call costs seconds and cents against
µs-per-decision search; the throughput thesis — *n* observable intermediate points → *n*th-root
speedup — depends on the mechanical loops staying mechanical), and the evidence agrees: the
human-in-the-loop fuzzing literature finds real-time steering interfaces essentially nonexistent,
while offline *structural* feedback (IJON-style annotations that modify the feedback function)
is the one shape that demonstrably works. The agent's leverage is **minting observable
intermediate points** — one good assertion multiplies millions of mechanical branches — not
performing search itself.

## The moment address

The universal handle every strong system converges on (Antithesis "moments", TTD `seq:step`
positions, Pernosco focus moments) falls out of the substrate for free:

```rust
struct MomentRef { env: Environment, moment: Moment }   // textual, versioned, copyable
```

The task-93 ruling keeps every reproducer **genesis-complete and portable** (corpus bases are
rebased genesis-complete; `Bug.env` is minted by `compose`), so a `MomentRef` is self-contained:
any finding, log line, transcript entry, or telemetry event stamped with one can be **re-reached
exactly** — by anyone, on any box, forever. Materializing it is the same engine mechanism as
exemplar materialization (`docs/EXPLORATION.md` §Navigation): branch from the nearest retained
ancestor snapshot (degrading to genesis — always correct, eviction is a performance knob), run
to the exact `Moment` (the deterministic force-exit machinery, tasks 47/55), and hand back a
live session. The shipped workflow this enables is the one Antithesis customers actually use:
**copy a moment from a finding → get a live fork at exactly that instant.**

Rule: **everything resolution and the findings report emit is `MomentRef`-stamped.**

## The search-surface criterion

> **An intervention is part of the search iff it is recorded as an `Environment` `Action`.**

- **Observation** — `read`, `regs`, `hash`, log/artifact retrieval: touches nothing, recorded
  nowhere, outside the search entirely.
- **Recorded moves** — `perturb`, override edits, `branch` with a mutated env: replayable from
  genesis, legitimate archive members, part of the search surface.
- **Improvisations** — see below: deliberately *neither*.

This criterion is a dial per verb, not a philosophy. The `Environment` is the ledger that
decides which side of the line any action lands on.

## Improvisations: exec is off the record

**Ruling (2026-07-02): `exec` — running a command inside a forked guest — is an improvisation:
one-off, never recorded into any `Environment`, its timeline never admitted to the archive.**
(The alternative — exec as a recorded guest-plane `Action` — was considered and rejected.)

Consequences, in order of importance:

1. **Exempt from the determinism discipline.** Because an improvisation's timeline is disposable
   by ruling, the exec channel needs none of task 61's deterministic guest-plane machinery. It
   can be as crude as injecting bytes into the fork's serial console and capturing output.
   Nondeterminism on a tainted branch is harmless by construction. This moves exec-in-fork from
   "blocked on the net vertical" to "buildable right after the read verbs" (task 81).
2. **Enforcement is structural, not conventional** (the "no bare `restore`" ethos). The first
   `exec` sets a **taint bit** on the live timeline; every snapshot derived from a tainted
   timeline inherits it; minting a reproducer (`recorded_env`) or admitting an exemplar from
   tainted lineage fails loudly (`ControlError::Tainted`). Donations must come from **pre-exec
   fork points** — the system guarantees it, not agent discipline. Otherwise one careless
   session seeds the archive with states not regenerable from `(seed, overrides)` and the
   reproducer guarantee rots invisibly.
3. **Fork-first discipline.** The original timeline is never exec'd; an improvisation begins by
   branching. (Antithesis's "undo" is the same shape: undo-by-branching.)

Improvisations generalize a concept `docs/EXPLORATION.md` already carries: **probe oracles run
on throwaway terminal branches, discarded so they never contaminate the timeline**. An
improvisation is any work on a tainted branch; a probe oracle is a mechanical improvisation, an
agent's `exec` session is a judgmental one.

## First-class instrumentation mutation: the ladder

The point of putting an LLM in the epoch loop is that it can **modify the instrumentation and
diagnostics of the problem** — this is first-class design, not an afterthought. Since exec is
off the record, live-patching diagnostics into a guest is closed off; every instrument change
flows through recorded or rebuildable channels. They form a ladder with very different
latencies, and the tiers inherit their exact semantics from `docs/EXPLORATION.md`'s replay-plane
rules:

| Tier | What the agent edits | Latency | Semantics (inherited) |
|---|---|---|---|
| **(a) replay-plane re-derivation** | matcher-DSL signals & trace **oracles** over the retained `RunTrace` store | instant — **no VM** | a new Oracle over recorded runs **finds real bugs**; a new CellFn/Sensor is *diagnostic only* (not campaign-predictive) |
| **(b) campaign config** | fault recipes/regimes, tactic portfolio & selector params, `CellFn` choice, `StopMask`, checkpoint/retention policy | next campaign | Tactic/EnvCodec-level changes cannot be evaluated offline — different inputs, must re-run |
| **(c) guest rebuild** | SDK assertions, state registers, buggify points (`harmony-linux` SDK) | new genesis — epoch-grained | the highest-leverage channel: new observable intermediate points |

Plus one deposit channel: **waypoint donation** — an *untainted* investigated fork donated to
the archive as a `VirtualExemplar` (Go-Explore's "return then explore", performed with judgment
instead of chance). Donation is admission, so it obeys the taint guard and the parent-rooted /
tail-complete contract (task 64).

The deep principle: **instrumentation changes are epoch-grained — the agent changes the
instruments between performances, never mid-performance.**

## The agent's verb surface (v1)

All against the task-58 socket; nothing here adds a second live protocol.

| Verb | Kind | Task |
|---|---|---|
| materialize(`MomentRef`) → session | navigation (replay/branch + run-to-`Moment`) | 80/82 |
| `run(until)` | navigation within a session | 58 |
| `read(gpa, len)`, `regs()` | **observation** (never recorded; hash-invariant) | 80 |
| `hash(scope)` | observation | 58 |
| `exec(cmd)` | **improvisation** (taints the timeline) | 81 |
| `perturb(fault, moment)`, branch with an edited `overrides` map | **recorded move** — replay-with-one-change is native: edit one entry in `BTreeMap<Moment, Action>` and re-materialize | 59/82 |
| donate(exemplar) | admission (untainted only) | with the Archive (post-64) |
| triage drivers: ddmin, trunk bisection + inevitability probing, LDFI counterfactuals | resolution *drives and consumes* these | `docs/EXPLORATION.md` §Triage |

**The transcript.** Every session action and result is logged, `MomentRef`-stamped (JSONL).
Because the substrate is deterministic, the investigation itself is a replayable artifact — it
can be re-rendered, scrubbed, shared, and audited after the fact (Pernosco's auto-recorded
investigation trail, generalized).

## The human layer: humans steer the LLM, not the VM

**Ruling (2026-07-02): the human's control surface is the agent, not the machine.** Human verbs
collapse to **watch, wind, and say**; all machine verbs stay agent-side. The UI writes only to
the agent's inbox, never to the control socket — so the taint and recording disciplines are
structurally unviolatable by a human, and the eighteen-year failure of steering UIs (they
demanded analysis-grade controls at machine cadence; HaCRS succeeded precisely by hiding
internals) is dissolved rather than re-fought: the LLM is the translation layer from intent to
fault vocabulary.

The input primitive is the **rehearsal mark**: a `(MomentRef, utterance)` pair pinned to the
timeline — "again, from letter C," addressed to the agent. The human winds to a point and says
what they want understood or tried; the agent compiles it into forks, reads, and recorded moves.

The visual layer is two synchronized, scrubbable timelines over one renderer:

1. **The Progression unfolding** — archive cells lighting up, branches spawning and dying,
   findings appearing; the per-run drill-down pane is the task-29 telemetry console (built,
   orphaned, strictly read-only) finally given its home one level below the front page.
2. **The investigation unfolding** — the agent transcript, every entry moment-stamped and
   clickable.

Task 29's design principle — *one renderer, keyed on V-time, so live and replay are identical* —
generalizes: **the UI is a pure function of (campaign artifacts, transcript, cursor).** Build
the harness on the agent SDK and ship the REPL/API first; a bespoke reactive notebook UI is
what a funded company ships as its *advanced* mode, and the adoption evidence (below) says
familiar surfaces win. The UI is sequenced last.

## Sequencing

Task numbers 65–75 are reserved by the Wave-5 exploration queue (task 64); resolution takes
**80–83**. Dependencies are on task 58 (the server) — not on Wave 5 — except where noted.

| # | Task | Depends on | Delivers |
|---|---|---|---|
| 80 | inspection verbs (`read`/`regs`) + materialize-at-`Moment` gate | 58 | observation surface; the moment address proven live |
| 81 | improvisations: `exec`-in-fork + lineage taint | 58 (61 explicitly **not** required) | the interrogation verb; structural taint guard |
| 82 | `dissonance/resolution`: session client + REPL + transcript | 80, 81 | the agent-facing surface; `MomentRef` as a copyable artifact |
| 83 | findings: the run-over-run behavioral diff | 60, 64 | the push surface: New/Resolved/Ongoing/Rare, `MomentRef`-stamped calls to action |

Deferred to later handoffs (deliberately unspecced now): the MCP/agent harness over the
resolution crate; the rehearsal-mark inbox + the two-timeline viewer; `donate` (needs the
Archive, task 64+); triage-driver integration (ddmin/bisection/LDFI as resolution commands);
any notebook UI.

## Appendix: the evidence (verified 2026-07-01)

What shipped and worked in the closest existing systems; each claim verified against primary
sources, adversarially (3-vote), during the 2026-07-01 research pass.

- **Antithesis** — the closest product. Advanced surface: a browser-based reactive notebook
  connected to the deterministic hypervisor; verbs: exec-in-sim, undo (via branching), rewind /
  fast-forward, unlimited branch. Moment model: immutable `(timeline, time)`. The shipped
  customer workflow since Sept 2024: **copy a moment from the logs → shell in a reproduction at
  exactly that instant** ("want a network dump 1ms before your crash?"). Push surface: the
  **Findings report — a diff of the software's behavior from one run to the next**
  (New/Resolved/Rare/Ongoing, "calls to action"), not a raw crash list — and docs call it the
  most important piece, ahead of the notebook. (antithesis.com/docs: multiverse_debugging,
  reports/findings; blog: multiverse_debugging, notebook_interfaces.)
- **Pernosco** — the omniscient-debugger high bar: precomputed all-states database, bulk search
  instead of breakpoints, one-click backward dataflow, a notebook that auto-records every focus
  change as a clickable moment bookmark. **Adoption warning:** by its author's own account it
  was only "modestly successful" — customers loved it; few adopted. (pernos.co/about;
  robert.ocallahan.org 2024.)
- **WinDbg TTD** — the whole trace as LINQ-queryable collections (`TTD.Events`, `TTD.Memory`)
  with `seq:step` positions and `SeekTo()` — query-not-step as a mode. Harmony needs no trace
  database for this: **deterministic re-execution is a lazy query engine** (the
  OmniTable/SteamDrill result) — a "query" is a replay with an observer.
- **Undo (UDB)** — no bespoke UI ever shipped; a 100% GDB-compatible CLI plus IDE plugins; 2026
  repositioning toward MCP/agents. The commercial bet: piggyback on surfaces users already have.
- **HITL fuzzing** (2026 systematic review, 44 works 2007–2025) — small and shrinking;
  monitoring concentrates mid-campaign but steering concentrates in *offline* preparation, with
  "virtually no real-time HCI interfaces for steering during fuzzing execution." **IJON**
  (S&P'20) is the canonical working feedback channel: source annotations that modify the
  fuzzer's feedback function. **HaCRS** (CCS'17): steering interfaces for non-experts work by
  hiding program-analysis concepts — terminal, suggestions, goals, nothing else.

The synthesis this doc rests on: moment-addressable execution as the universal handle; a small
verb set over it (jump, fork, exec-in-fork, rewind/undo-by-branching, whole-execution queries);
push = cross-run behavioral diff, pull = copy-a-moment-and-fork; feedback into the explorer is
offline and structural; and the investigating user is increasingly an agent.
