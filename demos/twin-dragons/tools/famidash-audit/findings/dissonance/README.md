<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Dissonance Famidash investigation

## Correction: early boundary patches withdrawn

**Do not apply `early-release.patch.gz`, `stereo-release.patch.gz`, or
`withdrawn/early-current.patch`.** These experimental changes close intended
hidden-coin routes in Stereo Madness and Base After Base. Independent replay
proved that the behavior occurred, but did not prove that it was a defect.
Blocking a route and finding another clearing path did not establish that
the repair preserved the level's objectives.

Original-ROM replays now establish:

| Previously reported opening | Objective reached through that route |
| --- | --- |
| Stereo Madness final ship ceiling | Coin 3 at frame 5,088; the original `stereo-escape.json.gz` then completes the level at frame 5,124 |
| Base After Base hollow ground | Coin 1 at frame 1,143, exactly the endpoint of `base-floor.json.gz` |
| Base After Base ship ceiling | Coin 3 at frame 3,388 in a new Dissonance witness |
| Back on Track ceiling fill | Coin 2 at frame 2,715 in a new Dissonance witness |
| Cycles upper ball passage | Coin 1 at frame 2,030 in a new Dissonance witness |

`coin-routes/` retains collection inputs and RAM observations. The pinned
release's coin timers are at `$04b3`–`$04b5`; its collected-bit mask is at
`$0461`. Source sprite layers place these coins beyond the apparent openings.
The original Stereo witness and the Base ground witness already contained
the evidence that invalidated their classifications. The Base ceiling,
Back on Track and Cycles collection inputs came from the subsequent actual
single-level Dissonance round. The collector replays ordinary controller
histories; it neither edits gameplay RAM nor generates search inputs.

Future review must inspect gameplay sprites, coin objectives, portals and
alternate routes alongside collision tiles. Decorative fill is not a
forbidden region. A particular component's coin route does not automatically
prove that every possible entrance to that component is intentional.

The production ROM and route tables were never changed by these experiments.
The rejected patches, their byte identities, replay results and alternate
clearing controls remain as historical evidence. Their former defect and
"clean control" classifications are withdrawn. The original narrative is
preserved in Git commit `cc5dc3bec8dfc31388bda738f5c5e664d3936c44`.

## Fofii positive control

The Fofii pit witness remains a separate finding. Its missing pit hazards
are below the playable route and do not close one of these coin passages.
`fofii-control.json.gz` records an actual Dissonance reproduction of the
known below-course endpoint `(4268, 897)` at frame 1,463. The retained pit
repair kills that input at frame 1,317 in both emulator cores. The original
release regression package and patch are documented in
`workloads/nes/src/famidash/repros/fofii/README.md` from the repository root.

The discovery started after 1,200 ordinary frames of the existing clearing
route, then used Dissonance to generate the continuation. Seed 1 recovered
the escape within 4,096 executions. A start-of-level search had not recovered
it in the earlier 50,000-execution budget; checkpoint placement matters.

## Initial discovery provenance

`searches.json` records two production arcade Dissonance campaigns: seed 6
from level 0 for 80,000 executions and seed 7 from level 6 for 50,000. They
reached the first eight built-in levels and part of Cycles. Their 124
candidates were detector signals, not confirmed bugs or an exhaustive audit.

The shipped search WASM was unchanged, SHA-256
`25d8e3053fd568d617b128dcb4b8f25e195852e315a64b09238197175e597f1c`.
QuickNES WASM SHA-256:
`d63a56ff06a9b08a01155fe04df37d4f6577c110712882c59d232f4cf2acbf31`.
Main release ROM SHA-256:
`7acf068a406d551ff18afa3fc1ed228ff90f72651c4a90a3862b997240bb9b58`.

The minified production bindings were fetched from
`https://ph14.github.io/harmony/arcade/assets/campaign-BLO6QPAW.js` and
adapted only to export the existing search classes to Node. Original JS
SHA-256: `bed6901a5145719e7b5f973a1ea77c4753753ae9d24415a1cfc4a7d20b2e7fd4`;
adapted JS SHA-256:
`253c6e8b114eebaf22d9c125cd9653a1ac89277415685fac372d9436ac293b4d`.

The harness calls Dissonance's `plan`, `run` and `admit`. Its inspection and
replay scripts do not replace archive selection or input generation.
Reference-route distance and geometry overlap are triage signals. The
earlier custom-mutation audit omitted `CHECKERBOARD_15` and mistakenly
treated clearing routes as reference bounds; it cannot establish coverage
or route legality. The later all-level audit is documented in the parent
README and retains separate search, replay and review statuses.

## Replay

From the repository root, with the pinned release runtime and game assets:

```sh
node demos/twin-dragons/tools/famidash-audit/dissonance-replay.mjs \
  CASE.json.gz ROM RUNTIME_DIRECTORY
python3 demos/twin-dragons/tools/famidash-audit/fceumm.py \
  CASE.json.gz FCEUMM_CORE ROM --intro-frames 40
node demos/twin-dragons/tools/famidash-audit/coin-paths.mjs \
  CASE.json.gz ROM RUNTIME_DIRECTORY coins.json
```

The early main-level mechanical witnesses differ between QuickNES and
FCEUmm only during entry animation; the explicit exclusion checks every
recorded frame from 41 onward and lists earlier differences. Fofii needs
no exclusion. FCEUmm revision:
`ac0d00a7fc01166f84c7bac01d1084ebfd2b865e`.

The discarded early release ROMs were experimental official-ROM data
transplants, never production assets. Combined SHA-256:
`c835ef71af6b5e282a0c2c2304636510f78712569973f0a201d7bfcea0396fe6`;
ceiling-isolation SHA-256:
`f2922a5b2e27cf0f4eb534909ec77d618e43a81367abeaeaa5a26adec500dd95`.
The archived current-upstream proposal targets
`541d68ffcae2096f116146ef21cbc4d62886eb6b` and has not been submitted.
