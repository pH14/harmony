# NESTED-INTEGRATION — from spike GO to product surface

Status: **PARKED PRODUCT SKETCH — NOT RATIFIED** (Paul, 2026-07-12). Paul reviewed this and
ruled it product strategy ahead of a product decision: the ratified goal is instead the
**reach matrix** (vendors Intel/AMD/ARM × forms bare-metal/virtualized — see
`docs/ROADMAP.md`). The portability-infrastructure items were extracted to beads (appliance
build `hm-tn9`, host-qualification preflight CLI `hm-69y` — renamed from "doctor"; harmonyd
`hm-9od` DEFERRED until a live consumer exists). The runner (R-track), event index
(O-track), plugin architecture, and cloud qualification sweep are retained here as design
record only — resurrect if/when a product is chosen. Terminology note: "personality" was
ruled → **"vendor"** (2026-07-12); this parked text predates the rename.
Downstream of the nested-x86 spike (`docs/NESTED-X86.md`; note its dispositions are under
re-certification per the PR #98 evidence-integrity review, 2026-07-12). Companion programs: `docs/APPLE-SILICON.md` (ARM), `docs/ARCH-BOUNDARY.md`
(ISA seam), `docs/LAYERS.md` (capability layering), `docs/GLOSSARY.md` (naming authority).

The spike proved the stack runs unmodified as an L1 guest with bit-identical determinism
(nested == metal, one `state_hash` across a hostile-host condition matrix; workloads
1.01–1.08× vs metal). This doc is the plan for making that a *supported deployment mode*
with first-class UX, rather than spike scripts.

## 1. The three deployment stacks

Rows mean the same thing in every column: bottom = physical machine; the row above it =
the one layer holding real VMX root mode; everything higher is a guest. Only the
*occupant* of each row changes.

```text
             TODAY                    NESTED (spiked)            OCTAVE (reserved)
             the box, shipping        the appliance              consonance under test

L2 guest     ┌──────────────────┐     ┌──────────────────┐       ┌──────────────────┐
             │   (open slot —   │     │     subject      │       │  inner's subject │
             │  inherited nVMX) │     │ postgres, k3s, … │       │   replays N/N    │
             └──────────────────┘     └──────────────────┘       └──────────────────┘
L1 guest     ┌──────────────────┐     ┌──────────────────┐       ┌──────────────────┐
             │     subject      │     │    CONSONANCE    │       │    CONSONANCE    │
             │ postgres, corpus │     │  (the appliance) │       │ system under test│
             └──────────────────┘     └──────────────────┘       └──────────────────┘
VMX root     ┌──────────────────┐     ┌──────────────────┐       ┌──────────────────┐
             │    CONSONANCE    │     │    stock KVM     │       │    CONSONANCE    │
             │ patched KVM, vmm │     │ unmodified host  │       │    the tester    │
             └──────────────────┘     └──────────────────┘       └──────────────────┘
hardware     ┌──────────────────┐     ┌──────────────────┐       ┌──────────────────┐
             │     our box      │     │  anyone's metal  │       │     our box      │
             │ bare metal, owned│     │  cloud / laptop  │       │   metal we own   │
             └──────────────────┘     └──────────────────┘       └──────────────────┘
```

Reading notes:
- **Rings are orthogonal.** Every box above contains a full ring 0–3 hierarchy; the
  vertical axis is VMX root/non-root nesting (flattened — guest instructions at any
  depth execute natively; only virtualization control traps downward).
- **TODAY → NESTED** moves the determinism machinery up one row; the root-mode row
  becomes commodity software and the hardware row becomes someone else's problem. The
  host contract shrinks to *expose VMX + an exact vPMU* — probeable at boot (N-0).
- **NESTED == TODAY for the artifact.** N-3 measured depth-invariance: same seed, same
  `state_hash`, L1 or L2, on the same chip. Cross-*chip* identity is the separate
  CPU-class question (§4.3).
- **OCTAVE** (name reserved 2026-07-10) = consonance testing consonance; outer-on-metal
  keeps it at exactly the depth the spike proved. Not scheduled; recorded so the seam
  choices below don't foreclose it.

## 2. Integration architecture: the appliance is a transport, not a layer

The spike drove gates via kernel cmdline + serial because it was forbidden production
changes. Integration inverts that: **the appliance boots `ControlServer` and the VM
boundary becomes one more byte stream under the existing control protocol.** Everything
above `unison::Machine` — explorer, counterpoint, film, resolution, runtrace, snapshots,
host-fault scheduling — works against a nested machine unchanged. That is the dividend
of the task-58 seam and the reason this program is mostly packaging, not core work.

Work items (I-stages), each with a gate:

| # | Item | Content | Gate |
|---|------|---------|------|
| I1 | `harmonyd` composition root | A real binary (vmm-core) that reads an appliance manifest (subject image + pins, RAM, factory config), builds `VmmFactory`/`SnapshotEngine`, serves control-proto. Today that composition exists only inside test binaries and conductor `boxrun`. | boots in the appliance; serves `hello` |
| I2 | vsock transport binding | control-proto listener on `AF_VSOCK` in harmonyd; vsock dialer in the explorer adapter (QEMU `-device vhost-vsock-pci`). Serial remains for boot logs + gate verdicts. Protocol unchanged (caps flag at most). | existing socket-Machine test suite green over vsock into a live appliance |
| I3 | appliance as first-class build | Promote `spikes/nested-x86/appliance/{build,init,run}` into `guest/appliance/` Makefile targets: pinned kernel, patched kvm/kvm-intel modules, harmonyd, subject images, init. Keep the spike init discipline verbatim (patched-module load check, in-L1 content-hash verification of L2 images before any boot). | one-command build reproduces the N-5 demo from the repo |
| I4 | launcher + `doctor` | Host-side CLI owns the pinned QEMU invocation (the spike's run-appliance.sh becomes code): vCPU pinning policy (mandatory core-type pin on hybrid CPUs, §4.2), vsock wiring, lifecycle. `doctor` = N-0 truth table productized + in-guest self-check, machine-readable report incl. CPU-class identification. | doctor GO on the box + on one qualified cloud class; refusal with a machine-readable report on an unqualified host |
| I5 | conductor `ApplianceRun` | Peer to `boxrun`: acquire machine by booting the appliance instead of composing in-process. Campaign/film/resolution code unchanged. | one existing campaign runs end-to-end nested via I1–I4 |
| I6 | gate mode kept | The spike's cmdline+serial gate path (`harmony.gates=… → GATE_RC`) survives as the dumb, robust CI path beside the interactive vsock path. | CI job template using gate mode |

Explicitly unchanged: patches 0001–0005, `Backend`/`Vmm`/`Machine`, control-proto framing,
snapshot machinery, guest SDK (`/dev/harmony` doorbell — already exercised nested in N-1).

## 3. Deterministic CI runner (consonance-only, no dissonance)

The wedge use case: run an **existing test suite** inside consonance. No faults, no
exploration. Natural failures become permanent reproducers. Dissonance is the upsell.

### 3.1 What exactly "pure determinism in a run" requires

A CI run is deterministic iff all of the following hold. This list is the contract; the
runner enforces every line and refuses (with a report saying which line failed) rather than degrade silently.

1. **Qualified host class.** The host passed `doctor` *and* the class has passed the
   hammer-under-load qualification (§4.1): VMX exposed; vPMU exact under descheduling,
   co-tenant steal, and cross-core migration (N-2 conditions — pinning is hygiene on
   qualified homogeneous hosts, **mandatory core-type pinning on hybrid P/E hosts**).
2. **Pinned artifacts, by content hash.** Appliance image sha256 + subject image sha256.
   Tags (`:latest`) are refused. The subject image is *built*, not mounted (see 3).
3. **A sealed boundary.** No live host I/O inside the deterministic epoch: no virtiofs,
   no passthrough network, no host-clock reads (trapped anyway), no host volumes. The
   OCI workload is **staged**: image flattened into the subject rootfs at build time.
   Services the tests need run *inside* the subject (compose graph / deterministic k3s).
   Live third-party endpoints are out of scope by construction.
4. **One seed.** A fresh seed per run in *surface* mode (flake hunting); a pinned seed in
   *freeze* mode (exact re-run). The seed drives the entropy stream and the work-quantum
   schedule — that schedule variation is what shakes races out without any fault
   injection.
5. **Epoch start = `setup_complete`.** Unpack/boot/service-start happens pre-epoch; the
   SDK's existing `setup_complete → SnapshotPoint` seals the epoch start (boot itself is
   deterministic too; the snapshot point gives a stable fork/replay origin).
6. **Single deterministic vCPU** (today). Suite parallelism is serialized into the
   seeded schedule. SMP determinism is a separate future chapter.
7. **A Reproducer + run report emitted always.** The Reproducer (`docs/GLOSSARY.md`
   ratified term: the replayable artifact) is `(appliance sha, subject sha, seed, host
   class, contract version det-*-vN)` — for a natural run, coordinates, not recordings.
   The run report is plain outcome metadata alongside it: exit code, terminal
   `state_hash` + `observable_digest`.

Replay honesty (belongs in every user-facing sentence): a Reproducer replays **bit-identically
within its certified CPU class**; replay elsewhere is attempt-and-verify (divergence is
loud — the trajectory pins catch it), and the class-matched machine can be remote while
film/resolution clients run on the developer's machine (task-58 client/server split).

### 3.2 The pipeline, concretely (GitHub-Actions-shaped)

```yaml
jobs:
  test:
    runs-on: [self-hosted, harmony-qualified]   # or a qualified cloud class (§4)
    steps:
      - uses: docker/build-push-action@…        # normal image build, push by digest
      - uses: harmony/setup@v1                  # installs CLI; doctor gate (fail-fast + report)
      - run: harmony oci test --image ghcr.io/acme/app@sha256:… \
                              --cmd "pytest -x" --seeds 8 --reproducers out/
      - uses: actions/upload-artifact@…         # reproducers; PR comment: `harmony repro <file>`
        if: failure()
```

Developer loop on failure: `harmony repro out/seed-1734.reproducer` (same class → local;
otherwise boots a class-matched cloud appliance and attaches locally), `--film` to
record, `harmony resolve` to time-travel. Fixed bugs' Reproducers accrete into a regression
corpus directory; nightly re-runs the corpus (re-exploration seeds across commits — a
Reproducer replays exactly only against its recorded artifacts).

### 3.3 Runner-track work items (R-stages)

| # | Item | Gate |
|---|------|------|
| R1 | stage-and-seal builder: OCI image → flattened subject rootfs (+ optional compose/k3s graph), content-hashed | same image digest twice → byte-identical subject image |
| R2 | in-subject agent: run command, stream junit/exit via report stream (already in `observable_digest`) | pytest suite exits with correct code + junit surfaced |
| R3 | Reproducer + run-report emission; `harmony oci test` / global `harmony repro` verbs | failure at seed S reproduces N/N same-class from the Reproducer alone |
| R4 | GH Action wrapper + docs | the yaml above works in a public repo |
| R5 | (later) shim-v2 / RuntimeClass `harmony` for k8s CI fleets; local `docker run --runtime harmony` | existing k8s job opts in with one line |

### 3.4 Guest-level interfaces: `harmony-oci` first, plugin architecture (ruled 2026-07-10)

Paul's ruling: **running OCI images is the primitive; `harmony-oci` is the first
guest-level interface**, exposed as `harmony oci <subcommand…>`, with a plugin
architecture so `harmony-docker-compose` / `harmony-kubernetes` can be added later and
appear as `harmony docker-compose …` / `harmony kubernetes …`.

This is the CLI-side twin of the existing **environment-tier layering** in
`docs/DISSONANCE.md` (`harmony-<env>` tiers; `harmony-kubernetes` declares
`harmony-linux` as its base). A frontend plugin translates a familiar input format into
a subject targeting a tier; the tiers were already designed to layer, so the frontends
layer the same way: `oci` (single image, base tier) → `docker-compose` (multi-container
graph over oci) → `kubernetes` (manifests → the deterministic k3s asset, task 56).
Build them in that order — each is a translator over the previous.

Two rules make the plugin architecture safe rather than fragmenting:

1. **Plugins translate and stage; only the core seals and runs.** The plugin contract is:
   consume familiar input (image ref, compose.yml, manifests) → produce a canonical
   **subject bundle** (staged rootfs + manifest + content pins + epoch definition — the
   R1 output schema). The core owns doctor/qualification, appliance boot, seed/Reproducer
   lifecycle, replay, the event index, resolution. The §3.1 determinism contract is enforced in
   exactly one place; a plugin *cannot* introduce a live mount or an unpinned tag,
   because it never touches the machine — it only emits a bundle the core refuses if
   unsealed.
2. **Frontends are namespaced; artifact verbs are global.** `harmony oci test`,
   `harmony docker-compose up` — but `harmony repro/resolve/events/doctor` stay
   top-level, because a Reproducer is interface-agnostic. The run report records which
   plugin (and version) built the bundle as *provenance*, but **replay must never require
   the plugin** — the Reproducer + stored artifacts alone reproduce, or plugin churn
   would erode reproducibility.

Mechanics: the `git`/`docker`/`cargo` convention — an executable `harmony-<name>` on
PATH (or `~/.harmony/plugins/`) is discovered and mounted as `harmony <name>`;
`harmony --help` lists discovered plugins. Plugins declare a minimum core version and
the subject-bundle schema version they emit; the core pins compatibility at dispatch
(hello-time rejection, the control-proto caps pattern). First-party plugins
(`harmony-oci` ships with the CLI) still go through the public seam — if our own
frontend needs a private hook, the seam is wrong.

## 4. CI testbed: what actually needs metal now

Depth rule from the spike: the appliance needs **one VMX-root layer with an exact vPMU
directly beneath it**. A cloud VM *hosting the appliance inside it* puts the subject at
L3 (beyond proven depth). The supported cheap topology is **the appliance as the cloud
VM** (custom image / qualified instance class) — exactly the spike's depth with the
provider's hypervisor in stock-KVM's seat.

### 4.1 Qualification suite (per provider × instance class)
- `doctor` truth table (N-0 productized), plus
- **the N-2 hammer under load**: samecore-steal + forced-migration conditions, **plus an
  explicit SMT-sibling condition** (stress pinned to the vCPU core's hyperthread twin).
  Note the spike's entire evidence base was already collected with SMT on (i9-9900K,
  2 threads/core) — sibling interference was ambient, not controlled; this makes it
  controlled. On metal, Linux perf's per-task counter save/restore is upstream code
  (spike: 1,509 unpinned migrations, zero mismatches; 150k same-core co-tenant
  deadlines, exact). In a cloud VM that guarantee is re-implemented by the **provider's**
  vPMU across *their* scheduling — unknown until hammered. Candidates, in probe order: Hetzner Cloud CCX (dedicated-core),
  GCE with `min_cpu_platform` pinned, OCI x86 BM (ephemeral-window reuse), GH-hosted
  runners (KVM present; vPMU expected absent — cheap to confirm).
- Output: a published qualification matrix; `runs-on: harmony-qualified` maps to it.

### 4.2 Standing policies
- **Hybrid CPUs (Alder-Lake+ clients): core-type pinning is a correctness requirement**
  (`cpu_core` vs `cpu_atom` PMUs differ; the branch event exact on P-cores does not
  count on E-cores — rr's experience, reproduced logic). `doctor` detects heterogeneity;
  the launcher pins or refuses.
- **Event choice stands**: retired conditional branches (exact; N-0 measured `n+2` 60/60
  while the instructions-retired control jittered ±1 — independently reproducing rr's
  folklore).
- Keep **one owned box**: bare-metal reference for class certification (the metal side of
  nested==metal), perf baselines, kernel iteration, octave's outer layer later. Scaling
  test capacity stops meaning procurement.
- Bonus: any Linux runner with /dev/kvm (even vPMU-less) can boot the appliance for
  build/clippy/portable gates — closing the cfg(linux) CI blind spot per-PR.

### 4.3 CPU classes (cross-chip identity)
A class = pinned virtual CPU surface + host floor + counter contract version
(`det-cfl-v1` pattern) + **class goldens** (corpus digests) as the certificate. The
Reproducer records its host class. Breadth of real classes (is "Skylake+ P-core" one class?) is an open
empirical question — the cross-chip differential sweep (bead `hm-bmh` apparatus, run
chip-vs-chip) is the measuring instrument.

## 5. Observability tie-in: the event→Moment index (rewind to a log line)

Naming note: deliberately **not** minting a term. This is mechanism-layer surface, which
per `docs/GLOSSARY.md` rule 1 takes plain-descriptive names only (an earlier draft's
"rehearsal marks" was a register violation — performance-practice vocabulary, the same
kill class as conductor/ensemble). Throughout: the **event index**, whose entries are
`(event, Moment)` pairs.

**Idea (Paul, 2026-07-10).** In a natural run, the app already narrates itself — log
lines, OTel spans (`PUT 5` → event). Because a deterministic run makes **(Reproducer,
Moment) an address**, stamping every app-level event with the Moment at which it was
emitted turns the app's own observability stream into an index of rewind points. An
error occurred? Step through the *app-level sequencing* that led to it — each step a
`run_to(moment)` on a replay, not a log-grep.

Mechanics — two capture paths, both cheap:
- **Passive (zero code change): serial/console writes.** The vmm handles the console
  exit, so it knows the Moment of every emitted byte; the serial capture is already in
  the state blob (`SERL`). Record `(byte offset → Moment)` entries in the runtrace
  journal at each console write; log-line → Moment falls out for free, and `logtmpl`
  (task 67) already templatizes lines for grouping. Any app that logs already populates
  the event index.
- **SDK / OTel path (one integration): the doorbell.** An OTel exporter (or log appender)
  that writes events through `/dev/harmony` (hypercall doorbell, R-L3 surface) — the vmm
  stamps the Moment at the hypercall, then re-emits the event host-side as OTel data
  with `harmony.reproducer` + `harmony.moment` attributes. Their existing Jaeger/Grafana
  then displays spans that *are* rewind coordinates.

Verbs:
- `harmony events <reproducer>` — list the event index (templated lines / spans, with
  Moments).
- `harmony resolve <reproducer> --at <moment|event>` — boot class-matched replay, `run_to`
  the entry's Moment, attach the resolution client (inspect memory/regs; prev/next event
  = step the *application* timeline; snapshot pool + lazy materialization make
  backwards-stepping cheap).

Notes and edges:
- An entry's Moment = the console/doorbell exit Moment — well-defined; buffered loggers
  index at flush (document it; the SDK path avoids the ambiguity entirely).
- The OTel collector, if used in-box, runs inside the subject; export leaves via doorbell
  or post-epoch drain — never a live network hole.
- This composes with the runner (§3): the Reproducer + its event index mean a failed CI run ships with
  its own clickable timeline. No dissonance anywhere in the loop.

O-stages: O1 serial event-index capture in runtrace + `events` verb; O2 `resolve --at` over the
snapshot pool; O3 doorbell OTel exporter + host-side re-emission; O4 (stretch) Grafana/
Jaeger link-out ("open in harmony").

## 6. Sequencing

```
spike branch merge (foreman)
  → I1 harmonyd → I2 vsock → I3 appliance build → I4 launcher/doctor
                                   ↘ I6 gate-mode CI template (early, cheap)
  I4 → I5 ApplianceRun            I4 → §4.1 qualification sweep (afternoon-scale)
  I1–I4 → R1..R4 runner track     R-track → O1..O3 event index
  (R5 shim-v2, octave: explicitly later)
```

Open questions carried, not blocking: SMP determinism (runner caveat until solved);
class breadth (measure, don't assume); Windows/WSL2 and Hyper-V-class hosts (probe when
a user exists); AMD/ARM personalities (ARCH-BOUNDARY seam, separate programs);
**work-clock gaps under branch-poor execution** (the honest cost of the
conditional-branches event, 2026-07-10 analysis): (a) **the `for(;;)` freeze** — a
busy loop with no conditional branch (`jmp .`, which is exactly what `for(;;)` compiles
to) retires zero work, so V-time stops, timer interrupts (delivered at work moments)
never arrive, the guest kernel never reschedules, and the whole virtual machine wedges
at that Moment — fail-loud, but the run is lost past that point, where a
proportional-clock design keeps simulating and can *observe* the hang. Designed fix
(2026-07-10, unbuilt): **nondeterministic detection, deterministic construction** — a
host-side liveness probe (W unchanged across wall-clock samples while a deadline is
armed) only *decides*; the break is *constructed* by rewind: schedule
`StarvationBreak at (W, k)` — `k` a fixed hardcoded constant (e.g. 1,000) — in the task-59 event schedule, restore the nearest
snapshot ≤ W, `run_until(W)`, take exactly `k` MTF single-steps, then apply the
`hlt` idle rule generalized (V-time warps to the next pending LAPIC deadline — a busy
branchless spin is *uncooperative idle*) and inject. Detection timing is laundered out;
replay re-constructs from the schedule with no watchdog; a branch retiring within the K
steps aborts the break (false positives self-heal via the rewind). Escalation ladder:
maskable timer → NMI (the hardware-watchdog analogue, for `cli;jmp`) →
`RunOutcome::Starved` + FINDING (that machine is dead on real silicon too). Introduces
the **(Moment, step-offset) address** — the same extension gap (c) below needs — plus an
EnvSpec/schedule entry (versioned) and `RunOutcome::Starved`; no new kernel patch in v1.
Validation gate before trust: adversarial payloads (`for(;;)`, `while(1)x++`,
`pause;jmp`, `cli;jmp`, huge `rep movsb`), 1,000-rep record==replay bit-identity with
breaks in-schedule, and a zero-break false-positive soak on postgres + corpus. (b) **V-time distortion through branch-poor bulk compute** (ERMS `rep movsb`
memcpy, constant-time crypto, heavily unrolled SIMD): determinism unaffected, but
guest-visible time crawls relative to work, so timing-realism-dependent behavior around
those phases is misrepresented (proportional clocks distort too — IPC-blind — but less
pathologically). (c) **injection density**: fault Moments are branch-boundary-grained,
so the explorer cannot currently express a cut mid-straight-line; the substrate can
(land at work W, then k MTF steps — extend the address to (Moment, step-offset)) but it
is unplumbed. Note `rep`-string interiors are a wash: retirement-counter designs of
either event cannot split a single `rep` instruction.
