<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# NES game logic

`nes-games` holds the game logic of the NES workloads: Super Mario Bros., Mega
Man 2, Metroid, Castlevania, Super Tilt Bro, Twin Dragons, Famidash, Punch-Out!!, The Legend of
Zelda, Donkey Kong, Ice Climber, Pac-Man and Joust. It builds for the
host and for `wasm32-unknown-unknown`, so the browser demo can run the same
game logic as the native workload.

Each game module not declared through assertions (below) has a `target` with the RAM decoder, observations, snapshots,
setup tapes and the `Target` implementation, and an `archive` with the
controller vocabulary, archive keys and milestones. Mega Man 2, Metroid and
Castlevania also have a `progress` module for named progress. Famidash has a `levels`
module with each side's level names, lengths and groups. `chord` holds the
controller chord draw. `nes_backend` holds the `NesBackend`, `NesMemory` and
`SnapshotState` traits that every target uses to run, snapshot and read a
machine.

Every target type is generic over its backend. The QuickNES parts are compiled
only on Unix, because `machine::quicknes` exists only there: the `NesBackend`
and `NesMemory` implementations for `QuickNesMachine`, its LZ4 snapshot
packing, and the constructors, capture and render methods of each target that
take a ROM and a core path. The `consonance` feature adds the backend
implementation for the Consonance machine. The `test-support` feature exposes
test constructors and the scripted Super Tilt Bro machine to the tests of
`nes-workload`.

Super Mario Bros., Mega Man 2, Metroid and Castlevania
also have a `campaign` module with their `Workload` implementation.
`CastlevaniaGame<M>`, `SmbGame<M, P>`, `Mm2Game<M>`,
`MetroidGame<M>` and `NesWorkload<G, M>` are
generic over the backend and build each target from a boxed factory, so the
native workload and the browser demo share every trait implementation: the
native one passes a QuickNES or Consonance factory, and `demos/twin-dragons`
passes one over the browser core. LZ4 snapshot packing is available on every
target for that reason. `pack_quicknes_state` writes the marker `QNP2`, the
state length, the length of the machine's `HQNESST3` header, that header
verbatim and an LZ4 block of the core state; a browser state has no header and
packs whole. The header holds the prepared execution hash, which differs
between binaries and hosts, so keeping it out of the LZ4 block makes a
snapshot's memory charge depend only on the emulated state. Unpacking refuses
any other marker. Job results are digested with
`postcard_semantic_result_sha256` from the searcher, which leaves out emulator
state, so a browser campaign and a native one produce the same decision
stream. Archive entries record a snapshot digest from `snapshot_identity_sha256`,
and an archive checkpoint must match those digests. SMB hashes the logical
snapshot identity; the other games hash the serialized snapshot.

`SnapshotState` has two identities. `logical_identity` is the archive and
saved-state identity: equal logical identities mean the state restores in
this execution, so for a native QuickNES state it keeps the whole header,
prepared execution hash included. `endpoint_identity` is the witness identity:
the emulated state, whichever binary reached it. It defaults to
`logical_identity`, and for a packed QuickNES state it zeroes the prepared
execution hash in the header, which differs between any two binaries.
`NesSnapshot`, `StbSnapshot`, `MetroidSnapshot`, `Mm2Snapshot`,
`CastlevaniaSnapshot` and `SmbSnapshot` expose `endpoint_identity_sha256`,
which hashes the serialized snapshot with its emulator state replaced by
`endpoint_identity`, through `nes_backend::endpoint_identity_sha256`.

Each game's recorded emulator policy ends with a hash field. The native
constructors take the core hash and the prepared execution hash from
`machine::quicknes::prepared_execution_sha256`, record both, and load every
machine with `QuickNesMachine::from_rom_bytes_with_identity`, which rejects a
load whose executable code differs from the prepared one. The browser demo
passes the hash of its core WASM module instead.

`arcade` holds what the arcade page draws, for both the browser campaign and a
native live campaign. `ArcadeGame` extends `Workload` with a view of each
observation, the four-number motion sample of a frame, the cleared-level count
and the emulator state of a snapshot. The `smb`, `metroid`, `mm2`, `castlevania`, `famidash`,
`twin_dragons`, `punchout`, `zelda`, `joust`, `donkey_kong`, `ice_climbers` and `pac_man` submodules implement it for each campaign game over any
backend. `ArcadeGame` decodes RAM in static functions, so `Famidash` takes
the index of a level group in `famidash::levels::GROUPS` as the const
parameter `GROUP`. Each group is its own game type, with that group's side,
levels and map tops. Punch-Out!! has no screen position for Mac, so its motion sample places a point by damage: x is the opponent's damage across the screen and y is Mac's damage in a band along the bottom edge. `trail::Traced` wraps a machine and records each job's motion trail
every four frames and at every chord end, and every controller action it runs. The trail starts at the first frame at or after the replayed prefix's planned frames: a weapon switch or missile volley in the prefix plans an estimate, so the trail then starts off the suffix's first frame by the estimate's error. `feed::run_noted` runs a job inside
that recorder and returns a `Note` with the trail, the view of every step and
each candidate's snapshot digest. `feed::Feed` joins notes with the
coordinator's admission decisions into the `Batch` the page reads: points,
trails, finished trails and their exits. It drops ids the archive no longer
holds once it keeps more than twice the archive's entries.
`feed::archived_state` builds a state's controller history and replays it
through a fresh target with `feed::played` when the archived input misses
frames the core ran. The digest is taken over the QuickNES
core state without the native snapshot header, so a native snapshot and a
browser capture of the same state have the same digest.

[`../nes`](../nes/README.md) re-exports these modules and keeps everything
else: campaign drivers and configuration, Super Tilt Bro's `Workload`
implementation, streams, films, evaluation, packaging, guest preparation and
binaries. The game READMEs under `../nes/src` document the RAM maps and
policies of each game.

```sh
cargo test --manifest-path workloads/nes-games/Cargo.toml --all-features
cargo check --manifest-path workloads/nes-games/Cargo.toml --target wasm32-unknown-unknown
```

Snapshot checkpoint codecs live with these shared target types so native and
browser callers use the same checksummed `HMSNAP02` envelope. The standalone
formats are Castlevania v1, Metroid v13, Mega Man 2 v4, SMB
QuickNES v4 and SMB Consonance v3; Super Tilt Bro's native campaign selects v5.
The games declared through assertions share `nes-workload-snapshot-checkpoint-v1`.
These plain snapshots use the postcard payload codec inside the framed envelope.
Previous flat envelopes are refused. Native `nes` target modules remain
re-export shims and do not supply separate checkpoint implementations.

Immutable checkpoint images charge the full capacity of each game's emulator-state
vector and the fixed snapshot/observation value. SMB also charges its backend's
portable state and owned work-RAM copy. Shared Consonance pages and sidecar chunks
use physical allocation identities; equivalent separately allocated states remain
separate charges. Freeze clones owned buffers and retains immutable shared state.
Metroid, Mega Man 2 and SMB stream evidence from borrowed fields, preserving the
existing resume bytes without building another owned evidence graph. These shared
implementations also serve native and browser consumers; file publication remains
a native coordinator facility.

## Games declared through assertions

`workload` lowers a game to the searcher's assertion representation
(`search::assertion`, described in the searcher README). Joust, Zelda,
Famidash, Twin Dragons, Punch-Out!!, Donkey Kong, Ice Climber and Pac-Man are
written this way. The other games have their own `target`, `archive` and
`campaign` modules. A game implements `Game`:

- `decode` turns work RAM, and the previous state, into the game's `State`.
- `declare` passes the state's assertions, named values and `sometimes`
  events, one at a time, to a `Declarations` sink. The workload collects
  them in a `Vec<Assertion>` to form the catalog. On each frame `Lowered`
  passes a sink that writes each into the frame's `Record` through a
  `RecordWriter` and sets the satisfied `sometimes` bits, so a frame builds no
  list of assertions and the slot count stays in a register until the frame
  ends.
- `setup` is the power-on tape, `setup_pokes` the RAM writes after it and
  `start` the tape after those (Famidash pokes its level and starts it);
  `genesis` checks the state they reach. `genesis_id` names that state in
  the workload's emulator identity, so a recording or checkpoint from another
  start does not resume here; Famidash names its side, level and goal.
- `CHORDS` is the controller vocabulary. `STRATEGY` is the TOML strategy,
  compiled against the catalog the declarations form.
- `moves` declares a forced move on a state, a transition the game scripts
  rather than a search decision, or an `Offer`: a choice site the searcher
  answers, its default label and the move each label plays. `move_sites`
  names those sites and their labels. `play` gives the move's next chord and
  what follows it, and `move_frames` bounds its frames. Famidash's level
  advance and Lucky Draw's in-level play are the forced moves, and Lucky
  Draw's `wait` the one offer. The setup tapes stay before the genesis, because they
  run once per machine, `genesis` checks where they arrive, and Famidash's
  level poke is not controller input.

`Lowered` builds the catalog from the declarations of a decoded all-zero RAM,
compiles the strategy, and turns a state into a `NesObservation`: its
`AssertionKey`, the `reject` line that holds, whether the goal holds, and the
bitmask of the `sometimes` declarations that hold. The strategy must name a
goal, and a game has at most 128 `sometimes` declarations.

`NesWorkload<G, M>` is the `Workload` implementation for every such game, with
`NesTarget<G, M>` as its target. An action stops at the first frame the
strategy rejects or that meets the goal, so the endpoint is the stopping frame.
The target replays the hold cut to that frame only when a snapshot or the next
action needs the machine there, and checks that the replay reaches the same
work RAM, so a terminal stop runs its chord once. The target reports an
interior observation when the compiled key changes, at a stopping frame and at
the end of each action. Each is an `ActionObservation`: the `NesObservation`,
the action's cost in frames on the last one, and the records of the frames it
covers when the workload keeps records. The cost is serialized with the
observation, so a browser worker's job outcome carries it; the records are not.
The serialized cost is part of each job's `result_sha256`, and so of
`stream_sha256`. A binary built before the cost was serialized gives other
values of both for the same decisions, so a comparison against such a binary
reads `decision_stream_sha256`, `decision_stream_records`, executions and
frames emulated, never `stream_sha256`.
`NesWorkload`'s `action_cost` sums the observations' costs, so the archive and
the records file charge an action the frames it ran. An action a stop cuts
short costs the frames up to the stop, while its planned cost
(`planned_cost_fn`, the chord's hold) counts the whole hold. A snapshot holds
the `NesObservation` and the accumulator alone. A goal frame is a terminal objective, and a rejected
frame is a doomed terminal. Named progress reports each `sometimes`
declaration by name with its first execution. The strategy's policy identifier
is part of the recorded workload policies, and `with_strategy` builds the same
workload under another strategy. The `Arcade` trait gives a game state's
arcade view, motion sample and cleared count, and the draw mixture, and
`NesWorkload` implements `ArcadeGame` from it, with `MEMORY_BYTES` naming the
memory the live feed reads. `joust/mod.rs` holds Joust's decoder and
declarations and `joust/joust.toml` its strategy; `zelda/game.rs` and
`zelda/zelda.toml` do the same for Zelda, and every other such game has a
`game.rs` and a TOML strategy in its module. `famidash/lucky_draw.rs` is C-side
level 24 (Lucky Draw) from its level select: every attempt's outcome is fixed by
the game's random register when the level starts, so the game offers a `wait` of
1 to 64 idle frames before the start, counts the frames waited in `menu_wait`,
and its strategy keys each level-select state on it. The default answer is one
idle frame: a start from a level-select state always replays the same attempt,
so a job that keeps the default moves on to the next phase instead. No in-level
input reaches the register, so in-level play is a forced move: the start plays
on, idle, to the attempt's death or win inside one move, and no in-level state
reaches the archive.

An action also stops at the first frame whose state declares a forced move.
The target plays the move to its end inside that action and reports the
arrival state as the action's last observation. The move's chords are absent
from the tape: replay and films rebuild them from the state. Its frames count
as execution work, in the frame count and in the action's cost, which the
records file and the archive both charge. A move that runs past its bound ends the
action as a doomed terminal and reports the `always` violation
`forced_move_within_bound` with the job's input, under `violations` in the
archive report and the diagnostics, so the input and the run's identity
replay the job.

An action also stops at the first frame whose state makes an offer, and the
next action from that state is the move, so the target replays the hold cut to
that frame at once. The workload asks the offer's site
through `Chooser::choose` and stores the answer as a move chord: the label in
`buttons` and 255 minus the game's site index in `hold_frames`, a hold no
pressed chord has (`move_chord`, `move_answer`). The target plays the chosen
move like a forced move, keeping the record of each state it decodes. A chord
at an offer that names no offered label, as after shrink, plays the default and
is stored as the default's move chord (`Offer::played`); a supplied tape keeps
the chord, and the target records the frames of the move it plays as the
action's cost, which `action_cost` and the default `shrink_cost` read. A move chord where no offer stands, as when a
table or splice copies one, is resolved as a chord. Films decode each frame from the state before it, as the
search does, so a state that counts frames, such as Lucky Draw's `menu_wait`,
ends a film where it ends the run. `action_choices` reports a move chord's
answer, so a job's first ask at that site can hold its parent's last answer.
A move chord's planned cost (`chord_time`) is the frames its answer plays
before any forced move it leads to, as the game's `Game::answer_frames` gives
them (one unless the game says otherwise): Lucky Draw's idle wait plans its
frames and its start the press and the start wait, the level being a forced
move. The archive and the shrink objective charge the recorded cost instead,
every frame the move and the forced moves after it play, so Lucky Draw's start
also costs its level. Two routes that wait the same frames cost the same, so
the older entry keeps its cell. The
records header's catalog names the chord sites and the game's move sites
(`Lowered::records_catalog`), and `nes-rekey` checks records against it.

Chords are choices too (ours). `sample_alphabet` draws an unresolved chord
(`chord::ask`, hold 0) that carries the chord drawn before it, and
`execute_job` resolves each one through `ChordVocabulary::resolve` against the
last chord the job ran or, before the job has run one, the carried chord: a
job restored from its parent's snapshot edits the chord its parent ended on, as
`change_one_control` does. A move chord is never carried. The tree has five
sites:
`chord_edit` (keep, direction, a, b, tap; default keep, asked with
`choose_weighted`), then `draw` sites for the value: `chord_direction`,
`chord_fresh` (a chord when there is no chord before it in the vocabulary),
`hold_stratum` and `hold_frames`. The edit weights are odds - 1 for
direction, A and B and 4 for the tap, so under `reproducing_profile` (contrary
(3 · odds + 1) / (4 · odds), or 3/4 without a tap, and no hold) the tree draws
each edit and hold with the shares of the `change_one_control` draw the other
games keep. That profile is the default policy, `with_choice_policy` sets
another, and the evaluator's `choices` request field passes an arm's policy
through. The chord draw policy is `chord_tree_v1`. A job without a plan, a
continuation, replays recorded chords and never meets an unresolved one. An
unresolved chord matches no archived chord, so the searcher's skip of a
suffix whose every prefix is archived does not apply to these games.

`examples/chord_equivalence.rs` checks that claim for a game: it draws
1,000,000 chords through `ChordVocabulary::draw` and 1,000,000 through the
tree under `reproducing_profile`, from the same starting chords, and compares
the share of each button bit, chord and hold length (a failure is |z| above 4.5
or a value drawn on one side only). It also compares the tables of consecutive
pairs, (previous chord, next chord) and (previous hold, next hold), with a job's
first draw paired with its parent's last chord. Pairs within a chain are
dependent, so each table takes one pair per chain, at the chain's index modulo
the chain length, and both sides take them at the same steps from the same
starting chords. A chi-square homogeneity test judges each table, with cells
under 50 pooled, and a table fails above Wilson-Hilferty z 4.5. Run it for one
game with
`cargo run --release --example chord_equivalence -- 7 joust`, or with no game
name for every game in its `GAMES` list; a game joins the check by an arm in
its `vocabulary` match.

`with_records` hands the run's records to the searcher's `RecordsWriter`: the
target keeps the record of every frame it decodes while recording, each
interior observation carries the records of the frames since the one before,
and `merge_action_evidence` passes them per action with the job's sequence
number. `Reporting::direct_records` closes the file or starts another one, so a
replay of the stream writes its own copy. The snapshot identity hashes the
policy identifier with the snapshot, so saved states taken under one strategy
are rebuilt by replay when a run resumes from them under another.

A strategy with `highest` or a latched stage keys a frame from its path, so the
target advances the accumulator at every observed frame, `NesSnapshot` and the
snapshot checkpoint carry it, and a restore puts it back; the frames inside a
forced move are decoded but not observed. `workload/shadow.rs` compares a
hand-keyed target with a strategy. `Shadow::new` lowers the game's strategy and
`hook` gives the hand target a `ShadowHook`, which it calls with the genesis
RAM (`start`), the frames of each action up to the departure of a forced move
(`advance`), the RAM after each step chord of the forced move (`forced`), each
snapshot it takes (`saved`, which returns a `ShadowToken` the hand snapshot
keeps) and restores (`restored`, with a reference to that token, or `None`
where the snapshot has none), and its raw key, or `None` where it has none,
after every action (`observe`), including an action that ends the path: the
hand target observes a dead or doomed state with `None` and a goal state with
its key, so the shadow checks that the two targets end paths at the same
frames. In a campaign that means each state a job reaches, replayed actions
included, after the candidate's snapshot; `current_key` keys only the root, so
a target that observes only there compares one state. The hook pairs a frame
that a `reject` line matches with no strategy key, since the strategy target
ends there as a doomed terminal, and keys a goal frame, even one a `reject`
line matches, since the strategy target ends it as a terminal objective. The
strategy target archives neither frame, since a rollout admits only runnable
states. A forced move cut short at a goal or reject passes `hook.forced` the
steps up to the stopping frame, the last being where it stopped. The report
counts an action applied before the previous action's state was observed as
`unobserved`; a state that ends a path, observed and then left by a restore,
counts nothing there. A restore brings back the cursor saved under its token,
so two paths that reach the same RAM at the same frame keep their own lineages.
A `ShadowToken` is opaque and does not serialize, so the hand snapshot keeps it
in a `#[serde(skip)]` field and a snapshot read from a checkpoint restores with
`None`. `None`, or a token another `Shadow` issued, counts as an unknown
restore. Clones of a token share one handle, and when the last clone drops, the
hook forgets the cursor saved under it, so the saved cursors are those of the
snapshots still alive. The handle holds a weak reference to its `Shadow`, which
keeps the cursors in a `Mutex` behind the `Arc` its hooks share, so a snapshot
can drop on any worker. The hook decodes each frame from the one before,
decodes a forced move's steps in a chain as the target does and observes only
the arrival, advances its own accumulator at the observed frames, completes the
raw key against the previous one with `ArchiveKey::complete`, and counts the
pair in the searcher's `ShadowTable`. The first `observe` after a save or a
restore, with no action between, replaces the cursor saved under that token
while the token lives, so a candidate whose snapshot an admission probe
restores before its key is observed keeps the key. `Shadow::report` returns the
searcher's `ShadowReport`. The hook records a hand lineage at every candidate,
where the archive records it at admitted entries only, so a hand key whose
lineage holds more than the parent key can disagree there.

## Arcade live-feed observations

Arcade adapters expose each archived snapshot's game-frame count for exact
ready-state and lookup replay. Feed batches retain level/objective completion
points separately from sampled motion, so downsampling and terminal delivery
can preserve progress. Sampling affects presentation only. The native live
service uses the existing campaign coordinator and full admission-observation
interface; search selection and admission policy remain unchanged.

## Punch-Out!! victory and loss

A Punch-Out!! run is won when the opponent index (RAM `$01`) is 14 and the mode
(RAM `$00`) is `$FE`, `$FC` or `$FA`. The post-fight routine at `$ADF7` reads the
table at `$A10D`, whose Mr. Dream entry is `0E 0E`, so a win and a loss both set
`$01` to 14. Its caller at `$A90D` takes the win path only for those three modes.
After a win the mode stays `$FC` through the credits. After a loss to Mr. Dream
the mode is `$FD`. The title-screen attract demo writes `$01 = $13` at `$A50A`
and never reaches a win mode.

Mac is dead when he is knocked down during a fight (`$05 = 2` with `$00 = $01`),
when the mode is `$FD` (lost) or when it is `$FB` (lost by technical knockout).
A dead or won state is terminal. The count of defeated opponents is 14 only in a
won state. Otherwise it is `$01` capped at 13, so a loss to Mr. Dream and the
attract demo's `$01 = $13` both count 13.
