# AA-3 re-cert evidence — 2026-07-21/22 box window (task 137, hm-idb)

**These runs are DIAGNOSTIC mechanism re-verification on the current box (fresh-pin basis).
They are NOT an AA-3 GO certification.** The AA-3 GO disposition remains **PARKED** pending
Paul's ruling on the pin basis (the certified payload bytes are non-reproducible post-wipe;
see `STATUS.md`). Foreman directive 2026-07-22: keep the box saturated; scale the smoke's
fresh-pin diagnostic basis to the ≥10⁶ acceptance scale as extended mechanism evidence.

- `STATUS.md` — verified facts, the pin-basis deviation, options A/B/C + recommendation.
- `smoke/` — the 3500-record diagnostic smoke: `floor-check` PASS (21/21).
- `recert-full.sh` — the ≥10⁶ diagnostic runbook (faithful to the certified
  `host/aa3-exact-shard.sh` semantics at 48d519f; adapted only for the post-wipe box:
  current kernel pins, box-local regenerated payload pins, on-silicon environment, sudo/KVM).
- `inputs/` — on-silicon environment + host-kernel + box-local (regenerated) payload pins.
- `full/` — the ≥10⁶ diagnostic verdicts (comparator + aggregate floor-check), added on landing.

Basis (both smoke and full): payloads rebuilt from the git-verified byte-identical certified
source (`48d519f`); pins regenerated because a fresh toolchain (reinstalled 2026-07-21) + build
path yield different bytes; `count-exactness` is the payloads' semantic gate (independent of the
byte pin). Kernel: `6.18.35-aa3preempt`, build-id `899b921e…` (running == on-disk vmlinux).
