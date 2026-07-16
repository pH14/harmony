# tasks/108 — the ARCH-BOUNDARY restructure (steps 1–4)

Bead `hm-b5n`. Branch `task/arch-boundary-restructure`, four step-commits:

| Commit | Step |
|---|---|
| `0ae752e` | 1 — C-list neutralizations (§C + the two §A ones) |
| `15379da` | 2 — mechanical extraction of the x86 value types into `vmm-backend`'s arch module |
| `442e75b` | 3 — **the keystone**: `Arch` trait + generic `Backend` + the engine/vendor split in `vmm-core` |
| `adfdcec` | 4 — `vm-state` arch-tagged records + `VM_STATE_VERSION` 1 → 2 |

The ISA seam is now compiler-enforced. Three invariants the ruling cares about, and
how each is *checked* rather than asserted:

- **Default-deny is structural, at two levels.** `Exit<A: Arch> = Common(CommonExit)
  | Arch(A::Exit)`. The engine matches `CommonExit` exhaustively and cannot reach an
  arch exit except through that vendor's `dispatch_arch`, which matches its own enum
  exhaustively. There is no wildcard arm over arch exits anywhere in the tree.
- **The engine is arch-blind.** `Vmm<B>`'s production code names no x86 device, no x86
  register, and no x86 exit; it holds `<B::A as Vendor>::Devices` and reaches
  everything vendor-side through the `Vendor` trait. This is a *compile* property now —
  an ARM vendor drops in beside x86 without touching `vmm.rs`.
- **Generics stop at vmm-core.** No `<A: Arch>` parameter appears in any dissonance
  crate. `campaign-runner`, being a composition root, *names* the concrete vendor
  (`Backend<A = X86>`) instead — which is the ruling's own "main names the
  `(Backend impl, Arch vendor)` pair".

## The state-hash invariant (the named risk)

`state_blob`'s chunk roster and order are **byte-identical** through steps 1–3:
`MEM`, `VCPU`, `SERL`, `DEV\0`, `VTIM`?, then the vendor's `LAPC`?/`LEGY`?, `SDK\0`?,
`VMST`?. The `state_components` diagnostic labels (which the box O1 localizer quotes)
are unchanged too — I renamed two while moving the code and the pinned test caught it;
they are back to `regs` and `desc-tables`.

**No golden moved in steps 1–3.** The one deliberate byte change in the whole task is
step 4's `vm-state` header (below).

## Deliberate encoded-byte changes (exactly two, both versioned)

1. **`environment`'s `HostFault::InjectInterrupt`** widened `u8 → u32` (§C: GIC INTIDs
   exceed 8 bits). Wire payload is now `u32` LE; `BLOB_VERSION` 5 → 6; the three
   `environment` goldens were regenerated. vmm-core keeps x86's range check at both
   ends: `> 255` is refused at stage time (`Unsupported`) and at apply time
   (`ContractViolation`) — never truncated.
2. **`vm-state`'s container header** gained the **arch tag**: `magic | version:u16 |
   arch:u16 | section_count:u16` (8 → 10 bytes), `VM_STATE_VERSION` 1 → 2,
   `ARCH_X86_64 = 1`. Every section payload after the header is byte-identical to v1.

   **Hash consequence, stated plainly:** `Vmm::state_blob`'s `VMST` chunk carries the
   canonical `vm_state` encoding, so any state hashed with snapshot-hashing wired
   (`wire_snapshot_hashing` — opt-in, the snapshot/branch path) shifts by those two
   header bytes. Every default path (M1/M2/corpus/Linux-boot) emits no `VMST` chunk and
   is byte-for-byte unchanged. Nothing pinned the old `VMST` bytes: the snapshot/branch
   determinism tests are all *relative* (same-seed-twice), which is why the suite is
   green without a single hash golden being touched.

   Why the tag is load-bearing and not ceremony: the section *tags* cannot distinguish
   an x86 `REGS` payload from an arm64 one — a foreign blob would decode into x86 fields
   with no length or tag mismatch at all. The tag makes that a loud `UnsupportedArch`.
   A new strictness test pins it: a byte-perfect blob under a foreign arch tag fails
   closed.

## Deviations considered and rejected

- **A superset `Exit` enum** (one enum with every arch's variants) — the ruling's own
  rejected alternative, and it is rejected for a reason I'd restate independently: it is
  the one shape that lets an unhandled ARM variant fall through an x86-written wildcard.
  The two-level enum makes that unrepresentable.
- **Keeping the device fields (`uart`/`lapic`/`legacy`) concrete on `Vmm`** and only
  moving the dispatch. Much cheaper, and it would have compiled — but the engine would
  still *name* x86 devices, so ARM could not reuse `Vmm` and §B's "interrupt fabric =
  vendor" would be a comment rather than a boundary. Went with the associated
  `Vendor::Devices`.
- **Renaming `vmm-core` / splitting it into `engine` + vendor crates.** Explicitly out
  of scope (module split, not crate split); the reserved names activate with the ARM
  window.
- **Neutralizing `VtimeSnapshot::tsc_adjust`** was *not* on the §C list, but the field
  sits in the **engine** and named an x86 MSR, so it became `guest_clock_offset`
  (`visible_tsc` → `guest_clock`). Wire bytes unchanged; the vendor's clock-offset
  register still rides the device blob under its own name.
- **`telemetry`'s `ExitCounts` mirror** kept its `hlt` field name: it is outside the
  surface list and its NDJSON schema pins the name. The backend's counter is `idle`.

## Known limitations / things the integrator should know

- **The trait is designed, NOT frozen.** Doc notes at both `Backend` and `Vendor` say so
  and point at the AA-3 trait-freeze memo. `run_until`'s late-only-stop contract is
  untouched, exactly as instructed.
- ~~`VmmError` still carries `Load`/`LinuxLoad`~~ — **fixed in round 1** (review
  suggestion (a)). The engine's error type no longer enumerates one vendor's loaders: a
  neutral `VendorBoot(Box<dyn Error + Send + Sync>)` carries the cause opaquely (still
  reachable via `source()` / `downcast_ref`), constructed by the vendor's own composition
  root through `VmmError::vendor_boot`. `SnapshotError::Lapic` got the same treatment
  (`DeviceRestore`). Nothing matched on the old typed variants, so the change is free.
- **`ExitReason`/`ExitCounts` remain a flat roster** (common + x86 today). They are
  *observability*, not dispatch — default-deny lives in the two-level `Exit` — so a
  vendor adds variants additively when it lands. Called out in the type's doc.
- **The snapshot-state seam is pinned to x86 — ruled and DEFERRED (round 2, PR #109).**
  `Vendor`'s `build_vm_state` / `validate_restore` / `commit_restore` are typed against the
  concrete `vm_state::VmState`, whose `regs`/`sregs`/`xsave` records are x86-64's. This is
  the **one** place in the trait a second vendor cannot simply implement: `hm-cbt` (the ARM
  skeleton) will have to change that signature — an associated `type Snapshot`, or a
  vendor-parameterized `VmState`.

  The deferral is deliberate, and I agree with it. Designing a vendor-associated snapshot
  type *now* means inventing the abstraction against **zero real second consumers**, which
  is the speculative-generality the pre-build ruling's "spikes gate trust" posture exists to
  prevent; the ARM record set is **AA-6's measured decision**, not something to guess; the
  trait is *designed, not frozen* (AA-3 owns the freeze, and the ruling explicitly accepts
  rework); and step 4 already bought the thing that actually matters — the **format** is
  extensible (arch-tagged TLV container; a foreign record set is rejected loudly as
  `UnsupportedArch`, never reinterpreted), and the storage path is opaque (the engine seals
  encoded bytes and never reads a record).

  **The CI arch gate cannot catch this class**, and it would be dishonest to imply it could:
  the aarch64 leg proves the tree compiles with the x86 vendor `cfg`'d *out*, but no vendor
  exists there to *instantiate* the trait, so a signature only a second implementor could
  refute stays invisible. The structural check is `hm-cbt` itself — the first real second
  vendor. (A stub "dummy vendor" purely to force the check was considered and rejected as
  redundant: `hm-cbt` supplies a real one.) The deferral is stated at the trait seam
  (`vendor/mod.rs`) and scoped into `docs/ARCH-BOUNDARY.md`'s landed-note **and** §D, so the
  additive-sibling promise carries the boundary rather than overstating it.

- **`control-proto::RegsView` is an x86-shaped wire view.** I did not change the wire
  (out of surface); instead the engine fills it through a `Vendor::regs_view` hook, so
  the leak is confined to the vendor and visible for whoever specs the ARM wire. This is
  the one remaining x86 shape above the seam, and it is deliberate + contained.
- `cargo fix` will strip test-module imports that only the vendor tests use — if you
  re-run it, re-check `bringup.rs`/`control.rs` test preludes.

## Gates run (all green, Mac-portable)

- `cargo build` / `cargo nextest run` — **workspace 1691/1691 pass, 29 skipped** (1690
  before step 4; the +1 is the new arch-tag strictness test).
- `cargo clippy --all-features --all-targets -- -D warnings` — clean on **three**
  targets:
  - native (macOS);
  - the mandatory `--target x86_64-unknown-linux-gnu` — it earned its keep twice,
    catching a `cfg(linux)`-only type error in `live_host_plane.rs` (step 1) and the
    Linux-only `boot_selected` composition paths (step 3) that the Mac build never
    sees;
  - **`--target aarch64-unknown-linux-gnu`** (round 1, added to CI's `gates` job) —
    *the* gate for this task's central claim. Nothing else in the tree would notice if
    the arch seam stopped being additive, because every other gate compiles only for
    x86-64. Run it as
    `CARGO_FEATURE_NO_NEON=1 cargo clippy --target aarch64-unknown-linux-gnu --all-features --all-targets -- -D warnings`
    (`CARGO_FEATURE_NO_NEON` steers blake3's build script off its NEON `.c`, which
    would otherwise demand an aarch64 C toolchain; this is a *check* — no aarch64
    artifact is ever linked or run — so that is sound).
- `cargo fmt -- --check`, `cargo deny check` — clean.
- **Miri** (`nightly-2026-06-16`, `-Zmiri-permissive-provenance`) — clean on every crate
  with a Miri job, **re-run on the round-1 code** (the `unsafe` `map_memory` seam moved
  modules when `bringup` went under `vendor::x86`, so it needed re-verifying, not just
  re-asserting): `vmm-core` **308 passed, 0 failed** (its own nightly job, ~68 min
  interpreted), plus `vmm-backend`, `vm-state`, and `environment`. No new `unsafe` was
  introduced; the moved pointer seams kept their `// SAFETY:` comments verbatim, and the
  pointer-retention test still drives the seam through the interpreter.
- **public-api snapshots** regenerated for `vmm-backend`, `vmm-core`, `vm-state`,
  `vtime`, `environment`, `control-proto`, `campaign-runner`. The `vmm-backend` /
  `vmm-core` snapshots are Linux-frozen (they carry `KvmBackend`), so they are generated
  against `--target x86_64-unknown-linux-gnu` — the diff is the reviewable record of
  exactly how the API moved.

## Round 1 (PR #109)

All three P1s were real. Each is verified below by a check that now *fails without* the
fix. The review's two `[suggestion]`s were cheap, so I folded both in rather than
documenting around them — doing so is what lets the ARCH-BOUNDARY landed-note claim be
*exactly* true instead of hedged.

1. **The arch seam did not actually compile additively** (`vmm-backend/src/kvm.rs`).
   The x86 KVM substrate was gated on `target_os = "linux"` alone, but `kvm_bindings`
   exposes a *different* `kvm_regs`/`kvm_sregs` per architecture — so
   `cargo check --target aarch64-unknown-linux-gnu -p vmm-backend` failed outright
   (`no field 'rax' on type '&kvm_regs'`). The keystone's own advertised property was
   untrue, and no gate would have said so. **Fixed:** the x86 substrate (`kvm`,
   `kvm_sys`, `patched_kvm`, `pmu_sys`, the `KvmBackend`/`PatchedKvmBackend`
   re-exports, `work_perf`, the composition roots in `bringup`/`corpus`/`boxrun`, and
   the box-only live tests) is now gated on
   `all(target_os = "linux", target_arch = "x86_64")` — the same seam `hostassert`
   already used. The whole workspace now clippy-cleans for aarch64, and that check is
   a CI gate so it cannot rot.

2. **The engine still enforced x86 interrupt semantics** (`vmm-core/src/control.rs`).
   The generic `ControlServer` hardcoded `vector > 255 → Unsupported` and
   `vector < 16 → reserved` — pure xAPIC. On a GIC both are wrong: INTIDs run well past
   255, and `0..16` are *deliverable* SGIs, not reserved. **Fixed:** the decision moved
   behind the vendor. `Vendor::check_wire_interrupt(vmm, vector) -> Result<(),
   InterruptReject>` replaces the old `can_inject_wire_interrupt` (which could not even
   see the vector); the engine maps the vendor's verdict onto the wire
   (`NoFabric`/`OutOfRange` → `Unsupported`, `Reserved{vector}` →
   `PerturbReservedVector`). x86 behavior is bit-identical — its 25 perturb/interrupt
   tests pass untouched — but the *ranges* now live where the knowledge is.

3. **The §C widening silently killed a mutation arm**
   (`dissonance/environment/src/envcodec.rs`). Widening `InjectInterrupt`'s vector to
   `u32` also widened what the mutation generator *draws*: `rng.next_u64() as u32`. On
   the only backend that exists, every drawn identity above 255 is refused at stage
   time — so `host_fault_from`'s interrupt arm (1 of 4) minted an inadmissible fault
   with probability 1 − 2⁻²⁴. This is on the live search path
   (`explorer/src/engine.rs:414` → `codec.mutate`), so it was a real loss of fuzzing
   power, introduced by me. **Fixed:** the generator draws from `MUTATE_INTID_MASK`
   (8 bits — the identity space the machine under test can accept), with the rule
   stated at the constant: *widening the storage must not widen the generated range*.
   A regression test asserts every generated identity is admissible.

   **Observation, not fixed (pre-existing):** the generator can still mint reserved
   identities (`< 16`, ~6% of that arm), which the control plane rejects at stage time.
   That predates this task and is a separate (small) efficiency question — I did not
   change it, to keep the fix scoped to the regression I caused.

### The two suggestions, both folded in

- **(a) The engine's error type named one vendor's loaders.** *(Bonus: the old
  `#[error("linux load error")]` dropped the cause from `Display` entirely — it was only
  reachable via `source()`. The new `#[error("vendor boot error: {0}")]` interpolates it,
  so a box boot failure now prints WHY, not just that it failed.)* `VmmError::{Load,
  LinuxLoad}` `#[from]`'d `multiboot::LoadError` / `linux_loader::LinuxLoadError`. Which
  loaders a machine has is per-vendor (ARM loads an `Image` + DTB; Multiboot is *deleted*
  for it, not ported — §B), so the engine must not enumerate them. Replaced by a neutral
  `VendorBoot(Box<dyn Error + Send + Sync>)` + `VmmError::vendor_boot(e)`; the typed cause
  survives via `source()` / `downcast_ref`, and **nothing in the tree matched on the old
  variants**, so it cost nothing. `SnapshotError::Lapic` — an engine error naming an x86
  device — became `DeviceRestore` for the same reason.
- **(b) `bringup` was x86-concrete in the engine namespace.** It genuinely *is* a vendor
  boot composition root (it installs the x86 CPU-contract policy, runs the Multiboot v1 /
  bzImage loaders, builds the x86 entry state), and every consumer was already x86-gated —
  so rather than bless it in place, it **moved** to `vendor::x86::bringup`.
  `corpus::boot_patched_payload` moved with it (`vendor::x86::bringup::boot_patched_corpus`).

**Result — the rider, satisfied exactly.** The engine namespace now names **no vendor at
all**: no device, register, exit, loader, or error variant, in any signature, field, or
error type. Verified by sweeping every engine module's production code (pre-`#[cfg(test)]`)
for vendor identifiers — zero hits. The ARCH-BOUNDARY landed-note says exactly that, with
no hedge. (Test modules still name x86, of course — a test needs *a* vendor to run
against.)

## Box-gate readiness (NOT run — the box is under the nested-x86 re-cert lock)

Per the spec I did not touch the determinism box. The keystone's "every box gate passing
unchanged" is the foreman's to verify in the post-re-cert window. What to run and what to
expect:

| Gate | Command | Expected |
|---|---|---|
| Corpus O1 determinism | `cargo test -p vmm-core --test box_corpus --release -- --ignored` | Same-seed `state_hash` identical; **unchanged from main** (no `VMST` chunk on this path) |
| Live Linux boot | `cargo test -p vmm-core --test live_linux_boot --release -- --ignored` | Boots to `GUEST_READY`; unchanged |
| Live determinism | `cargo test -p vmm-core --test live_determinism --release -- --ignored` | Two same-seed runs bit-identical; unchanged |
| Preemption / M1-M2 | `--test live_preemption`, `--test live_m1_m2` | Unchanged |
| Host plane (task 59) | `--test live_host_plane` | Unchanged. **Note:** `HP_VECTOR` is now a `u32` env-parsed value; behavior for any x86 vector (16–255) is identical |
| Snapshot / branch / dirty-remap | `--test live_snapshot_branch`, `--test live_dirty_remap`, `--test live_nonquiescent_snapshot` | Round-trips bit-identical. **`state_hash` on snapshot-hashing-wired paths moves by the two `vm_state` v2 header bytes** — this is the deliberate step-4 change. Compare *relative* (same-seed-twice, and restore-vs-fresh), not against any pre-task-108 absolute hash |
| k3s / Postgres | `--test live_k3s_postgres`, `--test live_postgres` | Unchanged |

### Paths that moved (for anyone re-running a box command from memory)

`vmm_core::bringup::*` → `vmm_core::vendor::x86::bringup::*` (and
`corpus::boot_patched_payload` → `vendor::x86::bringup::boot_patched_corpus`). The box
test *files* and their `--test` names are unchanged, so every command in the table above
is still correct as written; only in-source import paths moved.

**The one thing to watch:** any box artifact that recorded an *absolute* `state_hash`
from a snapshot-hashing-wired run before this task will not match, by exactly the v2
header. That is the version bump doing its job, not a determinism regression — the
same-seed-twice property is what the gates assert and it holds.
