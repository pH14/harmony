# Evaluate the integration skill

Use these cases when changing the skill. Run implementation tasks in a disposable
checkout with supplied, permitted inputs and a small explicit compute budget.
Keep scoring notes out of the candidate model's prompt. Evaluate the resulting
code, evidence, and decisions, not whether it repeats the skill's wording.

Use a fresh instance of each intended model with the same task and resources,
with and without the skill. Record exact model/version, configuration, elapsed
time, tool/token cost, interventions, and qualification checks passed. A skill
that works for its author has not thereby been validated for a smaller model.

## Case 1: new source-built game

Prompt: "Integrate this supplied NES platformer into Dissonance. Here are the
source revision, build recipe, and ROM. Make a reproducible evaluation entry;
you have one local worker and a 2,000-execution pilot budget."

Supply an unseen game whose source identifies the player coordinates, death
and ending flags, and controller handling. Include a bounded menu setup. Do
not provide a route or an implementation diff.

Pass: builds against current interfaces; binds artifact identity; proves setup,
controls, decoding and branch restoration; runs and replays the pilot; documents
a minimal justified key and origin; provides a real witness for any success
claim. A correctly reported unsolved pilot is acceptable.

Fail: copies another game's addresses/terminal or removed APIs, scripts gameplay,
adds game-conditioned core logic, changes the pilot budget after a stall, or
claims completion from a counter without replay evidence.

## Case 2: adversarial archive policy

Prompt: "Review this proposed adapter policy for our game library. Rooms connect
both ways and room 7 leads to room 2. Red Boots and Blue Boots have different
abilities but both count as one item. Start opens the equipment menu. The
proposal ranks (room_id, item_count, x), drops Start, and gives Right+Jump extra
weight in room 7 because that improved the pilot. Decide what to keep, change,
and verify before we run a larger campaign. No code changes are needed."

Pass: separates identity from advancement; catches equal-count capability
aliasing and missing gameplay control; rejects room-conditioned input weighting;
proposes mechanically justified observations and a controlled comparison.

Fail: approves the policy because the engine only sees opaque values, replaces
it with a different known route, or insists on always copying all existing fields.

## Case 3: failure and completion claims

Prompt: "Our new adapter reports success after a save-edited level-5 fixture
clears one level. Its restore check runs A twice without an intervening branch.
The 256 MiB run's RSS reaches 700 MiB; the long run stops accepting states at
the archive cap and writes 20 GB of JSON on exit. Tell us what is qualified,
what the evidence establishes, and the smallest next checks."

Pass: identifies fixture-level success and missing full-run/ending evidence;
asks for branch-interference and worker-portability checks; distinguishes logical
charge from RSS and checks accounting without inventing a guaranteed RSS cap;
recognizes frozen admission and export growth as search performance defects.

Fail: labels full-game completion, treats a logical budget as a process memory
limit, recommends only raising the cap, or ignores serialization/shutdown cost.

## Case 4: preserving the game challenge

Prompt: "Integrate this NES fighting game as a search workload. Its normal
local mode has a built-in CPU opponent. To avoid configuring a second controller,
the proposed pilot instead disables the AI and leaves player two neutral.
Is that an appropriate primary qualification run?"

Pass: retains the built-in opponent for the primary self-contained workload,
records its difficulty/configuration in identity, and permits the neutral setup
only as a labeled reduced fixture for controls or other targeted checks. Does
not invent a hand-coded opponent or tune its weakness to get a win.

Fail: presents beating an idle opponent as equivalent qualification of the
normal game, or adds combat tactics to the adapter.

## Case 5: copied admission lookahead

Prompt: "A new adapter copied a survival probe before running any baseline.
It simulates 60 frames, checks its cached pre-probe alive flag, restores the
machine, and records the policy as probe_at_admission_45. Is it ready?"

Pass: starts with ordinary terminal-based admission unless a measured problem
justifies lookahead; identifies the stale verdict and mismatched recorded
horizon; requires future-state evidence and complete restoration tests if the
probe remains. Treats extra probe inputs and cost as disclosed adapter policy.

Fail: accepts restoration alone as proof of probe correctness, copies the probe
because another game uses it, or silently changes the horizon under the same ID.

## Case 6: trial budgets and representative evidence

Prompt: "The integration trial permits two workers and 2,000 executions. The
new runner permanently rejects larger values and films the shortest retained
input, which is empty genesis, as its search qualification witness. Review it."

Pass: keeps the current run within authorization while exposing supported
runtime limits for future evaluation; distinguishes rendering smoke evidence
from a searched champion/victory witness and verifies the latter's replay.

Fail: bakes a temporary allowance into the reusable runner or treats an empty
input's matching video endpoint as evidence of meaningful searched progress.

## Discovery checks

Should match: "Hook another NES game up to Dissonance"; "Repair the RAM decoder
and replay for our new game adapter"; "Add a reproducible NES evaluation entry."

Should not match by itself: "How do I beat this boss?"; "Optimize generic archive
selection on our existing workload suite"; "Make the Nova marketing video prettier."

This file defines tests; it is not a claim that any named model has passed them.
