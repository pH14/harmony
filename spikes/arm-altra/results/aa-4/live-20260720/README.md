<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-4 W^X + rescan-on-exec — live proof on N1, 2026-07-20 (hm-rfz)

The stage-2 execute-guard KVM patch (0002; cap `KVM_CAP_ARM_STAGE2_EXEC_GUARD=246`,
exit 43) proven on real silicon. Host `6.18.35-aa4guard` (patch 0001 + 0002), build-id
`ac576f87…`, core 60 isolated. See `MANIFEST.txt` for hashes.

| Gate | Payload | Result |
|------|---------|--------|
| Pre-execute rejection of a hazardous page | `llsc-atomics` (2 planted exclusives) | **PASS** (`reject.*`) — 1 scan → 1 rejection, PC unchanged |
| Selective approval (guard is not blanket-reject) | `lse-atomics` (clean LSE) | **PASS** (`negctl.*`) — approved + ran to an MMIO exit |
| Write-before-modification + exact-page rescan + stale-generation | `aa4-self-modify` | **PASS** (`write.*`) |

The write proof exercises the full state machine live: page approved (gen 4) → guest
writes its own executable page → guard **exits before the write and revokes execute**
(`write_revocations=1`) → modified page **rescanned at a newer generation** (5) →
a replayed **stale-generation** approval **rejected with EINVAL**.

## Finding: guard scans whole executable pages (page-align code/data)

The write proof initially failed: `aa4-self-modify`'s rodata shared the text-tail page,
and a rodata word matched an exclusive encoding, so the guard rejected a page the
ELF-*section* scanner calls clean. Fixed by page-aligning payload `.rodata` (linker.ld) —
the W^X layout the guard requires (it sees pages, not sections), mirroring the guest
kernel's `STRICT_KERNEL_RWX`. This is a real W^X contract implication for any guest run
under the guard.

## Remaining (recorded, not run this window)
Notifier-replacement invalidation, two-vCPU concurrent scan/write race, and the
backing-replacement move (portable predicate committed at `sys.rs`; no live command yet).

## AA-4 concurrency: notifier-replacement — PASS (`notifier.out`)

`aa4-guard-notifier` (payload `aa4-reexec`: execute a clean page, marker, execute the SAME
unchanged page again). A memslot update's mmu-notifier invalidation must force the guard to
re-scan an already-approved page — proven with a self-verifying negative control:

- **control** (no memslot op): the target page's approval is REUSED on the second execute —
  `control_scans=1`, generation stable at 4.
- **notifier** (delete + re-add slot 0, same backing, at the marker): the second execute
  RE-SCANS — `notifier_scans=2`, generation **4 → 7**.

Because the page is never modified, the second scan is attributable only to the memslot
update's invalidation. `notifier_forced_rescan=true` and `control_reused_approval=true`.
Target-page-specific `audit.exec_scans` isolates the effect (a memslot replace re-scans every
executable page; only the audited target measures the approval invalidation on it).

## AA-4 concurrency: backing-replacement — PASS (`backing.out`)

`aa4-guard-backing` — same `aa4-reexec` payload and control, but the interposed op moves
slot 0 to a **distinct** anonymous backing whose contents are byte-identical (fresh mmap +
full copy, old backing unmapped). The guard must re-scan the target on the second execute
even though the page content is unchanged — proving the approval is keyed to the **mapping**,
not to a content hash:

- **control** (no move): `control_scans=1`, generation stable at 4 (approval reused).
- **backing** (move to distinct identical backing): `backing_scans=2`, generation **4 → 7**.

`backing_forced_rescan=true`, `control_reused_approval=true`.

## AA-4 concurrency: two-vCPU scan/write race — PASS (`race.out`)

`aa4-guard-race`. Two MMU-off vCPUs on a guarded VM built WITHOUT a vGIC (its `CTRL_INIT`
would finalise the vCPU count to one — KVM returned EBUSY on a second vCPU otherwise; the
race vCPUs use no interrupt controller). vCPU 0's PC is the target page (its fetch freezes it
for a scan); the writer vCPU (entered directly, MMU off) stores to the target GPA — the store
transits stage-2, so the guard sees it despite stage-1 being off. Deterministic,
single-threaded interleaving:

- **race** (target left frozen for vCPU 0's scan): the writer's store is **BLOCKED**
  (`GuardBlocked` at 0x40082000).
- **control** (vCPU 0's scan approved first): the same store instead revokes execute
  (`GuardWrite` at 0x40082000) — not blocked.

`blocked_when_frozen=true`, `write_when_approved=true`: the guard correctly blocks a write
behind a concurrent pending scan. **All three AA-4 concurrency gates now hold on N1.**
