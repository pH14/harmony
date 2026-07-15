# tasks/112 — ARM backend skeleton spec (hm-cbt) — SPEC-DRAFTING TASK

**This file is a stub.** The task is to REPLACE it with the full implementation spec
for the ARM backend skeleton, then open a **docs PR** (branch stays this task's own;
no crate code in this task).

**Bead:** `hm-cbt` (P2, the D-list pre-build lane) · **Deliverable:** the finished
`tasks/112-arm-backend-skeleton.md` implementation spec, PR'd for foreman review
(light tier). The implementation itself is a LATER task spawned from your spec.

## Sources to synthesize (read all before writing)

- `docs/ARCH-BOUNDARY.md` — the Arch trait / two-level `Exit<A>` / engine-vendor split
  (merged, PR #109) and the **§Pre-build ruling** (build-first; spikes gate trust, not
  construction; rework risk on the trait accepted).
- The `hm-cbt` bead (`bd show hm-cbt`) — the ruled D-list scope: KVM/arm64 ioctl
  backend against the documented kernel ABI; arm64 Image-header + DTB boot path
  (PSTATE, x0=dtb entry); GICv3 + generic-timer models in the ruled pure
  now_vns-in/deadlines-out shape; the ARM vendor personality behind the `hm-b5n` seam.
- `docs/ARM-ALTRA.md` — the AA-0..AA-6 spike program the skeleton must NOT front-run:
  every measured constant (skid_margin, event density, count offsets) is a spike
  deliverable; never invent numbers; SimCpu stays x86-parameterized until the AA
  constants pack exists.
- `docs/PARAVIRT-CLOCK.md` §ARM (AA-5 closure) — the clock page the guest side will
  eventually use; the skeleton only reserves the seam.
- The probe verdict on `hm-8l3` / the `hm-cbt` comment — **no local KVM ioctl dev loop
  on this Mac (REFUSE, host lacks nested virt); QEMU TCG is the local oracle** for the
  ioctl/boot path until the Altra racks (`hm-7pb`). The spec's gate section must be
  TCG-first with the KVM gates marked box-arrival.
- `consonance/vmm-backend/src/arch/x86/` + `consonance/vmm-core/src/vendor/x86/` — the
  shapes the ARM analogues mirror (value types, Vendor trait impl, dispatch).

## Constraints the spec must encode (ruled, not negotiable)

1. **Additive crates only** — zero edits to the neutral spine; a generic `<A: Arch>`
   parameter appearing in a dissonance crate is a review-blocking smell.
2. The trait is **designed-NOT-frozen**: AA-3's trait-freeze memo may force rework of
   the `run_until_overflow` late-only-stop contract — say so explicitly, with the
   rework-accepted note from the pre-build ruling.
3. No invented constants anywhere (see above); anything measured is a named TODO bound
   to its AA stage.
4. Gates: portable tests + clippy (native aarch64 Mac IS the target arch for pure
   logic), TCG smoke for boot/ioctl shapes, Miri for any unsafe with allocation-backed
   seams (PR #99 precedent; the PR #108 payload carve-out pattern where genuinely
   uninterpretable). Box/KVM gates are arrival-day items edged to `hm-7pb`.
5. Milestone the work so the keystone-facing seam (Vendor impl compiling against the
   engine) lands before device models; each milestone independently green.

## Definition of done (for THIS drafting task)

- `tasks/112-arm-backend-skeleton.md` fully replaced (goal, scope, non-goals,
  milestones, gates, deliverables, the constraint list above encoded).
- Opened as a docs PR with a review-grounding description (what was synthesized from
  where; any judgment calls flagged for the foreman).
- No crate code, no other files touched.
