# Coverage recovery (GitHub issue #69)

The CI compile break (Step::SdkStop, fixed in #68) had failed the coverage/mutants/nextest
jobs *before they could measure* since #63 — so tasks 73/78/69 merged without their coverage
gates actually running. #68 restored the region floor to 93.27% by pinning materialize.rs,
but the underlying per-file coverage is thin. This branch ratchets it back up.

Targets (per #69):
- dissonance/conductor/src/record.rs   ~67.5%
- dissonance/conductor/src/campaign.rs ~82.7%
- dissonance/conductor/src/lib.rs      ~71.5%
- dissonance/conductor/src/main.rs     ~0%  (cfg(linux) live-drive bin — box-only)

Goal: the region floor should RATCHET UP past 93.27%, not just sit at it.

## Result

Measured with CI's exact command on the determinism box (Linux, so
`main.rs`'s `cfg(target_os = "linux")` code compiles and counts — a Mac-local
run understates `main.rs` since `mod boxrun` doesn't even compile there):

| File | Before | After |
|---|---|---|
| `record.rs` | 68.00% | 87.66% |
| `campaign.rs` | 82.72% | 91.27% |
| `lib.rs` | 73.03% | 90.40% |
| `main.rs` | 0.00% | 61.14% (the remainder is `mod boxrun` — genuinely box-only; see `dissonance/conductor/IMPLEMENTATION.md` for the ignore-filename-regex proposal) |
| **Workspace region floor** | **93.31%** | **94.25%** |

All added tests assert real behavior (a table's exact bytes, a gate's exact
failure message, an `ExitCode`) — see
`dissonance/conductor/IMPLEMENTATION.md`'s "Coverage recovery" section for
the full breakdown, what was added, and the known residual gap
(`record.rs`'s `seal_base` retry-loop error arms, which need real
vmm-core/backend state to trigger and are not directly unit-tested).
