# Task 157 — W8 Intel box lane (hm-i2et) — implementation record

**Branch:** `task/intel-box-lane`. **Box:** hetzner (Intel Core i9-9900K, stock KVM
1396736). All box work pinned to the leased core (governor=performance,
no_turbo=1), serialized through `scripts/box-window.sh`, box returned to stock
after every run. One box-touching worker throughout (co-tenancy would make a real
divergence indistinguishable from noise).

Worked strictly in the spec order. **Closed: hm-lld, hm-rdp.** **Not started:
hm-2nt, hm-i8kc** (both need a guest-image rebuild; see below).

---

## 1. hm-lld — conductor opts into the remap-restore factory ✅ CLOSED

**Change (`dissonance/campaign-runner/src/boxrun.rs`).** `boot_server` now installs
a `RemapVmmFactory` via `vendor::x86::bringup::compose_restore_target`, mirroring
the existing `VmmFactory` **minus the boot-image load** — the exact shape of
`vmm-core/tests/live_dirty_remap.rs`'s (b) gate. `ControlServer::set_remap_factory`
flips the server's `RestoreMode` to `Remap`, so every campaign/sweep
`branch`/`replay` restore takes the memslot-remap path (the materialized mapping
IS the guest RAM the memslots register — no full-image memcpy, untouched pages
fault lazily under CoW) instead of the pre-task-95 memcpy path. campaign-runner
was outside the task-95 M2 surface waiver, so this composition root still restored
via memcpy. The remap factory is composed identically to the live VM (patched KVM
backend, xAPIC wired, V-time wired at `BOOT_SEED`, `enable_pvclock` under
`page_on`) so a restored branch cannot drift from its source.

A host-side A/B knob, `HARMONY_RESTORE_MEMCPY=1`, flips the *same binary* back to
the memcpy path (`set_restore_mode(Memcpy)`) for the before/after comparison and as
a fallback if a remap restore ever misbehaves. It never reaches guest state or a
hash — the task-95 (b) gate proves both paths restore to the identical
`state_hash`, so an A/B over it is a pure timing comparison.

**Live gate (box, core 2).** `campaign-runner box --seeds 8 --runs 2
--deadline-delta 20000000`, pinned pr44 postgres image (sha256 `f06a34…` /
`3c4a7f…`):

- Serial confirms `box: restore path = Remap (task 95 M2.2)`.
- Sweep **GATES PASS** over remap: per-seed reproducible, ≥2 distinct futures,
  `replay(base) == capture`.
- **A/B before/after** (identical params, one window; single shot each):
  - BEFORE memcpy: 16 branches in **213.62 s**
  - AFTER  remap : 16 branches in **207.69 s**
  - **Caveat — this figure is diluted, and understates the remap win.** It is a
    single-shot **2.8 % total-wall** delta, not the restore-cost ratio: each
    branch's wall is dominated by the fixed 20 000 000 ns V-time run + the 2 GiB
    `state_hash`, with the memslot-remap-vs-memcpy restore only a small fraction of
    it. The *isolated* restore cost is the task-96 stopwatch's **Branch phase**, not
    the total sweep wall; do not read 2.8 % as the restore speedup.
- Per-seed **and** base `state_hash` are **bit-identical** across the memcpy and
  remap arms (base capture `2eb3eb1f…`; e.g. seed `0x9e1f…` → `ec5edbbb…` in both)
  — the task-95 (b) property, now demonstrated at the campaign level. The task-96
  stopwatch's Branch phase now quotes gate-(d) restore numbers from real campaigns.

**Portable/box gates:** `clippy --all-features --all-targets -- -D warnings`
EXIT 0 (the `rand::thread_rng`/`rand::random` notices are pre-existing
clippy.toml disallowed-method entries that don't resolve in this crate's dep
graph — config warnings, not code lints; clippy exits 0), `fmt --check` OK,
`nextest` 177 passed / 3 skipped (the box-only `#[ignore]` tests) — run on the box
because the Mac cannot compile the `cfg(linux)` `boxrun.rs`/kvm crates.

---

## 2. hm-rdp — flow-agent doorbell, first-ever live firing ✅ CLOSED (guest-side)

**Finding.** The flow-agent's doorbell (`harmony-linux/flow-agent`) needs
`iopl(3)` (`CONFIG_X86_IOPL_IOPERM=y`, e60ff83) and an `/dev/mem` mmap of the fixed
REQ/RESP hypercall pages (`CONFIG_DEVMEM=y`, 48fb632) — flags added to the guest
kernel only after this code was written, so the live path could never have
executed pre-48fb632.

**Vehicle (committed, reproducible).** `harmony-linux/linux/build-flow-image.sh` +
`flow-init.sh` (mirroring `build-maze-image.sh`): a static busybox + static-musl
flow-agent + `flow-init.sh` as `/init`, booted on the doorbell-capable game/maze
bzImage (md5 `0bd7ddd5` — the same kernel whose *identical* play-agent doorbell
fired in task-86 M0), driven to a `FLOW_DONE` marker by `campaign-runner box`.
Built from source on the box (BUILD_EXIT 0) and fired twice (throwaway + committed
image) with identical results.

**Live firing (box, core 2), serial:**

```
FLOW_DEVMEM: present            <- CONFIG_DEVMEM=y (48fb632) live
FLOW_URANDOM: present
flow-agent: selfcheck urandom=b8cc1970940b6fcc6e3b1cc6c535806d monotonic_ns=29312789
flow-agent: Net doorbell unwired (host has no Net service) -> nominal
flow-agent: flow conn=1 1->2 policy=Nominal
flow-agent: nominal — no enforcement installed
FLOW_AGENT_RC=0
```

The agent reached `net_decide` with **no `iopl(3)` error and no `/dev/mem` error**
⇒ `iopl(3)=0` and the `/dev/mem` mmap of REQ_GPA/RESP_GPA both succeeded, the OUT
was issued, and a clean deterministic response returned. **The guest-side doorbell
path that "never once executed" now executes** — hm-rdp's claim is validated, and
the guest-side prerequisite for hm-wvh (task 61b live net-fault enforcement) is
cleared.

The real firing evidence is the serial above (`iopl(3)=0`, `/dev/mem` mmap OK, OUT
issued), **not** the sweep verdict. The boot is deterministic and the sweep prints
GATES PASS, but for *this* harness that is a **weak** signal (PR161-F1): the
≥2-distinct-futures check passes via the **VTIM reseed fold** — a branch reseed
folds `SeededEntropy::save_state()` into the hash chunk, distinguishing every
seed's hash with **zero** post-seal guest entropy (all `/dev/urandom` reads
pre-date the seal and are baked identically into the base) — not via any guest
divergence. That same fold is why `flow-init.sh` must **success-gate** the
`FLOW_DONE` marker: without it, a Crash-path run (a failed agent) would satisfy all
three sweep gates and print a vacuous PASS. The planted-failure red-fire of the
fixed image (W1 doctrine) **was run on the box — hm-5zch, both arms verbatim in
§2a below**.

**Caveat (→ hm-i8kc F10).** The host answered `DoorbellUnwired → Nominal` (a clean
deterministic fallback, rc=0) because the doorbell fires during `boot_server`'s
`drive_to_marker`, **before** `ControlServer::new` wires `enable_net` — the F10
ordering. A full `net_decide` round-trip with a real (Nominal-from-a-wired-host or
staged-fault) answer needs the agent to run mid-served-workload (the k3s image
path, hm-wvh) or an F10 wiring fix. This is a directly-observed corroboration of
hm-i8kc's F10, not a flow-agent defect.

No product-code change — the flow-agent crate is unmodified; the commit adds only
the box evidence harness.

### 2a. hm-5zch — the W1 red/green pair for the success-gated marker ✅ box-verified

The PR161-F1 fix (`flow-init.sh` emits `FLOW_DONE` only on `rc == 0`) was proven
red-before-green on the box (hetzner, core 2, governor=performance, no_turbo=1,
patched KVM via `box-window.sh`, reverted to stock 1396736), both arms in one
window. **ARM 1** boots a planted-failure image — the *committed, unmodified*
`flow-init.sh` with `/opt/harmony/flow-agent` replaced by a stub that exits 3
(`FLOW_AGENT_BIN=<stub>` at image-build). **ARM 2** re-runs the real fixed image.

```
LEASED_CORE=2 GOVERNOR=performance NO_TURBO=1
######## ARM 1 — NEGATIVE CONTROL (planted failure; MUST go RED) ########
FLOW_DEVMEM: present
FLOW_URANDOM: present
PLANTED_FAIL: simulated flow-agent failure (exit 3) — hm-5zch negative control
FLOW_AGENT_RC=3
FLOW_FAILED: reboot (flow-agent failed)
[campaign-runner] failed to reach the readiness marker: guest reached a terminal
    (Shutdown) at step 97471 before the readiness marker appeared
ARM1_RC=1
######## ARM 2 — POSITIVE CONTROL (fixed image; MUST stay GREEN) ########
FLOW_AGENT_RC=0
FLOW_DONE
[campaign-runner] box GATES PASS: per-seed reproducible, >= 2 distinct futures, replay == capture.
ARM2_RC=0
REVERT OK   (lsmod kvm = 1396736)
```

**ARM 1 goes RED:** the planted `rc=3` triggers `FLOW_FAILED: reboot` *before* any
marker, the triple-fault reaches `drive_to_marker` as `Step::Terminal(Shutdown)`,
`boot_server` fails, `campaign-runner` exits non-zero (`ARM1_RC=1`), and
**`box GATES PASS` appears zero times in the ARM-1 section** (total across the whole
run = 1, from ARM 2 only). Before the fix this exact run printed `GATES PASS` and
exited 0. **ARM 2 stays GREEN:** `FLOW_AGENT_RC=0` → `FLOW_DONE` → `GATES PASS` →
`ARM2_RC=0`, so the guard did not break the success path it protects. hm-5zch
closed; the gate is now trusted because it has been *seen* to go red on a planted
failure.

---

## 3. hm-2nt — draw-probe gate on the new Postgres image ✅ CLOSED (session 2)

**The bead asked for a re-bake; measurement says no re-bake was ever needed.**
The Jul-9 image is now an approved gate baseline, green at the standard
configuration, with its characteristics recorded — and the premise it was filed
under is refuted.

### What the bead assumed, and what is actually true

hm-2nt (and `live_materialization`'s own module doc) held that the Postgres
workload's `gen_random_uuid()` loop "rides `pg_strong_random` → RDRAND", so the
2026-07-09 rebuild's first entropy draw had merely drifted past the default hop
windows and a `READY_MARKER` moved into the uuid loop would recover it.

The new `postgres_baseline_marker_timeline` probe measures that directly — it
reads `Vmm::state_components()`'s `vtim:entropy` (the `SeededEntropy` position)
at each workload marker, so an interval "drew" exactly when the stream moved.
Both images, one boot each:

```
marker                                     vtime_ns      entropy_state  drew?
Linux version                              82019814   6bdb5981672a7b6a    -
random: crng init done                     82019814   6bdb5981672a7b6a    no
Run /init as init process                 422419380   5382e1a597a4908f   YES
PG37: starting postgres                   424055064   5382e1a597a4908f    no
database system is ready to accept …      441941448   5382e1a597a4908f    no
row|1|1|1|                                448967766   5382e1a597a4908f    no
PG37: workload end                        451053256   5382e1a597a4908f    no
GUEST_READY                               455499740   5382e1a597a4908f    no
```

**The Postgres phase draws no seeded entropy at all.** Every draw happens during
the initramfs/early-userspace span; from 422 M v-ns to the terminal the stream
never moves. The uuid values are seed-derived because the *kernel CRNG* was
seeded from the stream at boot, not because the loop draws live. So no marker
placement inside the workload — the bead's prescription — can change the draw
map, and the Jul-9 rebuild is not the variable: the two images run the same
program to within ~35 µs of V-time at every marker, with identical draw maps.
What broke in July was the then-default `HOPS=3`, already corrected to `HOPS=4`
for pr44 (the harness's own note said as much for the pinned image).

Three controls make that negative result trustworthy rather than an instrument
failure: (1) a **positive control** — the same probe *does* register the
early-userspace draw, so it is not stuck; (2) the probe wires the **SDK and Net
doorbell channels exactly as `ControlServer::new` does** (`wire_doorbell_channels`),
because `doorbell_service_offered` gates the Entropy service on those channels —
a bare VM would silently refuse a guest's entropy request and under-report;
measured both ways, byte-identical. (3) `SeededEntropy` is an xorshift whose
`save_state()` is the live state word, so an unchanged tag cannot hide a draw.

### The change

`live_materialization.rs` replaces its two `PINNED_*` constants with a
**`BASELINES` table**. A baseline is now both halves of what a gate needs: the
(kernel, initramfs) content pins **and** the image characteristics the draw probe
depends on — ready marker, hop count, window widths, provenance. `BASELINE=pr44`
(default) is byte-for-byte the old behaviour; `BASELINE=jul9` is the rebuild.
An unknown `BASELINE` is a loud refusal listing what is approved; the
name-without-hash and hash-mismatch refusals are unchanged. The two images live
under **distinct filenames** (`initramfs-postgres-jul9.cpio.gz`) so selecting one
is a `BASELINE=` choice, never a file swapped under a shared name — which is how
the 2026-07-09 drift happened in the first place.

### Live gate (box, core 2, governor=performance, no_turbo=1)

`BASELINE=jul9`, no other knobs — its own committed defaults, `REQUIRE_DRAWS=1`:

```
base: sealed at V-time 442953098 (2 attempts)
hop 0..3 landed 445221603 / 447221744 / 449247686 / 451289424  (attempts 1 each)
hot    depth 2041738  ratio  4524 ppm   (task-63 baseline 15463 ppm)
folded depth 4067680  ratio  9013 ppm   folds 1
worst  depth 8336326  ratio 18472 ppm   from_genesis
round-trip: folded == hot, worst == hot   (state_hash b72a4122…)
reproducer: leg == replay  Deadline@452433490  (state_hash 13a8ec92…)
draw probes (task 78): hops [false, false, false, true]; tail window DRAWS
[REPORT] GATES PASS
```

The pr44 regression arm, same session, same binary, its own defaults: `GATES
PASS`, hot ratio **4524 ppm** — identical — hop pattern identical, depths within
~100 ns. The refactor did not move the proven configuration.

### Open finding, filed not buried: the draw probe disagrees with the stream

The gate reports `hop_draws[3] = true` and `tail_draws = true` for windows
`[449.2 M, 451.3 M]` and `[451.3 M, 452.4 M]` — squarely inside the span the
measurement above shows is draw-free. Both instruments are load-bearing and they
cannot both be right:

- if the trailing-reseed probe has a **false positive**, then `REQUIRE_DRAWS` is
  vacuous on these guests and the task-78 "bit-identical *even when entropy is
  drawn inside a collapsed interval*" box evidence does not rest on a drawing
  window at all;
- if the probe is right, then a **restored, reseeded branch draws where the live
  boot does not** — an execution difference between the live and branched paths,
  which matters for replay fidelity.

Mechanically the probe *should* be a no-op under no draws: `reseed_probe_env`
records two markers to the **same** seed (`record_reseed(0, seed)` and
`record_reseed(rel, seed)`), and `reseed_entropy` is a plain
`vt.entropy = SeededEntropy::new(seed)` assignment, so with an unmoved stream
both legs end at the identical state. The portable loopback's draw-free script
agrees (all probes false). The next experiment is named in the bead: diff
`state_components()` between the plain and probe legs of hop 3 to see **which**
hashed chunk differs (entropy, `vtim:eff-vns`, or RAM). Not resolved here — this
lane certified an image, and quietly certifying it against a precondition whose
meaning is in doubt would be exactly the vacuity the W1 doctrine exists to stop.

### W1 red/green: every guard this commit adds, seen to fire

Five negative controls and a positive one, all on the box in one window. `rc=101`
is the test process panicking; `GATES PASS` appears **zero** times across the
whole red log and once in the green gate run.

| arm | provocation | observed |
|-----|-------------|----------|
| A | `BASELINE=whatever` | `rc=101` — *"is not an approved gate baseline (approved: ["pr44", "jul9"])"* |
| B | `BASELINE=jul9` pins against the **pr44 file** | `rc=101` — content-hash mismatch, both hashes quoted |
| C | image named without its hash | `rc=101` — *"overriding the image requires supplying its content hash"* |
| D | timeline probe, a marker that never appears | `rc=101` — *"only 1/2 markers appeared before the guest Shutdown"* (never a vacuous pass) |
| E | `BASELINE=jul9 HOPS=3` (the July default) | `rc=101` — `REQUIRE_DRAWS` red: `hops [false, false, false], tail true` |
| green | timeline probe, real markers | `rc=0` |

Arm **E** is the substantive one: it reproduces, on the new image, exactly the
failure hm-2nt was filed for — and shows the knob that governs it is `HOPS`, not
the image or the marker. Arm **D** matters because a characterization probe that
"passes" on an image whose workload never ran would be worse than no probe.

*(First pass at these arms reported `ARM_x_RC=0` because the run wrapper read
`PIPESTATUS` through a shell function — the panics were still the evidence, but
the exit codes were masked. Re-run with the test's own status; the table above is
the corrected run. Noted rather than silently fixed: a harness that reports a
green rc on a panicking test is the same green-on-fail shape PR161-F1 was.)*

## 4. hm-i8kc — /dev/harmony bridge liveness family (F2/F9/F10/F11) ⬜ NOT STARTED

The meatiest item, deliberately last; not reached. F2 ("the live transaction *is*
the item") needs a guest image that opens `/dev/harmony`, emits a JSON SDK event,
and reads entropy — i.e. a **libvoidstar** guest (the play-agent dynamic-glibc +
`dlopen` pattern, heavier than the static flow-agent) with `/dev/harmony` +
`libvoidstar.so` mounted into the container rootfs (F11). F9 (select
`AntithesisJson` ingestion so the driver's event-id-0 = `CATALOG_EVENT_ID` JSON is
not misparsed by the default `Ingress::Binary` path) is only meaningful once F2's
live JSON exists. **F10 is directly corroborated by item 2 above** — the net/SDK
doorbell is wired only after `boot_server`'s readiness marker, so a pre-marker
emission hits the default-deny un-wired doorbell; the fix is to wire the channel
before the boot drive. A full close needs a libvoidstar guest-image build + a live
`/dev/harmony` box transaction and is a multi-part effort in its own right.

---

## Judgment calls

- **Item-1 gate via the sweep, not the planted-bug campaign.** The box sweep's
  determinism gate (per-seed reproducible, replay==capture) proves remap-restore
  correctness end-to-end through the conductor without needing to pin the campaign
  image's supervisor-ledger gpa; the planted-bug campaign would fail `finish_campaign`
  ("no bug found") without gpa pinning, which is orthogonal to the restore path.
- **`HARMONY_RESTORE_MEMCPY` env knob** (vs a CLI flag on every box arg struct):
  one read in the shared `boot_server` covers sweep/campaign/game/maze uniformly;
  host-side, never state/hash-observable.
- **Remap factory hardcodes `PatchedKvmBackend`** (not a `BackendKind` match):
  `X86_64_BOOT.backend` is the compile-time const `Patched`, and a remap restore
  only runs on patched KVM — matching the worked example exactly.
- **Box-window lease pid.** `box-window.sh acquire` records `$PPID`; capturing the
  core via `core=$(… acquire …)` records the *command-substitution subshell* pid,
  which dies immediately and is swept on the next `sweep_stale`. Fixed in the run
  wrappers by redirecting acquire's stdout to a file
  (`… acquire "$LEASE" > .core`) so the long-lived script is the lease holder.

## Box safety

Every box run acquired/released through `scripts/box-window.sh` with an EXIT-trap
release (reverts to stock + verifies on the last lease out, even on failure).
After the final run: `kvm = 1396736 (REVERT OK)`, `live leases: 0`. The box is at
its clean resting state.

**LANE COMPLETE — closed: hm-lld, hm-rdp | reached-not-closed: (none) |
untouched: hm-2nt, hm-i8kc.** Box back to stock KVM 1396736, no leases held.
