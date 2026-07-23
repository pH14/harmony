# CLI — the `harmony` command

> **Status: DRAFT — NOT RATIFIED (2026-07-13, pondering session).** This document develops
> the CLI/plugin thinking recorded in `docs/NESTED-INTEGRATION.md` §3.4. That section's two
> safety rules and the `harmony-oci`-first ordering were **ruled by Paul 2026-07-10** and are
> treated as standing here; the surrounding product sketch remains parked and this document
> does not un-park it. `docs/GLOSSARY.md` stays the naming authority: the verbs below are a
> slate for ruling, and any that ratify must be added there in the same PR. Both consonance
> and dissonance surfaces are moving targets right now — so this document pins the **grammar
> and contracts** hard and keeps the **verb slate** deliberately soft.

## 1. What the CLI is

One binary, `harmony`, that is the product surface of the whole family. It has three jobs:

1. **Acquire a deterministic machine on the current host.** On the box (the TODAY column of
   NESTED-INTEGRATION §1) that means composing the VMM in-process, the way conductor's
   `boxrun` and the live-gate test binaries do today. On anyone else's metal or cloud VM it
   means preflight-qualify the host, boot the appliance, and speak the control protocol over
   vsock (the NESTED column — gated on `hm-l2g`/`hm-tn9`). The user types the same verb
   either way; substrate acquisition is the CLI's problem, not the user's.
2. **Own the artifact lifecycle.** Every run mints a Reproducer and a run report; replay,
   film, events, and resolve consume Reproducers regardless of what produced them. The CLI
   is where the identity *state = replay(reproducer)* becomes user experience.
3. **Dispatch to guest-world plugins.** `harmony oci run <container>` is `harmony-oci`
   translating a familiar input into a subject bundle and the core running it. Plugins add
   frontends, never new artifact types.

The CLI is a **client-first** design: everything above `unison::Machine` already works
against the task-58 control seam, so the CLI is one more client of `ControlServer` — the
same binary drives an in-process machine on the box, a nested appliance, or (later) a remote
class-matched replay host. The wire already speaks the needed verbs
(`Snapshot/Drop/Branch/Replay/Run/Hash/Perturb/Read/Regs/Exec` plus paged
`SdkEvents`/`Console` — `control-proto`'s `Request` enum); **v0 adds no new wire
vocabulary.** It does **not** require a resident daemon: `harmonyd` (`hm-9od`) stays
deferred; v0 composes in-process exactly so the CLI can exist before the daemon does.
(When a daemon is eventually justified, the CLI is the live consumer that un-defers it.)

## 2. The grammar

```text
harmony <verb> …            global artifact verbs — interface-agnostic
harmony <world> <verb…>     namespaced frontends — one namespace per guest-world plugin
```

Two standing rules (ruled 2026-07-10, NESTED-INTEGRATION §3.4):

1. **Plugins translate and stage; only the core seals and runs.** A plugin consumes familiar
   input (image ref, compose.yml, manifests) and produces a canonical **subject bundle**. The
   core owns preflight/qualification, machine acquisition, seed and Reproducer lifecycle,
   replay, the event index, and resolution. The determinism contract is enforced in exactly
   one place: a plugin *cannot* introduce a live mount or an unpinned tag because it never
   touches the machine — it only emits a bundle the core refuses if unsealed.
2. **Frontends are namespaced; artifact verbs are global.** `harmony oci test` but
   `harmony replay/film/events/resolve/preflight` stay top-level, because a Reproducer is
   interface-agnostic. The run report records which plugin (and version) built the bundle as
   *provenance*, but **replay must never require the plugin** — the Reproducer plus stored
   artifacts alone reproduce, or plugin churn would erode reproducibility.

Register discipline (GLOSSARY rule 1) applies with a twist: the CLI is the first genuine
**product surface**, which is exactly the layer where the family register is allowed to
live. So the surface vocabulary is family words (*unison*, *film*, *resolve*) plus the ruled
artifact/axis nouns (*reproducer*, *moment*, *timeline*, *campaign*) plus plain verbs (*run*,
*replay*, *events*, *preflight*). Fenced words stay fenced: **sweep** (gate-only) and
**session** (transport) never appear in porcelain. Mechanism vocabulary (`SnapId`, seal
mechanics, tactics, cells) stays out of the product surface entirely.

## 3. The two currencies, as UX

The GLOSSARY's rule 3 (name the currencies) becomes the CLI's artifact model:

- **Reproducer** — cheap, portable, the thing you attach to a bug report or upload from CI.
  Invariant: **no run without a Reproducer.** Every run-shaped verb, core or plugin, ends in
  the one core run pipeline that emits `(reproducer, run report)`. For a natural run the
  Reproducer is coordinates, not recordings: subject pins + seed + host class + contract
  version (NESTED-INTEGRATION §3.1.7).
- **Captured state** (snapshots, seals) — expensive, machine-local, a resource. Porcelain
  hides pool mechanics; what a user does with state is **materialize** it from a Reproducer
  at a Moment (`docs/DISSONANCE-STRATEGY.md` already fixes *materialize* as the
  replay-to-captured-state operation, distinct from *recompute cells*). "Restore" as a
  user word is deliberately avoided: restore names the engine's snapshot-resource operation,
  and the portable thing a user holds is never a snapshot.

Replay honesty rides every relevant verb (§3.1 of the parked sketch, kept verbatim): a
Reproducer replays **bit-identically within its certified CPU class**; elsewhere replay is
attempt-and-verify and divergence is loud. The CLI owns the class-matching UX: same class →
local; otherwise it says so, and (later) offers a class-matched remote machine while film/
resolve clients stay local.

Artifact honesty is already enforced below the CLI, not invented by it: on the control wire,
an improvisation (`Exec`) **taints** the timeline and the server refuses to mint a
Reproducer from it (`RecordedEnv` → `Tainted`). `harmony resolve` inherits that discipline
for free — you can poke a materialized state interactively, but you cannot pass the poked
timeline off as reproducible.

## 4. Core verb slate — consonance (draft, for ruling)

| Verb | What | Notes |
|---|---|---|
| `harmony run <bundle>` | execute a staged subject bundle deterministically; emit Reproducer + run report | `--seed` (freeze) / `--seeds N` (surface mode); the escape hatch for hand-built worlds — plugins are sugar over this |
| `harmony replay <reproducer>` | re-execute; verify trajectory pins; `--to <moment>` stops on the axis | naming question vs the sketch's `repro` — see §8 |
| `harmony materialize <reproducer> --at <moment>` | replay to a Moment and hold live captured state, ready to attach | the ruled restore-word; the two-noun identity made tangible |
| `harmony events <reproducer>` | list the event index — `(event, Moment)` pairs from logs/spans | O-track (parked sketch §5); serial path needs zero guest change |
| `harmony resolve <reproducer> --at <moment\|event>` | boot class-matched replay, `run_to` the Moment, attach the resolution client | fronts the existing `dissonance/resolution` REPL/session |
| `harmony film <reproducer> --span <a..b>` | render what the screen showed | ruled word; task-87 machinery |
| `harmony unison <bundle>` | determinism self-certification: run twice (or N×), compare `state_hash` | the family word doing product work; relation to `acceptance-suite` is an open question (§8) |
| `harmony preflight` | host qualification probe: GO or fail-closed machine-readable refusal | bead `hm-69y` (renamed from "doctor"; Paul wants it composable — launcher invokes it, users can too) |
| `harmony plugin ls` | list discovered plugins + versions + bundle schema versions | `harmony --help` also lists them (§3.4 mechanics) |

Cross-cutting conventions:

- **`--json` everywhere.** The preflight refusal report, run report, unison verdict, and
  events listing are machine-readable first (the acceptance-suite exit-code + JSON-report
  contract is the precedent); human rendering is a view over the same data.
- **Fail closed, name the missing thing.** An unqualified host, an unpinned tag, an unsealed
  bundle each produce a refusal that says which contract line failed — never silent
  degradation (§3.1 of the parked sketch, made a CLI-wide rule).
- **Snapshot-pool verbs are plumbing.** If exposed at all, under a debug namespace
  (`harmony debug snapshots …`), not porcelain — a user manages Reproducers, not pools.

## 5. The plugin architecture

### Discovery and handshake

The `git`/`cargo`/`docker` convention, as ruled: an executable `harmony-<name>` on PATH or in
`~/.harmony/plugins/` is discovered and mounted as `harmony <name>`. Beyond discovery, one
reserved subcommand (the docker-cli-plugin-metadata pattern):

```text
harmony-<name> manifest   →  { name, version, core_min, bundle_schema, verbs: […] }
```

The core pins compatibility at dispatch — hello-time rejection, exactly the control-proto
caps pattern. A plugin that emits bundle schema vN against a core that speaks vM ≠ N is
refused with the version pair in the message, not run in hope.

### Two verb classes inside a plugin

1. **Translator verbs** (`run`, `test`, `stage`): the plugin is invoked as a pure translator
   — it stages input into a subject bundle in a core-provided directory, emits a run request
   (bundle path + knobs), and exits. **The core then seals, runs, and owns the console/UX.**
   The plugin never holds the machine, never sees the control socket, cannot outlive the
   staging step. This is rule 1 enforced by process shape, not by review.
2. **Passthrough verbs** (`inspect`, help text, listing): plain exec passthrough, no machine
   access, no bundle. Environment contract: `HARMONY_CORE_VERSION`, `HARMONY_HOME`,
   `HARMONY_OUTPUT=json|human`.

First-party plugins go through the same public seam — if `harmony-oci` needs a private hook,
the seam is wrong (ruled, §3.4).

### The subject bundle is the seam

The bundle is the R1 output schema of the parked runner track: **staged rootfs + manifest +
content pins + epoch definition** (epoch start = `setup_complete`, the SDK's existing
SnapshotPoint). Split of identities:

- the **bundle** is workload identity (what runs) — content-hashed, tags refused;
- the **seed** is run identity (which timeline) — core-owned, never in the bundle;
- the **Reproducer** binds both plus host class + contract version.

The bundle schema is versioned from day one; it is the compatibility spine that lets the
plugin ecosystem and the core evolve at different speeds — which is the moving-target hedge
this whole document leans on.

### Layering mirrors the guest worlds

The plugin namespace is the CLI-side twin of the `harmony-<env>` environment tiers
(`docs/DISSONANCE.md`): `oci` (single image, base tier) → `docker-compose` (multi-container
graph over oci) → `kubernetes` (manifests → the deterministic k3s asset). Built in that
order; each is a translator over the previous. A plugin's namespace name should be the
guest-world name — the user-visible catalog of plugins *is* the catalog of worlds harmony
can run deterministically.

## 6. `harmony-oci` — the first plugin (sketch)

Ruled 2026-07-10: running OCI images is the primitive; `harmony-oci` is the first guest-level
interface and ships with the CLI.

```text
harmony oci run  <image-ref> [--cmd …]                  # dev loop: boot + run, console streamed
harmony oci test --image <ref@sha256:…> --cmd "pytest -x" \
                 --seeds 8 --reproducers out/           # CI shape (parked sketch §3.2)
harmony oci stage <image-ref>                           # translate only; print the bundle
```

`run` is the demo verb — *a fully deterministic run of a container on the current machine* —
and `test` is the CI wedge (surface mode: fresh seeds shake races out with zero fault
injection; failures become permanent Reproducers). Both are the same translator path: image
ref (digest-pinned; tags refused) → stage-and-seal → subject bundle → core `run`. Services
the workload needs run *inside* the subject; live host I/O inside the epoch is structurally
impossible.

## 7. Dissonance surface — deliberately thin, deliberately tentative

We know less here, and the strategy is explicitly staged (`docs/DISSONANCE-STRATEGY.md`), so
the slate mints almost nothing:

- `harmony campaign run <config>` / `harmony campaign report <id>` — *campaign* is ruled
  vocabulary with a pinned definition (pure function of `(campaign_seed, machine)`), and
  `CampaignConfig`/`CampaignReport` are the ruled integration primitives. The config format
  itself is unspecified (strategy gap 8, stage 5) — the CLI shape can exist before the
  format does, because the verb takes an opaque artifact.
- **Findings are Reproducers.** The dissonance UX dividend is that the *global* artifact
  verbs do the product work: a campaign finding is addressed by `(timeline, Moment)`, so
  `harmony replay / film / events / resolve` operate on findings with zero new vocabulary.
  This is the one-reproducer invariant (LAYERS constraint 1) showing up as UX.
- **Not minted:** no verbs for tactics, selectors, cells, archives, or portfolios (mechanism
  layer); no `fuzz` verb (wrong register and wrong claim); nothing for Resolution's
  between-campaign judgment loop until that loop exists (strategy stages 6–8) — when it
  does, it is artifact-shaped by ruling, so it will slot in as verbs over
  `CampaignReport`/`CampaignConfig` rather than a live control surface.

## 8. Open questions — the slate for ruling

1. **`replay` vs `repro`.** The parked sketch used `harmony repro <file>`; the glossary's
   abbreviation discipline (the `logtmpl`/`det-` kill class) argues for **`replay`**, with
   the artifact spelled *reproducer*. Recommend `replay`.
2. **Is `unison` a product verb?** Determinism self-certification (`run twice, compare`) is
   the family word doing exactly its ruled meaning. If yes: does the acceptance-suite runner
   front it (`harmony unison --suite acceptance`) or stay an internal gate? Recommend yes +
   internal-gate-stays-internal.
3. **`materialize` in porcelain?** It is the ruled word and the precise operation, but it is
   a mouthful. Recommend keeping it — teaching the word teaches the two-currency model.
4. **`preflight` final name.** `hm-69y` carries it as a working name (plain-descriptive,
   GLOSSARY rule 1 compliant). Decide at ratification.
5. **Crate name and home.** The binary is `harmony`; the crate should take a boring role
   name (`cli`) per GLOSSARY rule 1. It is family-level (spans consonance and dissonance),
   so it lives at top level — not under either family dir. **Counterpoint stays unspent**:
   an argument-dispatch veneer is not the "genuine family/product-level role" the reserve
   clause demands.
6. **Where does the bundle schema live?** It is the plugin seam *and* the R1 stager output.
   Candidates: a small top-level `bundle` crate (schema + validation only), or
   consonance-side next to the machine-acquisition code. It must not live inside any plugin.
7. **`~/.harmony` layout.** Plugin dir, content-addressed Reproducer/artifact store,
   qualification cache + class certificates, corpus dirs for regression re-runs. Naming and
   shape unruled; "workspace" deliberately avoided until needed.
8. **How much of the parked I-track does v1 pull in?** `harmony oci run` on the box needs
   none of it; on anyone else's machine it needs appliance build (`hm-tn9`) + preflight
   (`hm-69y`) + the vsock transport (I2). The CLI can ratify and ship box-first without
   un-parking the product sketch — but the grammar must not assume box-only.

## 9. Sequencing (proposed, not scheduled)

### What exists today — the veneer inventory

There is no `harmony` binary yet (the namespace is clean). Eight host CLIs already exist,
each a clap-derive bin gated behind a `cli` feature so the libraries stay clap-free — a
house convention the `harmony` crate keeps. Today's invocation reality is
`cargo run -p <crate> -- …` plus box-pinned `cargo test --test live_*` gates; the v0 CLI is
a veneer over these composition roots, not new machinery:

| Existing surface | Today | Proposed CLI home |
|---|---|---|
| `conductor` `campaign`/`bench-campaign`/`game` modes | `taskset -c N cargo run -p conductor --release -- game box …` | `harmony campaign …` (§7; rides the `campaign-runner` rename) |
| `conductor materialize` | conductor subcommand | `harmony materialize` — the verb already exists, it just lives in the wrong binary |
| `film` `plan`/`render`/`demo` | 3-step pipeline via `cargo run -p film` | `harmony film` (collapse plan→render behind one verb; keep the stages as flags) |
| `resolution` REPL (`open regs read hash run exec vary transcript`) | `cargo run -p resolution -- --seed … --record t.jsonl` | `harmony resolve` attaches it |
| `det-corpus` `run`/`validate` (JSON report + exit-code contract) | `cargo run -p det-corpus -- run --manifest …` | internal gate; optionally fronted by `harmony unison --suite` (rides the `acceptance-suite` rename) |
| `unison` bin (`toy-compare`/`toy-bisect`) | mechanism demo | stays a mechanism-layer tool; no product exposure (the *verb* `harmony unison` is new work over real machines) |
| `telemetry` `console` (SSE web console, live/file replay) | `cargo run -p telemetry --bin console -- --source file:run.ndjson` | `harmony debug console` for now; possible later promotion |
| `benchmark-report` / `exploration-report` | offline renderers | stay task-report plumbing |

The deterministic-boot and snapshot save/restore paths currently live only inside the
`live_*.rs` box-gate tests — the v0 `run`/`replay` verbs are the first non-test composition
of those paths, which is most of v0's real work.

1. **v0 — the binary and the grammar.** `harmony` with dispatch, `preflight`, `unison`,
   `replay`, `film`, `events`, `resolve` — each a veneer over the inventory above,
   in-process, box-first. No plugins yet, but the dispatch convention and bundle schema are
   versioned from day one. Value: the vocabulary starts being real; every later surface
   change has a place to land.
2. **v1 — `harmony-oci`.** The stage-and-seal builder (R1) + translator protocol +
   `oci run`/`oci test`. Box-first; nested via the I-track when its beads land.
3. **Later.** vsock/appliance transport, remote class-matched replay, the GH Action
   (`harmony/setup@v1`), `docker-compose` and `kubernetes` plugins in layer order,
   campaign verbs when the config format exists.

No beads are minted by this draft; if it ratifies, v0/v1 become beads with the usual
dependency edges (v1 depends on `hm-l2g`→`hm-tn9`/`hm-69y` only for the nested path).
