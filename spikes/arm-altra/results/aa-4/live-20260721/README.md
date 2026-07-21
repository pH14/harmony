<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-4 W^X + rescan-on-exec — full re-cert on the rebuilt aa4guard, N1, 2026-07-21 (hm-rfz)

The AA-4 stage-2 execute-guard apparatus, **re-run LIVE on the rebuilt `6.18.35-aa4guard`
host** after the box was account-wiped — so the W^X evidence is recomputable on the current
kernel, not stale pre-wipe artifacts (hm-rfz / hm-efs). Cap
`KVM_CAP_ARM_STAGE2_EXEC_GUARD=246` / exit 43; core 60 isolated. `Machine::new_guarded`
attests the patched cap — a stock kernel cannot construct a guarded VM, so none of these
gates can pass on stock (the PR-98 structural-soundness bar). See `MANIFEST.txt` for the
build-id and payload pins (which bit-reproduce the 2026-07-20 payloads).

## Mechanism gates — all PASS

| Gate | Payload | Result |
|------|---------|--------|
| Pre-execute rejection of a planted exclusive | `llsc-atomics` (2 exclusives) | **PASS** (`reject.out`) |
| **Section-aware anti-weakening** (forged data-labelled hazard) | `aa4-mislabel-evasion` | **PASS** (`mislabel-evasion.out`) — static scan passes, guard rejects at entry |
| Write-before-modification + exact rescan + stale-gen `EINVAL` | `aa4-self-modify` | **PASS** (`write.out`) |
| Memslot-notifier-replacement forced rescan | `aa4-reexec` | **PASS** (`notifier.out`, gen 4→7) |
| Distinct-backing-move forced rescan (keyed to mapping, not content) | `aa4-reexec` | **PASS** (`backing.out`, gen 4→7) |
| Two-vCPU scan/write race (write behind a pending scan is blocked) | `aa4-reexec` | **PASS** (`race.out`, `blocked_when_frozen=true`) |

**Selectivity** (the guard approves clean pages, it is not a blanket-reject) is proven inside
these gates: `write`/`notifier`/`backing` each *approve* the target page (generation 4) before
the write-revoke or the forced rescan (generation 7) — an approval that a blanket-reject guard
could never produce. Each concurrency gate also carries a self-verifying negative control.

This completes the AA-4 W^X apparatus for PR #135 (hm-rfz): the mechanism is proven live on
the rebuilt host, superseding the pre-wipe evidence.

## Disclosed finding — AA-5 guest booted *under* the guard (new, untested combination)

Beyond the 2026-07-20 scope, I also booted the **full AA-5(c) Linux guest** under the
execute-guard (`linux-boot --stage2-exec-guard`). The guard **rejected** page `0x4041d000`
(generation 56) with **7 exclusives at a regular 0x54-byte stride**
(`finding-guest-under-guard.out`). That stride is the signature of the kernel's
`.altinstr_replacement` / `.altinstructions` alternative-fallback **data**, which shares the
executable `.init` `PT_LOAD` and is mapped executable during early boot.

This is the **runtime analog of the static `.rodata` false-positive** the section-aware
scanner now excludes (hm-jth / hm-7o68-F3): the *static* scan correctly treats that data as
non-code, but the *raw page-granular* runtime guard — which by design does not trust section
metadata — flags it when the `.init` region is executable. So the AA-5 guest cannot currently
boot **under** the AA-4 guard.

**Scope:** AA-4 (bare planted proofs on aa4guard) and AA-5(c) (the Linux guest on aa3preempt)
were always *separate* gates; running them on one guest is a new combination, not a PR-#135
deliverable. The mechanism gates above are unaffected. Closing the combination — teaching the
runtime guard the same `.init`-data section-awareness, or laying out the guest so no
data-bearing page is executable — is filed as a follow-up (see the bead), not an AA-4 gate
failure.
