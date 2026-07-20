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
