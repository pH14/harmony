# Source falsifier and bounded replay design

This is an offline source result and design, not a native replay registration.
It spends no emulator frames. msr1 is reassigned; any later native work uses
ms02 only. The closed CC01 decision and ledger are unchanged.

## The proposed earlier suppression does not exist

`MetroidGame::execute_suffix` makes a candidate for every living, nonfailed
action endpoint. `CoordinatorCore::admit_job` submits every viable candidate
to `Archive::insert_after_observed`, passing the last rejected boundary's
`previous_key` as key-completion context. The archive uses that value only in
`key.complete(parent_ctx)`. `MetroidArchiveKey::complete` returns `self` and its
lineage is `()`. There is no test that discards a candidate because its key
matches the preceding boundary.

`existing_input_id(parent_id, suffix)` is the relevant early return: an exact
retained input produces a duplicate disposition without a competition. An
ordinary different-input candidate in a full slot reaches the existing
retention observer before either rejection or replacement. An empty slot
admits a new representative without a competition. The observer names members
of the current slot; inactive cached entries are not competitors. A current
competitor with no cached snapshot is reported as `None`, not substituted with
an old snapshot or reconstructed implicitly.

The archive implementation before its test module is byte-identical to AP01's
source commit `02798b443a448e57bc41e1f11307fd50092d20f7`, SHA-256
`5b09d43f93cdcd57fe7897dc60bb01fbe655ec8e90385ce8afbbde771e1a815b`.
The Metroid archive implementation before its test module is also unchanged,
SHA-256 `ba2d026ec6d50eb3c4c75d42b9f92173b9d38bdd3bff750714fb3378349c3e36`.
These are hashes of file bytes before the first newline-delimited
`#[cfg(test)]` marker. Later retry support changed the campaign/rollout code;
the whole program is **not** claimed byte-identical to AP01. Native replay must
still qualify the actual source/build and every original result and decision.

The executable source falsifier is
`same_key_observation_distinguishes_duplicates_current_slots_and_missing_snapshots`
in `dissonance/searcher/src/search/archive.rs`. Two same-key rejected boundaries
with different inputs both invoke the observer, even when `previous_key` is
present. An exact retained input does not. A reserved job keeps the replaced
incumbent snapshot cached, but the next observation names its replacement. A
planted missing current snapshot stays unavailable. Toy snapshot values are
opaque to the rule; these are not native boss measurements.

Run the source checks from the repository root:

```sh
cargo test --manifest-path dissonance/searcher/Cargo.toml
cargo clippy --manifest-path dissonance/searcher/Cargo.toml --all-targets --all-features -- -D warnings
```

## Minimal diagnostic, if separately qualified

The query is whether the ordinary local rule rejects a living classified
candidate whose snapshot-local HP is lower than its current same-boss
representative's HP. Record resources and classification availability on both
sides. A positive row is representation-loss evidence at that boundary, not a
proof of better continuations, global coverage, lifetime damage, or defeat.
Issue #281 still requires a paired continuation counterexample before a policy
change. Unknown context or an unavailable snapshot cannot become a negative.

Reuse `replay_campaign_checkpointed`, `Reporting::observe_retention`, and the
existing paused restore/context reader. Do not add an earlier generic hook.
The smallest implementation path is a bounded, opt-in workload capture of
competition snapshots and inputs during exact replay, followed by independent
paused context reads. Keeping emulator inspection outside the callback avoids
sharing a target across coordinator/worker threads and avoids touching the
replayer's state. Stream records and policy state must remain untouched.

The exact saved pilot provides useful finite completeness checks:

| Quantity | Expected value |
| --- | ---: |
| Recorded jobs | 2,548 |
| Admitted job frames | 250,267 |
| Rejected decisions | 3,295 |
| Retained decisions after the root | 1,031 |
| Exact-input duplicate decisions | 1 |
| Recorded ordinary replacements | 475 |
| Expected full-slot observations | 3,770 |

The last count is 3,295 rejections plus 475 replacements under AP01's ordinary
one-member slot rule. Its 1,032 total retained entries include the supplied
root; the other 556 admissions open slots. This is an expected audit count,
not an observation that any of those states has lower HP. Require replay to
reproduce every job's physical frame count, semantic result SHA and ordered
decisions. Capture all competitions or report incomplete evidence; do not
extrapolate a reservoir sample to the pilot's discarded states. Include stable
incumbent IDs, execution/competition order, actual local disposition, root
identity, and separately hashed local inputs/snapshots. Never call a
challenge-local input a genesis witness.

Before any emulator call, check asset hashes, original stream/origin hashes,
complete input counts, supported source features, and prospective output and
process limits. The original stream explicitly names an ARM core. The current
x86 core cannot pass that backend identity unchanged. Any qualified cross-build
derivation must retain the immutable original, list the exact identity-field
changes, and keep every job/skip record identical. Do not pass the ARM hash to
an x86 runtime or loosen the production replay identity check. First qualify
the exact root's restore, unchanged paused bytes/context and action semantics;
stop on any incompatibility instead of editing snapshots or digests to fit.

A later native registration must separately bind qualification and diagnostic
stages, source/build/features, both original and runtime assets, frame/setup
accounting, wall/process deadlines, memory/output ceilings, refusal behavior
and failure artifacts. Favor one complete short replay and one paused target
over repeated genesis replays or another search. There is no allocated native
stage yet, no new action bank or seed, and no fresh-search panel earned here.

## Capture implementation checkpoint

`workloads/nes/src/metroid/retention_capture.rs` now implements the bounded
workload capture and a streaming reader, attached through the existing
`MetroidGame` retention callback. It does not inspect or restore an emulator.
The file names the actual stream/origin hashes and the snapshot/key encodings;
an incompatible decoder fails before rows. Storage is capped at 5,000
competitions, 4,096 actions per input, 256 KiB per framed record and 512 MiB per
file. Exact-output completeness requires a matching footer and EOF. Capture or
read errors are sticky; missing incumbent snapshots remain explicit.

Source tests exercise actual record round trips, missing state, output refusal,
record/input/encoding/output caps, interrupted and extra-byte files, incompatible
encodings, and unchanged policy identity without constructing a target. The
149-test NES library suite with AP01's motion/context features and strict
all-target Clippy pass. This proves the capture plumbing's checked behavior,
not cross-build replay compatibility or a scientific outcome.

The standalone `metroid-retention-replay` caller is now implemented with four
source tests and strict Clippy. Qualification uses exactly the first four
original jobs (865 frames), after a paused restore of the pinned root. Full
inspection requires a successful qualification bound to the executable and all
inputs, including the original final checkpoint. The replayer checks each job's
frames, result digest and decisions; inspection also requires byte-identical
final raw checkpoint bytes before accepting cross-build observations. The
selected job bytes are never reserialized; only the explicitly declared backend
core-hash field is rebound in the derived header. Missing state and HP255 remain
unavailable. No fresh job, action draw, seed or supplied root is selected.

The prospective qualification/inspection ceilings are 500,000 and 750,000
physical frames, respectively. These cover recorded work plus the first
divergent job's possible 4,096-action origin reconstruction, six suffix actions
at up to 120 frames, and two 929-frame setup allowances. They are bounds, not
measured expenditure. Successful known work is the verified replay frames plus
the inspector's measured 929-frame setup; the generic replay constructor's
counter is unexposed and remains an explicit gap. The caller has a hard process
watchdog capped at 300 seconds; separate external memory, output and service
caps are still necessary. Native build, prospective registration, qualification
and the actual AP01 observation remain required. No emulator work has been
allocated by the source implementation or tests.
