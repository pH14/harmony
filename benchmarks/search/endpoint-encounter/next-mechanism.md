# Next candidate: bounded local retry after an ordinary terminal action

CI01 changes the question. AP01 retained six live states with classified Ridley
HP below 140, including a parent chain with HP139,131,130,129. All have health 79
and no missiles. They received 27 jobs: 14 had no retention candidates, eight
had only rejected candidates and five retained children. The HP 129 state received
four jobs / 179 admitted frames; all four had empty decision lists. It was both
retained and selected. These observations refute blanket explanations that the
archive kept no low-HP encounter state or never returned to it. They do not
identify why every continuation failed or prove a general retention cause.

The next candidate is **one local retry after a normal workload-terminal action**,
from the preceding live snapshot within the same rollout. A retry draws another
ordinary command; it does not repeat a remembered button combination. All failed
attempts and restores must be accounted. Further failure at that same live state
ends the rollout. Ordinary successes may continue under a fixed action/frame cap.
Crashes, emulator errors and discovered software failures retain their existing
reporting and stopping behavior. This must be explicit opt-in search behavior,
with no boss HP, geometry, game route, reference input or extra reward.

This is a hypothesis about continuation generation and temporary local state,
not a new retention policy. It differs from failed unconditional persistence
(which changes button correlations) and existing learned-exit retries (which
replay saved transitions only after strict same-slot resource improvement).
No claim of benefit follows merely because those mechanisms differ.

## Exact model and counterexample before an implementation

For a K-step aliased chain, suppose each independent unit-cost action succeeds
with probability q; failure loses the current local chain. With at most r tries
per step, let s = 1-(1-q)^r and t = s/q. One episode succeeds with probability
s^K and costs t sum(j=0..K-1) s^j in expectation. Regenerative expected work to
success is their ratio. Failed attempts are included, not free lookahead.

[`derive_local_retry.py`](derive_local_retry.py) uses exact rational arithmetic.
For K=1 the candidate and baseline both cost 1/q: resampling supplies no magical
per-attempt advantage. For K=5,q=1/4, expected work is 1364 attempts without retry
and about191 with one retry. The benefit comes from reducing reset of already
completed *unretained* steps. If every intermediate is perfectly retained and
immediately revisited, the ideal return cost is already K/q=20; the favorable
aliasing argument then does not apply.

An alive-trap counterexample reverses the result. With equal chances of an
immediate goal or an alive dead end, a failed rollout costs two attempts; retry
adds one useless attempt. Expected work rises from 3 to 4. Viability is therefore
not a sufficient value signal. These examples do not fit q or K to Ridley, imply
that its live actions are productive, or predict a native win.

## Next bounded decision, before emulator time

Spend at most one 45-minute source/model pass on feasibility and a cheap
falsifier. Check existing campaign action/result/input and replay contracts:
failed trial commands must never be silently included in a surviving replay
witness or silently omitted from physical cost. Keep defaults byte-compatible.
Specify fixed total work, command and per-job limits, RNG derivation, failure
records and temporary snapshot memory. A retry is not allowed to evade the
original job/campaign budgets or use an uncharged inner search.

Reject this candidate before native work if it needs a broad execution/replay
rewrite, cannot distinguish ordinary terminal states from errors, or only
repackages an existing retry mechanism. First exercise an executable generic
finite-state fixture with aliasing, complete retention, alive traps, exhausted
budgets, and failure-path replay. No new observer framework or heuristic sweep.

Only a small qualified implementation can earn a separately frozen matched-work
comparison from the original E01 encounter, keeping all supplied-root evidence
conditional. Its endpoint and futility rule must be fixed before execution;
neither this model nor CI01 allocates that panel. Failed conditional capability
stops the candidate. A useful conditional result still has to pass fresh paired
development, independent confirmation, MM2 transfer and untouched validation
before fulfilling the original goal. Do not select another damage root or
promote/tune a policy from the six inspected snapshots.

## Feasibility outcome and the actual bounded law

The source pass produces a small shared retry state helper and one explicit
branch marker in worker results. The generic coordinator removes the abandoned
dead action from surviving inputs, while retaining every attempt's evidence
and charging the target's monotonic physical frame counter. Metroid and MM2
use the same helper. Defaults omit the new metadata and execution-policy field.
No archive, selector, input alphabet, action duration or scheduling rule changes.

The implemented law consumes the **same pre-drawn one-to-six attempt sequence**
as control. It draws no replacement beyond that sequence. Every failed attempt
consumes the existing job/input cap. A second consecutive death stops; a live
step starts a new opportunity. Errors and victories never trigger restore.
The prior uncapped-per-stage formula is therefore only explanatory. The exact
[`derive_predrawn_retry.py`](derive_predrawn_retry.py) model includes the real
finite cap: at K=5,q=1/4, expected work is 3868 versus 57536/23 (about 2501.6), not
1364 versus 191. Seven aliased steps cannot be reached under either six-attempt
policy. These are toy implications, not native gain estimates.

Five generic fixture checks exercise aliased and distinct keys, surviving input
composition, attempted work, repeated ordinary death, errors, victory, cap
exhaustion, restore failure, restored milestones and an alive trap. The first
surviving retry report also pins its original worker snapshot digest. A native
qualification must replay its linear input against that snapshot; a retry count
without an actual surviving replay cannot pass. Default serialization uses the
complete old action vector; retry positions append after it to avoid ambiguous
optional fields inside postcard action encodings.

The source/fixture decision permits one separately registered native
qualification: exact historical default artifacts plus one/two-buffer candidate
runs from the original E01 root. It does not yet permit a performance panel,
new root, added HP reward or longer search.
