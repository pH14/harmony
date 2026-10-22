# Random-schedule baseline

These scripts run a target the way a Jepsen nemesis would: every test boots
fresh, applies a fault schedule drawn at random, runs the workload and checks
the history. No test learns from another. The schedule is the only part that
differs from a search. Boot, workload, check and fault execution all come from
`harmony debug run IMAGE --config RESOLVED --seed N --actions FILE`.

## Schedule

`random_schedule.py` writes a JSON action list that `debug run --actions`
accepts. It draws fault kinds, nodes and parameters with the distributions of
`sample_action` in [`archive.rs`](../../faults/src/archive.rs):

- the kind is uniform over the kinds the vocabulary offers;
- the node, disk and hook are uniform;
- each window is `2^k` ticks of 10 ms, `k` uniform in 0..10;
- clock shifts, park edges and holds, event-kill rarity and the coverage
  quantum use the same ranges as the search;
- each action gets a random nonzero `choice`.

`--vocabulary` takes the `action_vocabulary` string that a search of the same
image records in its `stream.jsonl`. Event actions target any node, because a
schedule written in advance cannot know which nodes are event-ready.

Two densities:

| mode | schedule |
|---|---|
| `dense` | back-to-back actions for `--length` seconds, `Wait` drawn as one more kind, as the search draws a suffix with no history |
| `sparse` | repeated: wait uniform 0 to 2 x `--interval`, one fault, wait again, then heal; Jepsen's nemesis `stagger` with the default 10 s interval |

A heal restarts every node that a `Kill`, `KillAll`, `PowerLoss` or
`PowerLossAll` left down and runs `--heal-hook` when a network hook left a cut.
Every schedule ends with a heal and `--quiet` seconds of `Wait`. After the
input, `debug run` keeps running the bundle's check until it completes with no
fault pending.

## Running

`run_baseline.py` runs `--slots` tests at once for `--minutes` of wall time and
appends one JSON line per test to `OUT/results.jsonl`: seed, start and end
seconds, exit status, `bug_found`, guest ticks and violations. A test still
running at the deadline is killed and marked `cut_at_deadline`. Run directories
of tests without a finding are deleted; the action file of every test stays.

```sh
python3 run_baseline.py --harmony harmony --image IMAGE.oci \
  --config RUN/resolved.toml --vocabulary "$(cat VOCAB)" --heal-hook 4 \
  --mode dense --length 120 --slots 4 --minutes 60 --out OUT
```

A finding is a `debug run` directory, so `harmony explain OUT/tSEED --no-shrink`
and a `debug run` of `OUT/tSEED.actions.json` on a fixed image apply to it as
they do to a search finding.
