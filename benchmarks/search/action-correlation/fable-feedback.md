## 1. Is the finite theory and matched control sound?

**What is actually proved (I re-derived it from the supplied files):**

- Candidate self-transition: 1/72 + (1/6)(1/9 + 1/2 + 1/2) = 43/216. Matched control: p + (1−p)/36 = 43/216 ⇒ p = 37/210. Both correct.
- Direction-same: candidate 11/27, matched 253/945, iid 1/9. All-change: 1/9, 173/945, 2/9. Correct.
- Spectrum: a degree-k tensor contrast has candidate eigenvalue (3−k)/6 ⇒ 1/3, 1/6, 0 with multiplicities 10, 17, 8 (sums to 36). Matched control is 37/210 on every nonconstant contrast. Correct.
- The Rust test's expected counts (3 + 4[same A,B] + 18[same D,B] + 18[same D,A]) reproduce the kernel exactly; `before & 0xf0 == after & 0xf0` parses as intended under Rust precedence; run-length identity holds beyond one step because the self-transition probability is state-independent in both kernels; path-by-path preservation of `hold_frames`, length and tap positions is true by construction (the code never touches them), not merely by the 4096-seed sample.

I found no concrete bug in `chord_correlation.rs`. Two unverified assumptions matter more than any code line:

1. **Duration/button independence in `sample_chord`.** If the base sampler ties hold length to the ordinary chord it drew, the transform re-labels a chord under a duration drawn for a different chord and the "unchanged law" claim is only marginal. The supplied files do not show `sample_chord`.
2. **Error handling at the call site.** `apply` returns `Err` for >6 chords or any non-ordinary non-tap chord. If any caller path swallows that error and keeps the untransformed suffix, the candidate arm silently contains independent suffixes. Policy-identity rejection at replay does not catch this; only a count of applied-versus-attempted transforms would.

**The strongest scientific limit is the control's attribution claim.** Matching whole-command run length does not isolate "component correlation." The candidate differs from the matched control in three directions at once: stronger single-component persistence (direction-same 0.407 vs 0.268 per transition), slightly weaker pair persistence, and zero triple persistence. The mechanically dominant difference for platformer locomotion is almost certainly the first — longer direction holds with A/B toggling — which is just a different flavour of the already-failed duration/persistence sweeps. A candidate win over matched-whole would have shown "this spectrum beats a flat spectrum," not "component structure matters"; a loss (which triplet 1 delivered) cannot separate the two either. Isolating it would need a fourth product-kernel arm matched on degree-1 eigenvalues, and I do not recommend building it.

**Second limit: the endpoint is a weak instrument.** Ordinary hits cluster at 33.9M–48.8M with a 50M cap, so censoring is near coin-flip. With both-censored ties and ≥3/4 strict wins, the gate is dominated by censoring noise, and the futility rule makes a whole panel hinge on one control hit — exactly what happened in the earlier D01 (48.3M vs censored). The matched-whole hit at 34.8M sits inside the ordinary calibration range and is not evidence that persistence helps.

**Status of the live panel (do not touch it):** after triplet 1, the candidate needs strict double wins on all three remaining seeds. That is attainable, so the registration stands; it is also improbable given a ~75% ordinary hit rate at this horizon.

**Proved local statement vs adaptive utility:** the exact model proves the transform is a valid, marginal-preserving, reversible kernel with a well-matched control and a precise spectral difference. The constructed tasks prove *no reward-independent dominance* in either direction. So the prior probability of game gain is the base rate of any random proposal knob; exact arithmetic did not raise it. Triplet 1 is already one unfavourable sample.

## 2. Challenge to the framing

The project's stated aim is boss defeat/Wily 4, but every allocation still measures precursor surrogates (first missile, energy tank). Two replicated precursor gains failed to transfer; #285 shows 0/10 *controls* at 400M. A comparative panel cannot detect improvement from a zero base rate, and a precursor gain is uninformative unless the precursor is the bottleneck — which nobody has shown. Your own record identifies the gap: E03 examined eight *surviving witness tapes*, "not a census of their campaigns," and the C01 audit says the sidecar cannot label work by capability state. The funnel from "enter Kraid area" (observed) to "defeat" (never) has unmeasured rungs in the middle: *archived boss-capable state* and *damage from such a state*. The action-correlation study, like the selector/retention/duration studies before it, changes an input to the search without knowing which rung fails. Adding a new exact identity does not change that.

## 3. One next decision

**If D01 fails: fund no second correlation panel, no α sweep, no persistence/retention/selector variant. Spend zero fresh search frames. Resolve one fact from existing archives: does any fresh ordinary campaign at 50M–400M frames ever *hold* a boss-capable state?**

**Falsifiable mechanisms, ordered:**

- H_cov: no fresh campaign archives a state satisfying (boss-area room ∧ health > 0 ∧ missiles ≥ 1 ∧ boss slot loaded or loadable). Falsified by one such archived cell.
- H_fight: from an archived boss-capable state, the ordinary action law produces no guarded HP decrease within a bounded budget. Falsified by one HP decrease under the F03 lifecycle contract.

Note that a final-archive census measures *active recoverability* directly (archived = restorable now), which is exactly the caveat you raised about source-path unions being only lower bounds on past visitation.

**Cheap executable counterexample:** restore each archived snapshot of the three completed triplet-1 arms on ms02 (and #285's 400M archives and C01 controls if their final archives were persisted) and read RAM with the already-qualified `boss-context` reader — NQ02 qualified it across 62 real self-restores, so no new observer work is needed. Cost is one RAM read per cell; if the cell key already encodes room, it is a filter on the archive index. If final archives are not persisted, add a default-off end-of-run archive export (must leave default bytes identical, as B01/QX01 verified for prior changes) and apply it to a deterministic reproduction of the one C01 control that hit at 33.9M — that is known auxiliary work, as B01 was, not a performance sample.

**Evidence already available vs genuinely missing:**

- Available: qualified boss lifecycle classifier (F02/F03), restore-safe reader (NQ02), reference HP trajectories (Ridley 140→0, Kraid 96→0), Kraid-area entry in fresh tapes (L04), 0/10 defeats at 400M (#285), the health-underflow terminal predicate.
- Missing: (a) whether any archived cell is boss-capable; (b) whether the ordinary action law can damage a boss from such a state; (c) how many expansions capable cells receive — answerable only if the archive stores per-cell selection counts, which the supplied files do not state; do not reconstruct frames to get it.
- For MM2 the analogous census needs a stage/boss reader that is not in the supplied record; do not assume one exists.

**Prospective go/no-go, frozen before any emulation:**

1. Census outcome across ≥3 campaigns totalling ≥150M frames.
   - **0 capable cells:** H_cov stands. The failure is upstream coverage/resources; every proposal-level or selector-level knob acting in boss rooms is moot. The only performance allocation this earns is the one untested interaction — resource retention with corrected terminal — measured on a critical-path endpoint (first archived boss-capable cell), with precursor endpoints retired as surrogates. Same 4-seed, matched-control, ≤1.25× resource discipline.
   - **≥1 capable cell:** proceed to step 2.
2. Fight-local probe: up to 4 archived capable states, ordinary proposal only, ≤2M frames each, three-way replay, HP decreases counted under the F03 contract. Origins are diagnostic and labelled as such; they never become performance origins, so the no-solution-tape rule is respected.
   - **≥1 guarded HP decrease:** the action law can hurt bosses; the bottleneck is work allocation to capable cells. That, and only that, would justify a capability-aware selector — with a from-state damage rate as its calibrated endpoint rather than energy tanks.
   - **0 decreases in ~8M frames:** the ordinary law is fight-inadequate at 42-frame mean holds. This earns the bounded learned proposal below, gated on the same from-state damage rate before any campaign-level run.

Either branch replaces surrogate sweeps with a measured bottleneck for roughly one arm's worth of auxiliary frames or less.

## 4. Learned proposal vs further tweaks

**Reject further retention/selector/persistence tweaks.** Their justification is a precursor gain that failed transfer twice, plus #285, #288, duration and continuation negatives. The action-correlation kernel belongs to this class; its exactness is about the proposal, not the game.

**The bounded learned proposal has stronger mechanistic justification but is not yet earned.** It is the only mechanism class that adapts the action law to dynamics without routes, and an implementation pattern exists (SMB step tables, suffix-only folding). Its weaknesses are real: Metroid/MM2 `DrawState = ()`, so with the current hook it can only reshape the *marginal* action distribution from retained inputs; retained inputs are exposure-biased toward early-game locomotion, so unconditioned reinforcement would push mass *away* from rare fight actions. Calibrated contextual credit is not something the hook provides, and nothing supplied shows the SMB tables ever produced a gain. So it should be built only in the H_fight branch, with a minimal non-route context (e.g. room class × resource class from RAM, which is an observation, not a target), and its first gate must be a from-state damage rate — not another energy-tank panel.

**Summary:** the finite theory is correct and the control is well matched to the wrong quantity; the panel is unlikely to pass and would be uninformative if it did. Stop measuring surrogates, census the archives you already paid for, and let the missing rung of the funnel — capable state or damage — decide the single next mechanism.
