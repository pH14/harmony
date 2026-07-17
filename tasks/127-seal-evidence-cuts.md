# Task 127 — Capture seal evidence cuts across the VM control seam (hm-bbx.6)

Claim `hm-bbx.6` first (`bd update hm-bbx.6 --claim`). This is the **keystone** of the
Differential-migration epic (`hm-bbx`): it is currently the only thing blocking `hm-bbx.4`
(Explorer↔Differential-cells integration), which in turn blocks `hm-5sv`, `hm-m78`, `hm-cs5`.
Get it right — a whole cascade sits behind it.

Read first, in full: the bead `bd show hm-bbx.6` (description + design + **acceptance criteria**
+ notes — they are the contract), `docs/DISSONANCE-STRATEGY.md` (the ratified ruling — the DD
GO is ratified; `hm-bbx.5` prefix ratification is closed), and `docs/GLOSSARY.md` for the
vocabulary (Moment, seal, cut, Reproducer). The epic `bd show hm-bbx` carries the overall
acceptance frame.

## Scope — capture and transport ONLY

Bind every **successful production seal** to its exact **evidence cut**. Concretely:

- The snapshot response (in `dissonance/control-proto/` — `types.rs` + `codec.rs`) carries,
  atomically from the **same stopped-server state**: the snapshot handle, the synchronized
  `Moment`, the taint, and the **included SDK-event count** (= the ordered SDK-capture vector's
  **prefix length**; positions below the count are included, at/after are excluded).
- `SocketMachine`, the `Machine` test doubles, snapshot metadata, and `PendingFork` (in
  `vmm-core` / the explorer seam — `dissonance/explorer/{seam,spine,adapter}.rs`) carry that
  server-stamped cut through metadata, pending forks, and persisted lineage **without a second
  read as authority** — the server stamp is the sole authority.

**Out of scope (do NOT do here):** decoding SDK payloads, building Differential relations,
reducing observations, or deciding archive occupancy. This child owns *capture + transport*.
The serial-console scrape stays source-local and stop-granular — it is NOT folded into this
cursor, and console bytes must be structurally unable to enter the SDK count or any
seal-relative cell test. A later seal-relative source gets its own declared cursor; independent
cursors never imply cross-source order.

## Acceptance criteria (from the bead — all must hold)

- Control-protocol **goldens + hostile-decode tests** cover tainted AND untainted snapshot
  replies carrying the cut.
- **Same-Moment fixtures** prove events emitted before the seal are included and events emitted
  after it are excluded (this is the load-bearing determinism property; coordinate with
  `hm-ynt` in tasks/126 — the cut is by SDK-vector prefix length, NOT by Moment comparison).
- Branch/replay **preserves the captured SDK prefix length**; the cut is **identical across
  same-seed runs and platforms** (macOS + Linux — this is a determinism contract, so if any
  part can only be exercised on Linux/KVM, run that part on the determinism box `ssh <det-box>`
  pinned per `docs/BOX-PINNING.md`, and say which coverage ran where).
- A **failed or non-quiescent snapshot returns neither a usable handle nor a cut** (no partial
  cut on failure).
- Console bytes cannot enter the SDK count or seal-relative cell tests.

## Gates & done

Full portable gates green (fmt, clippy --all-targets -D warnings, nextest, public-api snapshot;
this changes `control-proto`'s wire surface, so expect and justify a `public-api.txt` diff and
new/updated control-proto goldens). This is a wire-format change on a shared seam — treat the
codec/golden discipline as first-class. Open a PR with a review-grounding description that maps
each acceptance-criterion to its test. `hm-bbx.6` closes on merge and unblocks `hm-bbx.4`.
Escalate (do not guess) on any contradiction between the bead's acceptance criteria and
`docs/DISSONANCE-STRATEGY.md` — that is an integrator ruling, not an implementer decision.
