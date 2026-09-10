# PC01: a discarded state has a better local continuation

The registered comparison succeeds on its **secondary counterexample criterion**:
6/32 shared tails produce strictly more new surviving boss damage from the exact
discarded state, lower remaining boss HP and no worse player resources at a common
completed command boundary. None supplies the reverse witness. Neither arm
defeats Ridley. All 64 episodes eventually die, so this is no combat success or
fresh-search improvement.

| Fixed-bank outcome | Discarded candidate | Retained incumbent |
| --- | ---: | ---: |
| Tails with the registered directional counterexample | 6 | 0 |
| Tails with any new surviving damage, descriptive | 7 | 4 |
| Living named boss defeats | 0 | 0 |
| Episodes ending in death | 32 | 32 |
| Measured continuation frames | 3,801 | 3,526 |
| Observed episode length, frames | 18–610 | 23–723 |

The earliest qualifying tail is index1. At its first completed command,97 frames
after restoration, the candidate has reduced boss HP137→132 (**5 new points**),
while the incumbent has reduced140→136 (**4 new points**). Both remain alive with
health 79 (7.9 energy), 0 missiles, capacity 20, 1 energy tank, equipment 17 and no
boss defeat. The inherited three-point difference did not count as new damage.
The recorded emulator/context hashes at that exact boundary reproduce under
held-command verification. All qualifying prefixes, both-way comparisons and
nonqualifying tails remain in [the complete analysis](pc01-analysis.json).

There are 69 common completed boundaries, all with uninterrupted qualified root
intervals. All 242 measured boundaries, including clipped terminal commands, pass
held replay checks. Terminal partial commands remain ineligible for the paired
criterion. Actual exposure differs because each arm stops at its own death; no
unseen continuation after death is imputed. No episode reaches the prospective
128-command/8192-frame ceiling. See [the descriptive audit](pc01-descriptive-audit.json).

These are six finite witnesses from **one previously selected complete-state
pair**, not six independent discoveries. Two witnesses begin with the same
button chord for different durations. Position, pose, enemy phase and projectile
state differed at the root. PC01 does not identify scalar HP as the causal feature
or show that preferring lower HP will improve average search utility. Matching
visible boss identity also cannot exclude invisible same-key reloads; the
quantity is observed HP loss, not proven lifetime damage.

## What the result permits

For this measured local-progress criterion, the retained state does not dominate
the discarded state over all shared finite command prefixes. Together with RR02's
direct competition capture, this supplies the missing continuation counterexample
for issue281. It justifies assessing a minimal explicit retention representation
that can preserve such a complete-state distinction at bounded, matched memory.

First compare the existing retention mechanisms and write a source-level
counterexample for the proposed change, including unknown boss context, opposing
resource advantages and ordinary-policy compatibility. Do not infer that HP
buckets, scalar HP priority or an extra archive member is already the correct
intervention. Any chosen diagnostic or policy panel needs a separate decision
and registration. The unchanged action/continuation nominees that failed earlier
gates remain closed; no automatic rerun, parameter sweep or fresh-search allocation
follows. Matched development, independent confirmation, both-game evaluation and
untouched boss/Wily validation remain outstanding.

## Cost, identity and closure

PC01 uses source 613fdc2a and binary
`3a546773abc4b35f8264d5fb9f0af5b4f68e72c5a4315bc573fe347fcb626284`.
Its exact roots were exported without emulation from frozen RR02 record 692 after
checking the complete 3770-record capture and all four payload hashes. The
[registration](pc01-registration.json), [design](pc01-design.md), full suffix bank
and scorer were published before dispatch. No HP/resource edit or foreign-state
identity rewrite occurred.

ms02's native process runs from 19:44:11.761987 to 19:44:12.364036 UTC on September 10:
**0.602 seconds wall, 0.556 CPU seconds, 18,028 KiB peak RSS**. Service counters record
0.579 CPU seconds and14,725,120 bytes peak cgroup memory. Its single owned target
measures **15,583 total frames: 929 setup + 7,327 diagnostics + 7,327 verification**.
No new PC01 counter is missing; older ledger gaps remain unchanged. The 1.1M
ceiling is not charged as actual work. Resumed totals are 2,982,484,247 admitted
search and 68,383,589 known auxiliary frames in [the closed ledger](ledger-after-pc01.json).

The completed process exited successfully. Its unit was briefly kept after exit
solely to read service counters, then explicitly stopped; MainPID 0/inactive/dead
receipts and the journal are preserved. The allocation is closed and its unused
window/budget released. msr1 was not accessed.

Ten compressed native evidence files total 31,151 bytes and reproduce 222,185 raw
bytes. [The manifest](pc01-evidence-manifest.json) pins both forms. Verify their
hashes, complete 64-episode order, all cost increments, shared input prefixes,
frozen score, service closure and ledger chain with no emulator:

```sh
python3 benchmarks/search/continuation-reassessment/verify_pc01.py
python3 -m unittest discover -s benchmarks/search/continuation-reassessment -p test_pc01.py -v
```

The new callers pass 14 targeted Rust tests with the capture's motion/context
features and 13 in the default build, plus strict Clippy; four scorer
counterexamples pass. Default-build checking caught a fixture that incorrectly
expected a differently featured snapshot decoder to reproduce the same hash.
Its expectation now explicitly requires rejection under incompatible features;
this test-only correction does not alter the frozen native binary or result.
