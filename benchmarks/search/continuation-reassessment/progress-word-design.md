# Productive scoped words: theory and production qualification

SR02 is closed for valid conditional futility. The older exit-continuation
candidate also remains closed after its independent fresh-search confirmation
losses. This is a distinct, explicit action experiment: the existing exit bank
rejects same-slot transitions and reacts to ordinary preference improvements;
Metroid's optional scoped boss progress is not that preference. That source gap
identifies an expressible learning opportunity, not evidence that reuse helps.

## What can be established before emulation

Fix a retained state s and an observed word w that moved its parent to s with
same known progress scope, strictly higher progress and no endpoint resource
loss. Let Y(s,a) indicate a specified useful successor after applying a from s.
For a fresh action law mu, write q = E_mu[Y(s,A)], c_f = E_mu[cost(s,A)],
r = Y(s,w), and c_w = cost(s,w), with positive costs. If we repeatedly restore
this same state, independently choosing w with probability rho and otherwise
mu, the expected cost until the first hit is

    C(rho) = ((1-rho)c_f + rho c_w) / ((1-rho)q + rho r).

This renewal identity requires a positive hit probability, identical restored
state, independent draws and the stated cost accounting. Correlation between a
trial's cost and its own outcome does not invalidate E[cost]/Pr(hit). For q > 0, relative
to rho=0, any positive reuse share improves this frozen-state quantity exactly
when r/c_w > q/c_f. Neither the progress predicate nor the source observation
of w establishes that inequality: they concern w's parent, while r concerns s.
Unknown hidden phase and longer-term consequences remain experimental questions.

The production finite-world fixture supplies both sides of that missing
assumption. Binary word [0,1] moves a live parent from progress 0 to 1, with
unchanged known scope and health. Reapplying the queued word to progress 1:

- In the stationary world it reaches live progress 2 in two actions.
- In the adverse world the hidden phase changes at odd progress; [0,1] kills,
  while [1,0] reaches live progress 2. The queue sees the same endpoint predicate.

For an explicit finite fresh law, take seeds 0..511, retain those accepted by
production `energy_strategy(seed, 0, 255)`, and choose uniformly among the 511
accepted seeds. Production `draw_suffix(OneOrTwo, AlphabetOnly, ...)` succeeds
on 26/511 stationary trials and 33/511 adverse trials. These are exact counts
for that declared finite law, not estimates of the PRNG's full seed population.
Fresh cost is one or two actions. Thus reuse's stationary success per action
is 1/2 versus at most 26/511 for that fresh law; in the adverse world it is zero
versus a positive fresh rate. The fixture executes the state transitions and
obtains the word through production archive admission and the production queue.

This does not model a whole search campaign: retained states, costs and queue
availability change after each result. The implementation reserves one slot in
four, which is neither an independent Bernoulli rho nor a fixed share of executed
work when ordinary duplicates are skipped. The identity is a conditional design
criterion and counterexample framework, not a campaign speedup or power claim.

## Explicit implementation contract

`alphabet_scoped_progress_reuse_v1` learns only from newly retained candidates
with a parent, equal group-0 slot, equal known progress scope, strictly higher
progress value, and both known resource axes no lower. Qualification compares
endpoints; it makes no guarantee about intermediate states or future dominance.
It stores the actual parent-relative suffix, one to six actions, and queues one
trial from its retained child. It does not import historical witness words into
fresh campaigns. At most 1,024 trials are pending, newest first; overflow removes
the oldest. There is no exit graph and the logical memory reserve is fixed at
queue capacity. Queued stable ids do not pin snapshots or revive inactive states.
Dispatch also requires an available reconstruction origin and remaining actions.
A failed trial supplies no new word unless an independently qualifying retained
progress event occurs. Ordinary draws and old policy identifiers remain unchanged.

`alphabet_scoped_progress_fresh_control_v1` has the same queue, slot schedule,
parent checks, learned-word duplicate filter, splice-seed rejection loop and
memory reserve. It redraws a fresh alphabet suffix from that accepted seed and
draw checkpoint, with no second duplicate filter. This separates word reuse from
returning to the progress state, at the first difference from identical history.
Later arms are allowed to diverge. The recorded donor/leaf identify the learning
event; resolved tail bytes identify the executed suffix. Replay checks complete
execution and rederives the control draw. It rejects oversized raw tails before
time truncation and corrupted fresh-control bytes. Like existing continuation
replay, it uses recorded dispatch evidence rather than regenerating scheduling
choices or proving a recording's authenticity.

The NES challenge caller accepts only these two additional mixture identifiers,
requires the explicit Metroid progress feature, and permits them only with its
unchanged selector. Omission remains alphabet-only. Slot retention is separate;
all arms of a future action comparison must hold that policy fixed. Its existing
frame, execution, direct-work, memory, output and wall ceilings remain binding.

## Gates and limits of the current evidence

Source tests exercise actual retained/rejected learning, absent and cross-scope
metadata, resource loss, cross-slot transitions, bounded storage, stale parents,
the stationary/adverse worlds, and a first action divergence with matched parent,
seed and learning history. Complete production campaigns run both worlds and
both policies with one/four workers, one/two result buffers, and actual snapshot
eviction. Their stream bytes and final report/checkpoint agree across buffering;
replay agrees and planted suffix corruptions fail. Full searcher and NES checks
are recorded separately with source evidence. No unsafe invariant is changed.

Only a separately published prospective registration may allocate native work on
ms02. The next useful native gate is a short implementation qualification with a
new frozen seed, these two modes and an ordinary-alphabet control under the same
progress retention. Require actual queued action divergence, complete replay,
identities, physical receipts and bounded resources. Nonactivation closes that
qualification without seed replacement; it is not an efficacy estimate. A pass
permits designing, not silently launching, a separately registered conditional
endpoint screen with matched controls. No horizon rescue, reuse of failed-panel
seeds, control promotion, pooling or fresh-search claim follows from this source
work. Fresh development, independent confirmation, MM2 and untouched validation
remain unachieved. msr1 is unavailable and must not be accessed.
