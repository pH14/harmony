# Tiny worlds panel and scorecard

`workloads/tiny-worlds/README.md` describes the worlds, their options, the
verdict rules and the measured false-fail rates.

## Running the panel

Build the tiny-worlds executable from the change and from its base, then run
`uv run workloads/tiny-worlds/panel.py --binary CANDIDATE --compare BASE`. The
rules print PASS or FAIL; the compare worlds report a ratio and interval per
leg. The exit status is nonzero for a failed rule or a clearly bad or
undecided world. Rerun a failed rule before blaming the change, because a rule
near its threshold can flip.

## Reading the result

- Clearly bad on any world: drop the change.
- Undecided at the layout limit: not plausible yet.
- A watch leg: name the matching game leg and measure it in the slices.
- A world whose `identical runs` count equals its layout count never ran the
  change, so it says nothing about it.
- Plausible means the panel did not reject the change. It forecasts the game
  only once the scorecard shows its predictions land.

## The scorecard

Keep it in private notes. One entry per change and game leg, for workload key
changes as well as searcher changes:

- The change, and the world and leg ratio the prediction rests on.
- The game leg: its two ends and its unit, such as tries after the resume.
- The prediction: the ratio of the changed arm's median tries to the unchanged
  arm's, with an 80% range. Start from the world's ratio and move it toward
  1.0 where the world's rates differ from the game's. For a key change, the
  world's ratio compares the information hidden against the information in
  the key.
- Filed before the game run starts. Never edit a filed prediction.
- The outcome and hit or miss. A run that never reaches the leg's end is
  censored at the tries it reached.

Put a change's entries in its pull request description.

## Building and improving the worlds

Build or adjust a world for each hard part at diagnosis, and again after a
scorecard miss. A miss means the world lacks the game's mechanism or runs it
at a different rate. For example, retries that land in a third of attempts in a
world and in a few percent in the game cannot predict a change to retries.

1. Write down the game measurement: the leg, its tries per seed, and the
   mechanism the film shows.
2. Change an existing world or build one from the families in
   `workloads/tiny-worlds/src`. Add an option only for the mechanism the game
   showed.
3. Check that the unchanged searcher loses in the world where it loses in the
   game, at about the game's rate.
4. Add the world to the compare table in `panel.py`, measure its false-fail
   rate with two unchanged searchers, and record the rate and the game
   mechanism in the tiny-worlds README.
5. Add a rule when the world records a behaviour every searcher change should
   keep.

Run the checks the tiny-worlds README lists before opening its pull request.
