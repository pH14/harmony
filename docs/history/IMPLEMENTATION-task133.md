<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Task 133 — SelectorV1 exploit path: implementation write-up (hm-0paj)

**Origin:** PR #134 tribunal finding **F3**, ruled **Option B (deferred follow-up)** by Paul 2026-07-21.
This task is that follow-up. Frontier task; surface = `dissonance/explorer/` (the fix) +
`dissonance/campaign-runner/` (portable regression + box smoke) + `.github/workflows/nightly.yml`
(Miri floor ratchet).

## 1. The bug (F3)

`FrontierEntry.env` stored `rollout.genesis_env` — the genesis-complete reproducer positioned at the
rollout **terminal** (`recorded_env` folded at the last stop) — while `exemplar.cut` names the earlier
**seal**. On the first EXPLOIT step, `SocketMachine` keys the recorded child delta from the seal it
branched off, so `run_rollout`'s `compose(base_env, recorded_env)` sees a delta whose `base_offset` is
the seal against a base whose `pos` is the terminal — the adjacency check `d.base_offset == b.pos`
(`SpecEnvCodec::compose`, `adapter.rs`) fails and the campaign aborts `NonAdjacentChain` on the first
exploit.

Latent in every shipped gate because:
- the box game config is **PureRandom** (`GenesisSelector::choose → None`), which never exploits, so
  the exploit compose never ran on real KVM;
- the portable toys (`GameToyMachine`, internal `ScriptedMachine`) **echo** their branch env from
  `recorded_env`, so `recorded_env.base_offset` is always the `mutate` output's `base.pos` and the
  adjacency check holds trivially. The existing portable SelectorV1 tests passed on that alignment.

## 2. The fix (Option B: the seal owns the coordinate)

`dissonance/explorer/src/campaign.rs`. At seal materialization (`step`, barrier 2), build the
**seal-consistent** genesis-complete env: fold the base's genesis-complete env with the branch-local
delta the machine recorded **at the seal** (`materialize_candidate` now also returns that delta via a
`recorded_env()` taken right after the seal snapshot). Store it in both the admitted `FrontierEntry.env`
**and** the seal evidence's `env` → `CellAssignment.env` (so a restart rebuild reconstructs a
seal-consistent entry too). Mirrors the task-132 `DeclaredMachine` "one owner for a coordinate" lesson.
The adjacency check is **untouched** — `NonAdjacentChain` stays fail-closed for genuinely non-adjacent
chains.

**Hash-neutral for the shipped path.** The change is a *provable no-op* wherever `recorded_env` echoes
the branch env — the internal `ScriptedMachine` and the plain `GameToyMachine` both return
`self.current`/`self.recorded` regardless of when `recorded_env` is called, so
`seal_genesis_env == rollout.genesis_env` for them, byte-for-byte. Every existing explorer (137) and
campaign-runner (163) test is byte-identical before/after. The change differs only where `recorded_env`
is seal-aware (the real `SocketMachine` and the test-only `SealAnchor`), and even there it changes only
the stored reproducer's coordinate metadata — never the guest `state_hash` (`machine.hash()` is over
guest state, independent of the env blob). Confirmed on the box: an unmodified PureRandom campaign's
record→replay `state_hash` sequence is bit-identical (§4).

## 3. Portable regression (real production codec, portable)

`dissonance/campaign-runner/src/gamecampaign.rs`:
- **`SealAnchor<M>`** — a `Machine` adapter that re-anchors `recorded_env` at the seal
  (`base_offset = branch seal Moment`, `pos = current stop`), exactly as the real `SocketMachine` does.
  Behavior-neutral (`GameToyMachine`'s trajectory is a pure function of the env *spec*, not the blob
  *coordinate*), so it reproduces the box coordinate portably through the real `SpecEnvCodec`/`QuietCodec`
  + `run_game_campaign`.
- **`selector_v1_exploit_composes_under_seal_anchored_coordinate`** — a SelectorV1 exploit step composes
  with seal ≠ terminal and the campaign replays bit-identically. **Validated non-vacuous:** reverting the
  fix makes this test fail with `NonAdjacentChain("branch-local delta's origin does not meet the base's
  capture point")` on the first exploit.
- **`non_adjacent_delta_still_aborts`** — the negative test: a genuinely non-adjacent delta (gap *and*
  overlap) is still refused `NonAdjacentChain`; the exactly-adjacent pair composes.

## 4. Box smoke (real KVM) — EVIDENCE

Box `ssh hetzner` (Intel i9-9900K, Linux 6.12.90, patched KVM), leased core 2 via
`scripts/box-window.sh acquire t133`, pinned `taskset -c 2`. Binary built from branch head `3fe2c50`
(`campaign-runner` release, sha256 `7734fac3…befb7d6b5`). Guest: `bzImage` + `initramfs-game.cpio.gz`
(task-86 SMB image, ROM `GAME_ROM_SHA256 0b3d9e1f…a3b66dea`). Config: `--explore-period 3
--deadline-delta 2000000000 --campaign-seed 7`. Full transcripts retained on the box at
`/root/t133-evidence/` (`selv1_2b.{out,json}`, `pure_8b.{out,json}`, `selv1_8b_cascade.out`).

### 4a. F3 is cleared on real KVM — SelectorV1 exploit composes + replays bit-identically ✅

`campaign-runner game box --config selector-v1 --max-branches 2 --repeat 2 …` → **exit 0**:

```
game box: campaign 1/2 (config=SelectorV1, 2 branches, 2000000000 ns per rollout; boot-to-ready 795 ms)…
game box: campaign 2/2 (config=SelectorV1, 2 branches, …; boot-to-ready 760 ms)…
game box: repetition 2/2 bit-identical to the first.
game box work evidence: 2 branches, weakest rollout 1999998649 ns of V-time / 5246 COMPLETED frames.
```

Branch 0 is the genesis EXPLORE; branch 1 is the EXPLOIT (`ExploreExploitSelector`, period 3, non-empty
frontier ⇒ exploit by construction). Branch 0 `state_hash d9ba2849…d2991fd4`. The exploit step's
`run_rollout` compose — the exact site that aborted `NonAdjacentChain` pre-fix — now succeeds; the
campaign completes and **replays bit-identically** (record→replay determinism, `--repeat 2`). Per-branch
`state_hash`es retained in `selv1_2b.json` (the checker input). Pre-fix, this campaign aborts
`NonAdjacentChain` on the first exploit — the whole reason the SelectorV1 exploit path had never run on
real KVM.

That the F3 compose works past the abort is corroborated by an 8-branch probe (below): step 2 (exploit)
reaches the barrier-2 seal, which is only possible if `run_rollout`'s compose already succeeded.

### 4b. Shipped PureRandom path is hash-neutral ✅

`--config pure-random --max-branches 8 --repeat 2 …` → **exit 0**:

```
game box: repetition 2/2 bit-identical to the first.
game box work evidence: 8 branches, weakest rollout 2000000064 ns of V-time / 5020 COMPLETED frames.
```

Record→replay `state_hash` sequence bit-identical across repetitions, and byte-identical to a pre-fix
PureRandom run in every guest-observable artifact (the fix is a no-op on this path — no exploit occurs, so
no seal env is folded differently). `pure_8b.json` retained. The lease was released cleanly and the box
reverted to stock KVM (`kvm size 1396736`).

### 4c. NEW FINDING — a deeper exploit-SEAL cascade blocks a full seal-and-replay smoke ⚠️

Once past F3, the never-before-run exploit path hits a **distinct** real-KVM blocker when an exploit
materializes a **fresh candidate seal** (barrier 2). `--config selector-v1 --max-branches 8 …`:

```
game box: campaign failed: … differential campaign: snapshot requested at a non-quiescent point
```

Mechanism (pinned by an instrumented probe): the quiet-reseed (task 78) shifts an RDRAND to coincide
with the SDK-event candidate's V-time, so the seal lands the guest **mid-RNG-exit** and vmm-core's
`save_vm_state` returns `ContractViolation → NotQuiescent`. The **same** moment `223508107` seals fine in
the seed-only explore (step 1) but is non-quiescent in the reseeded exploit (step 2) — so it is the
reseed, not the coordinate. The server contract says the caller should "run a little further and retry"
(as `seal_base` does), but the two-barrier candidate-seal path does not. A naive retry-forward exposes a
**second** blocker: running forward overshoots a staged reseed Moment —
`run overshot staged Moment 729310246 (now at V-time 733924708); schedule unsatisfiable` — because the
guest's V-time intercepts are spaced wider than the retry step.

This is an **entropy × seal-quiescence** interaction (task-63 seal-rate grid / task-78 reseed schedule /
task-132 two-barrier), **orthogonal to the F3 coordinate fix**, and likely needs a ruling on whether a
candidate seal may advance off its nominated Moment. It does **not** regress anything shipped (PureRandom
passes; F3 composes). Filed as follow-up bead **hm-esfd** (P1, depends on hm-0paj). Per the spec's smoke-fire-once discipline
("run this minutes-long probe and report before any larger spend"), it is reported here rather than
fixed under this task.

## 5. Gates

- explorer: 137 nextest ✅, public-api snapshot ✅ (unchanged — only private items touched), clippy -D ✅, fmt ✅
- campaign-runner: 163 nextest ✅, clippy -D ✅, fmt ✅
- `cargo deny` ✅
- Miri (campaign-runner lib): 86 passed / 12 ignored (was 85 / 11). `.github/workflows/nightly.yml`
  ignored-ceiling ratcheted 11→12 and passing-floor 85→86, with the new
  `selector_v1_exploit_composes_under_seal_anchored_coordinate` (a filesystem-boundary `run_game_campaign`
  test) added to the ignored rationale; the added passing test is trivial pure logic (`non_adjacent_delta_still_aborts`),
  so the interpreted budget is unchanged.

## 6. Integrator notes

- **F3 (deliverables 1 & 2): complete and validated** — portable regression proves the fix and is
  non-vacuous; the box confirms the exploit COMPOSES past `NonAdjacentChain`, the campaign completes, and
  it replays bit-identically. PureRandom is byte-identical (hash-neutral).
- **The exploit-SEAL cascade (§4c) is a distinct, newly-discovered box bug**, filed as a follow-up bead
  (P1). It blocks a full "exploit seals a fresh candidate + replays" box gate but not the F3 fix. Resolving
  it touches ruled seal-rate/reseed territory and should get its own task + likely a ruling.
- No follow-on work was opened beyond that bead. Branch not pushed (per worker policy).
