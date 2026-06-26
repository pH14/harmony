# Task 44 — workload-agnostic `load_image` in consonance; move the bzImage transform to `harmony-linux`

> **FOLLOW-ON to task 43 (audit finding F1) · behavioral + determinism-sensitive · DO NOT
> AUTO-SPAWN until task 43 lands.** Depends on `harmony-linux/` existing and on the design ruling
> in `docs/DISSONANCE.md` (consonance runs an *opaque* guest).

Read `tasks/00-CONVENTIONS.md` and `docs/CONSONANCE-WORKLOAD-AUDIT.md` (F1) first.

## Why

`consonance/vmm-core/src/linux_loader.rs` (~1239 LOC) parses the Linux `setup_header` and lays down
`boot_params` / page tables / GDT — it is wired into `bringup.rs` (image autodetect + `boot_linux`),
`lib.rs` (`pub mod linux_loader`), and `vmm.rs` (`LinuxLoad` error variant). **Turning a Linux
kernel image into initial machine state is a `harmony-linux` concern, not a substrate concern.** A
deterministic VM that knows the bzImage format has leaked the guest tier into the substrate (audit
finding F1).

## What to do

Split the loader at a workload-agnostic seam:

1. **consonance owns the dumb half** — a primitive that takes already-resolved bytes + entry state
   and never parses a guest format:
   ```rust
   /// Place opaque segments into guest physical memory and set the initial vCPU state.
   pub struct ImageSegment { pub gpa: u64, pub bytes: Vec<u8> } // or a borrowed slice
   pub struct EntryState { pub rip: u64, pub regs: InitialRegs, /* page-tables/GDT as segments */ }
   pub fn load_image(mem: &mut GuestMem, segments: &[ImageSegment], entry: &EntryState)
       -> Result<(), LoadError>;
   ```
2. **`harmony-linux` owns the Linux half** — the bzImage `setup_header` parse + `boot_params` +
   page-table/GDT construction that *produces* `(segments, entry_state)` and calls `load_image`.
3. Re-point `bringup.rs` / `vmm.rs`: the Linux-image autodetect and `LinuxLoad` error move out of
   the substrate; consonance exposes only `load_image` + a generic `LoadError`.

## Determinism (the hard constraint)

Boot must stay **bit-identical** — the committed `harmony-linux/linux/MANIFEST.sha256` artifacts and
every `live_*` box gate must produce the same `state_hash` / `GUEST_READY` before and after the
split. The seam is a pure refactor of *where* the bytes are computed, not *what* bytes land in
memory. Prove it: run the determinism gate twice across the split (same digests).

## Acceptance gates

Standard suite (`build`/`nextest`/`clippy -D warnings`/`fmt`/`deny`/miri where `unsafe` is touched)
on the changed crates, plus: the `harmony-linux` live Linux-boot + Postgres box gates pass with
identical digests; `git grep -n 'linux_loader\|bzImage\|setup_header\|boot_params' consonance/`
returns **zero** (the Linux knowledge is gone from the substrate).

## Non-goals

Changing boot behavior; supporting a non-Linux loader now (the point is only that consonance *could*
host one); the live-test relocation (audit finding F2 — separate, optional).
