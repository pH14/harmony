# Bug 1 (fault-timing) box campaign — data + recipe

- `calibration.json` — box-calibrated manifest: bug 1 gpa set to the real ledger
  **canary** gpa on the logged image (`0x7fbe2000` = 2143166464), window `[1500,1520]`
  (~offset 500 past the sealed base, inside the supervised loop). Bugs 2/3 keep toy
  values (calibrated later).
- Runner: `conductor bench-campaign --bug 1 --config signal|baseline --seed S
  --max-branches 512 --deadline-delta 2000000 --calibration calibration.json
  --initramfs initramfs-campaign.cpio.gz --ready-marker CAMPAIGN_READY --out <json>`
  on the box (worktree ~/harmony-t69m2), pinned via box-window.sh, 3-wide.

## OPEN BLOCKER (2026-07-06)
A calibration run at the real canary gpa `0x7fbe2000` fails:
`control error: verb not supported by this backend`. The default-manifest de-risk at
gpa `0x3000` ran fine (produced cells, 0 finds), so this is gpa-specific — the patched
KVM's CorruptMemory enforcement rejects the high gpa (near the top of the 2 GiB RAM).
Under investigation: likely a gpa-range limit in the fault-arming path, or the ledger
page sits in a region the backend cannot corrupt. Fix before the ≥20-seed runs.
