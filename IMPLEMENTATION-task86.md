# Task 86 — the real-game workload (SMB): M0 implementation record

**Branch `task/game-workload` · M0 (workload bring-up) per the 2026-07-09
amendment.** M0 = guest image + core provisioning + billboard + workload-init
+ the boot/determinism gate harness + campaign runs under the *existing*
default/baseline search, absorbing film's re-homed live gate. M1 (the selector
referendum, gates 3/4's verdict) stays queued behind a selector artifact —
the gate *machinery* is built and committed; only its Signal input is absent.

## What was built, where

- **`guest/play-agent/`** (new crate, standalone workspace): the weighted
  chord input policy (one entropy byte per `W`-frame window, weights sum to
  exactly 256), the SMBDIS-verified RAM-map decode, the billboard writer
  byte-matched to film's merged `HBBD` v1 layout (golden-pinned), the
  state-register catalog (`REG_GAME_MODE..REG_BILLBOARD_LEN`, ids under
  `pack_state`'s 16-bit bound), the per-frame agent loop over a mock-core
  seam, and the Linux glue (dlopen'd QuickNES FFI, hugetlb+pagemap+mlock
  billboard pinning, the flow-agent doorbell). See its `IMPLEMENTATION.md`.
- **`guest/linux/`**: `versions.lock` pins QuickNES_Core `26bb785c` by
  tarball sha256; `fetch.sh` fetches it (the ROM is **never** fetched);
  `build-game-image.sh` builds core + agent, copies their ldd closure, bakes
  the user-supplied `HARMONY_SMB_ROM` (sha256 recorded in-image and echoed) or
  SKIPs loudly, packs the reproducible `initramfs-game.cpio.gz`, and exports
  `guest/build/quicknes_libretro.so` — the **same** artifact film's renderer
  dlopens (the shared pin, 1:1 by construction); `game-init.sh` reserves the
  billboard hugepage and maps agent failure to the `reboot -f` crash terminal;
  `game-image` Makefile targets.
- **`dissonance/benchmark`** (extended, not forked): `exploration.rs` — the
  tasks-84/86 discovery-event log (per-branch touched cells + depth +
  terminal `state_hash`), `GameManifest` (ROM sha256 + input shaping +
  budget), medians/IQR over the existing exact-rational order statistics, the
  pooled STADS curve + running Good–Turing stopping rule, the strict
  signal-beats-pure-random predicate (greater medians AND disjoint IQRs on
  BOTH measures, live control), and the `SMB-EXPLORATION-REPORT.md` renderer
  (`exploration-report` bin). Verdict is Pass/Fail/**Incomplete** — an M0
  baseline-only run or a ROM-less manifest renders loudly, never green.
- **`dissonance/conductor`**: `gamecampaign.rs` — the quiet-arm campaign
  driver (`FaultPolicy::none()` via `SpecEnvCodec::seeded`, **no `.perturb`
  anywhere**), sealing the base at the play-agent's `setup_complete` snapshot
  point; host-side cell keying per R-L2 (`LinkSensor` features → the
  `(game mode, world, level, x-bucket)` tuple; `REG_FRAME` floods ignored);
  the `GameToyMachine` (SMB-shaped, drives the REAL wire-decode → sensor →
  key path portably); CLI `conductor game mock` / `conductor game box`
  (`--repeat 25` = the fresh-boot bit-identical determinism gate; `--logs-out`
  = the report's input artifact).
- **CI**: `quality.yml` gains the play-agent out-of-workspace gate step;
  `nightly.yml` gains its Miri line (run green locally on the pinned nightly).

## Judgment calls on the record (for the reviewer/integrator)

1. **Task 84's machinery does not exist on `main`** (spec-only — no maze, no
   composed-engine campaign, no exploration report). The spec says "the
   campaign path through the composed engine is task 84's, reused" — there is
   nothing to reuse, and the amendment orders M0 to run the **existing**
   default/baseline search. So the campaign rides the conductor task-60/69
   hand-rolled loop shapes; the `explorer::Explorer + SocketMachine` on-ramp
   remains task 84's deliverable (NOT rebuilt here, and not smuggled in). The
   benchmark extension was built generically (`ExplorationLog`/report) so
   task 84's maze can emit the same log and share the report machinery.
2. **The campaign harness lives in `dissonance/conductor`, not
   `consonance/vmm-core`.** The surface list says "vmm-core — campaign
   manifest/config wiring only", but the campaign harness this project
   actually has (tasks 58/60/69) lives in conductor and drives vmm-core's
   `ControlServer` unchanged; zero vmm-core edits were needed (the game image
   drops into the existing `--initramfs` + marker + boot path). Task 84's spec
   text has the same vmm-core/conductor labeling slip.
3. **`ExplorationConfig::Signal` is refused loudly** by the campaign driver
   (`SignalUnavailable`): task 70 was NO-GO'd, so running the default search
   under a "signal" label would fake the held-out test. The report renders
   Signal-less inputs as **INCOMPLETE**, never green.
4. **Billboard ownership**: the layout is film's merged definition, mirrored
   byte-for-byte (goldens on both sides); integrator reconciliation onto one
   owning constant can ride either crate's next touch.
5. **QuickNES license label**: repo LICENSE is GPL-2.0 (spec table said
   LGPL-2.1; the embedded Nes_Emu is LGPL). Same fetched-never-vendored
   discipline either way; no repo code links it.
6. **Register numbering** is play-agent-local (ids 1–9 + points 1–2, the
   sdk-demo precedent), mirrored as constants in `gamecampaign.rs` with the
   mirror-comment; ids stay < 2^16 for `pack_state`.

## Portable gates (all green, macOS + Linux-target cross-check)

- `guest/play-agent`: 36 tests (fixed-tape chord decode, per-register RAM
  fixtures, billboard round-trip vs film's canonical bytes + hard golden,
  once-per-window emission, markers-fire-once, ≥256-case proptests), clippy
  `-D warnings` on host and `x86_64-unknown-linux-gnu` (compiles the FFI),
  fmt, `cargo deny licenses`, **Miri green** (pinned nightly-2026-06-16,
  lib + agent suite).
- `dissonance/benchmark`: 42 tests incl. 3 new ≥256-case proptests
  (bookkeeping vs naive models, serde round-trips, known-separation ⇒ known
  verdict), clippy/fmt.
- `dissonance/conductor`: 116 tests incl. the gamecampaign suite (bit-identical
  reruns, seed divergence, cell-key injectivity + FRAME-flood immunity, Signal
  refusal, selector-v1 vs pure-random divergence), clippy on both targets, fmt.
- Root: full-workspace build + `cargo deny check` clean. CLI smokes:
  `conductor game mock` runs and appends logs; `--config signal` refuses;
  `play-agent --smoke` runs the frame loop + billboard with no core/ROM/host.

## Box gates — the live path (handed to the foreman)

**Blocked on the ROM**: `HARMONY_SMB_ROM` is user-supplied (Paul owes the
dump); without it every game gate below reports **SKIP loudly** by
construction. Box was reachable at hand-off (`ssh hetzner` OK 2026-07-09).
⚠️ **Observation**: `lsmod` showed `kvm_intel 417792, used by 6` — neither
the documented stock size (1396736) nor idle. Other work was live on the box;
I did not touch KVM. Verify stock per `docs/BOX-PINNING.md` before/after any
run, and pin builds to a leased core (the image build uses `-j$(nproc)` —
scope it: `taskset -c <lease> make -C guest game-image`).

```sh
# 0. provision (box, leased core; needs the branch checked out there)
make -C guest fetch                                  # pulls the pinned QuickNES tarball
taskset -c <core> make -C guest kernel               # if bzImage not already built
HARMONY_SMB_ROM=/path/to/smb.nes taskset -c <core> make -C guest game-image
#   -> guest/build/initramfs-game.cpio.gz  (ROM sha256 echoed — record it)
#   -> guest/build/quicknes_libretro.so    (the SAME pin film's renderer dlopens)
# ROM-independent smoke (can run before the ROM lands): unset HARMONY_SMB_ROM,
# build, boot — the guest must print GAME_SKIP and halt (Quiescent).

# 1. smoke-fire-once-before-campaign-spend (the ruled discipline):
taskset -c <core> cargo run -p conductor --release -- game box \
    --max-branches 4 --deadline-delta 2_000_000_000
# serial must show GAME_ROM_SHA256 + GAME_READY; the base seals at the agent's
# setup_complete SnapshotPoint; expect nonzero distinct_cells.

# 2. gate 2 — determinism 25/25 (fresh boot per repetition, bit-identical
#    per-branch state_hash sequence; every branch is itself a reproducer
#    replayed 25/25 across the repetitions, the deep one included):
taskset -c <core> cargo run -p conductor --release -- game box \
    --config pure-random --campaign-seed 1 --max-branches <B> --repeat 25 \
    --logs-out smb-logs.json
# Co-tenancy discipline per docs/BOX-PINNING.md: solo vs co-tenant divergence
# is a P0 STOP + escalate, never serialize-to-hide.

# 3. M0 campaign runs (both available configs, >=20 seeds each, identical
#    budget; append to one logs file):
for s in $(seq 1 20); do
  taskset -c <core> cargo run -p conductor --release -- game box \
      --config pure-random --campaign-seed $s --max-branches <B> --logs-out smb-logs.json
  taskset -c <core> cargo run -p conductor --release -- game box \
      --config selector-v1 --campaign-seed $s --max-branches <B> --logs-out smb-logs.json
done
# manifest.json = benchmark::GameManifest::smb(Some("<rom sha256>"), <B>) as JSON
cargo run -p benchmark --bin exploration-report -- \
    --logs smb-logs.json --manifest manifest.json \
    --out dissonance/benchmark/SMB-EXPLORATION-REPORT.md
# M0 expectation: verdict INCOMPLETE (signal missing) — commit the report as
# the baseline-plateau record (the non-vacuity documentation); gates 3/4's
# verdict is M1's, after a selector artifact exists.

# 4. film's re-homed live gate (task 87 -> 86 M0): record a campaign
#    reproducer (billboard gpa/len ride the trace's REG_BILLBOARD_* events;
#    REG_FRAME every vblank is the shot list), then render per
#    dissonance/film/IMPLEMENTATION.md "The box gate" with
#    HARMONY_SMB_CORE=guest/build/quicknes_libretro.so HARMONY_SMB_ROM=<rom>:
#    (a) core loads in the box guest (step 1 proves it — retro_load_game ok);
#    (b) one real unserialize+retro_run validates film's env_cb assumption;
#    (c) a >=300-frame clip renders; (d) render-determinism (same bundle
#    twice, byte-identical); (e) 25/25 hash-neutrality (film observation
#    on/off -> identical state_hash).
#    Likely friction (both sides document it): QuickNES may demand more env_cb
#    services at load — extend play-agent's `retro::env_cb` and film's env_cb
#    symmetrically; both are one-match-arm changes.

# ALWAYS: box-safety per the spec — pkill the campaign bin first, wait users=0,
# rmmod/modprobe to stock, verify 1396736 on a FRESH ssh connection.
```

Sizing note for `<B>` (the branch budget): the spec sizes it from task 84's
measured branch rates, which do not exist yet (finding 1). Until 84 lands,
size from this workload's own smoke: pick `<B>` so 20 seeds × 3 configs fits
the box lease, and record it in the manifest (it is a manifest parameter, not
a gate constant).

## Findings / escalations

- **Task 84 absent** (finding 1 above) — sequencing note for the integrator,
  not a defect: M0 was explicitly re-scoped around it by the amendment.
- **`kvm_intel` size anomaly on the box** (417792, 6 users, 2026-07-09) —
  flagged for the foreman's next box session; not investigated further from
  here (active co-tenants).
- No spine defects surfaced; `explorer`/`link`/`guest/sdk` untouched
  (read-only surface respected). One pre-existing seam note the explorer pass
  confirmed: `Explorer::progression_step` does not drain `sdk_events()` into
  `RunTrace.events` (engine.rs builds `events: Vec::new()`) — irrelevant to
  M0 (the hand-rolled loop drains them itself), but it will matter when task
  84 composes `LinkSensor` into the real engine. Recorded here rather than
  patched (spine is read-only for this task).
