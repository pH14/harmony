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
- **`VmmError` still carries `Load`/`LinuxLoad`** (multiboot / bzImage) variants — engine
  error type, vendor-specific payloads. Honest wart; it dissolves at the crate split, and
  forcing it now would have meant a boxed vendor error for no gate-visible gain.
- **`ExitReason`/`ExitCounts` remain a flat roster** (common + x86 today). They are
  *observability*, not dispatch — default-deny lives in the two-level `Exit` — so a
  vendor adds variants additively when it lands. Called out in the type's doc.
- **`control-proto::RegsView` is an x86-shaped wire view.** I did not change the wire
  (out of surface); instead the engine now fills it through a `Vendor::regs_view` hook,
  so the leak is confined to the vendor and visible for whoever specs the ARM wire.
- `cargo fix` will strip test-module imports that only the vendor tests use — if you
  re-run it, re-check `bringup.rs`/`control.rs` test preludes.

## Gates run (all green, Mac-portable)

- `cargo build` / `cargo nextest run` — **workspace 1691/1691 pass, 29 skipped** (1690
  before step 4; the +1 is the new arch-tag strictness test).
- `cargo clippy --all-features --all-targets -- -D warnings` — clean, **and** the
  mandatory **cross-target** `--target x86_64-unknown-linux-gnu` for
  vmm-backend/vmm-core/campaign-runner/vm-state. That gate earned its keep twice: it
  caught a `cfg(linux)`-only type error in `live_host_plane.rs` (step 1) and the
  Linux-only `boot_selected` composition paths (step 3) that the Mac build never sees.
- `cargo fmt -- --check`, `cargo deny check` — clean.
- **Miri** (`nightly-2026-06-16`, `-Zmiri-permissive-provenance`) on `vmm-backend` and
  `vm-state` — clean. No new `unsafe` was introduced; the moved pointer seams kept their
  `// SAFETY:` comments verbatim.
- **public-api snapshots** regenerated for `vmm-backend`, `vmm-core`, `vm-state`,
  `vtime`, `environment`, `control-proto`, `campaign-runner`. The `vmm-backend` /
  `vmm-core` snapshots are Linux-frozen (they carry `KvmBackend`), so they are generated
  against `--target x86_64-unknown-linux-gnu` — the diff is the reviewable record of
  exactly how the API moved.

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

**The one thing to watch:** any box artifact that recorded an *absolute* `state_hash`
from a snapshot-hashing-wired run before this task will not match, by exactly the v2
header. That is the version bump doing its job, not a determinism regression — the
same-seed-twice property is what the gates assert and it holds.
