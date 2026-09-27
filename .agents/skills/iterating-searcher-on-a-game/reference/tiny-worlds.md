# Tiny worlds panel and scorecard

`workloads/tiny-worlds` runs small deterministic worlds on the real searcher.
Its README is the source of truth for world options, verdict rules and the
measured false-fail rates; this file covers how the game iteration uses them.

## Contents

- Building the two executables
- Rules and compare runs
- Reading a compare result
- The scorecard
- Building a new world

## Building the two executables

The panel compares two builds of the tiny-worlds executable, each linked to one
version of the searcher. Build the baseline from the change's base commit in a
separate worktree, then build the candidate:

```sh
cargo build --release --locked --manifest-path workloads/tiny-worlds/Cargo.toml
cp workloads/tiny-worlds/target/release/tiny-worlds /private/tiny/candidate
```

Record the commit each executable came from; the reports carry build-time
source hashes but no commit names.

## Rules and compare runs

```sh
uv run workloads/tiny-worlds/panel.py --jobs 10 --binary /private/tiny/candidate
uv run workloads/tiny-worlds/panel.py --jobs 10 --binary /private/tiny/candidate \
  --compare /private/tiny/baseline
```

- The first command runs the rules. Each rule records a current searcher
  behaviour, such as whether boss damage counts or whether credit is kept after
  leaving a room, and prints PASS or FAIL.
- The second command also runs the compare worlds on both executables with the
  same layouts and runtime seeds. Each world is shaped after one Metroid
  behaviour and reports a few legs; the README's compare table names the
  behaviour and the legs.
- The exit status is nonzero when a rule fails or a world is clearly bad or
  undecided. The panel writes no files; keep its output outside the repository.
- A rule close to its threshold can flip between runs of an unchanged searcher,
  so rerun a failed rule before blaming the change.

## Reading a compare result

Each leg prints the median candidate-to-baseline ratio of work, a 99% interval
and how many layouts reached the leg's end in each run.

| Verdict | Meaning |
| --- | --- |
| slower | The interval lies above 1.25. The world is clearly bad. |
| faster | The interval lies below 0.8. |
| inside the band | The interval lies within 0.8 to 1.25. |
| undecided | The interval crosses 0.8 or 1.25. The world doubles its layouts and runs again, up to its limit. |
| watch | Undecided at the limit, reaching above 1.25 with a low end at or below 1.0. The world passes, and the leg is named. A leg that reaches above 1.25 with a low end above 1.0 leaves the world undecided. |

Goal misses get their own verdict from a sign test on the layouts where only
one run missed the goal; more misses by at least 5% of layouts is clearly bad.

Act on the result this way:

- Drop a change that is clearly bad on any world.
- A world still undecided at its limit means the change is not plausible. A
  verdict needs every interval resolved; an undecided interval is never a pass.
- A plausible change is one the panel did not reject. Carry every watch leg to
  the game: name the matching game leg and measure it in the slices.
- A world whose `identical runs` count equals its layout count never ran the
  change, so its verdict says nothing about the change.
- A leg printed as `nan` had fewer than three layouts reach it in both runs, so
  it was not measured.
- Two unchanged searchers fail each world in 0% to 2.3% of comparisons, as the
  README lists. A single clearly bad world near those rates deserves a rerun
  with more layouts (`--world-scale`) before the change is dropped.

## The scorecard

The scorecard shows whether the panel predicts the game. Until its entries
land inside their ranges on the game, including entries that predicted a
change, use the panel only to reject changes.

Keep it in private notes outside the repository. One entry per change and game
run:

| Field | Content |
| --- | --- |
| Change | What the change does and its commit. |
| Panel evidence | The world, leg, ratio and interval each prediction rests on. |
| Game leg | Its two ends, such as Tourian entry to `tourian_bottom`, and its unit, such as tries after the resume. |
| Prediction | The ratio of changed to unchanged tries: a median and an 80% range. |
| Filed | When the prediction was written, which must be before the game run starts. |
| Outcome | The measured ratio per seed and hit or miss against the range. |

- Start each prediction from the matching world's leg ratio, then move it
  toward 1.0 where the world's measured rates differ from the game's, such as
  how often a replayed input lands. Write that reason in one line.
- Write every prediction before the game run starts, and never edit a filed
  prediction. An update is a new line filed before any outcome is known.
- Count a leg only when both runs reach both of its ends, and count a run that
  never reaches the far end as censored at its budget.
- A few seeds cannot resolve effects as small as 0.8 to 0.9, so a game run
  checks a prediction near 1.0 only for a large miss.
- A world whose measured rate differs from the game's cannot predict a change
  that acts on that rate. In one Metroid entry, jittered retries landed in 32%
  of attempts in the world and about 4% in Metroid, and the prediction missed.
- Put the entries for a change in its pull request description.

## Building a new world

Build a world only after the game shows where the loss is: film, census and leg
tries have named the place and the quantity that runs out.

1. Write the game measurement down first: the leg, its tries per seed, and the
   mechanism the film shows.
2. Build the world from the existing families and options in
   `workloads/tiny-worlds/src`, and add an option only for the mechanism the
   game showed. Keys in most families rank only goal or not goal, so a loss
   that comes from the tier order needs a ranked variant.
3. Check that the unchanged searcher loses in the world where it loses in the
   game, at about the game's rate. A Metroid-shaped world with a Tourian-like
   gauntlet was dropped because its gap sat at farming, while Metroid's gap is
   on the route to Tourian.
4. Add the world to the compare table in `panel.py` with its legs, measure its
   false-fail rate with unchanged searchers, and record the rate and the game
   behaviour in the tiny-worlds README.
5. Add a rule when the world records a behaviour every searcher change should
   keep.

Tiny-world changes are workload changes: run the three checks in the
tiny-worlds README (`cargo test`, `cargo fmt --check` and `cargo clippy`).
