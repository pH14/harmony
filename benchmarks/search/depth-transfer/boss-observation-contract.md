# Snapshot-local boss observations

This contract measures consecutive raw observations. It supplies neither a
search policy nor an exact lifetime-damage oracle. Search snapshots, keys,
terminal predicates, actions and rewards stay unchanged.

## Source argument

At disassembly commit `4270d57f9468daebdeea485686e31e26218a780c`,
[Bank07](https://github.com/nmikstas/metroid-disassembly/blob/4270d57f9468daebdeea485686e31e26218a780c/Source_Files/Bank07.asm)
`GetEnemyType` stores type-byte bits 6–7 in `EnSpecialAttribs` and the lower six
bits in `EnDataIndex`. Bit 6 identifies a boss; bit 7 changes the initial HP
variant. `LF536` preserves prior status OR attributes in `$040C + slot` before
setting status 6 and replacing the special byte with a hit timer. Zero HP sets
status 3. `LF4EE` restores the saved attribute and status bits after the timer.
The saved byte can predate the current enemy in normal states.

For gameplay mode 3 in areas `$12` and `$14`, classify each of the six enemy
slots using this state-dependent rule:

| Current status | Effective attributes | Additional requirement |
| --- | --- | --- |
| 1 or 2 | current special AND `$C0` | effective bit `$40` set |
| 3 or 6 | saved `$040C` AND `$C0` | saved low six bits are 1 or 2; effective bit `$40` set |
| Anything else | unavailable | no classification |

The visible identity is `(area, slot offset, data index, effective attributes)`.
It needs no loader flag, room target, action history or remembered entry event.
An unavailable classification is not proof of boss absence. Each input contains
WRAM and cartridge RAM from the **same paused frame boundary**. Existing held
chords expose cartridge RAM only at their endpoint; mixing that with earlier
WRAM is invalid for this contract. The standalone probe uses one-frame actions.

## Interval definition and boundaries

Let `K_t`, `H_t`, and `S_t` be classified identity, HP and status at caller frame
`t` in epoch `e`. A comparison requires observations at `(e,t-1)` and `(e,t)`,
both classified with equal `K`, and neither HP equal to the unavailable sentinel
255. Define the reported quantity as:

- `H_(t-1) - H_t` when HP decreases and `S_t` is 3 or 6;
- zero when these comparable HP values are equal;
- unavailable otherwise, with a reason such as baseline, identity change,
  inactive/unclassified slot, HP increase, or unexplained decrease.

Only six previous classifications and one clock stamp are retained. Every
restore/reset must clear the observer or change its epoch, and seed a new
baseline from the actual restored state before another frame advances. A
baseline read consumes no emulator frames and contributes no damage event.
Frame gaps, repeated clocks and overflow cannot establish an interval. Missing
or invalid raw bytes fail decoding. No route total is restored or inherited.
Aggregates retain unavailable evidence: if no interval is comparable, the HP
loss sum is null, not zero.

This gives reset noninterference: changing any pre-reset observations cannot
change reports following an identical restored baseline and suffix. For a
continuous sequence, initializing a fresh observer at any observed cut gives
the same subsequent intervals as the uninterrupted observer. These properties
are executable checks, not assumptions that the caller reports every restore.
The planted unreported-restore counterexample deliberately creates a false
8-HP decrease; notifying the observer removes it.

## Qualification and remaining limits

[F03](f03-registration.json) reads the saved byte in two independent full
120,540-frame public-reference replays. Their raw traces are identical, and
removing the new columns reproduces F02 byte for byte. The Python reference
model keeps all 98 HP decreases (Ridley 62/140 HP, Kraid 36/96 HP), with no
unexplained changes in those two observed lifetimes. It verifies every one of
28,521 adjacent stored-observation cuts. The Rust implementation checks the
981 fight-frame slot-0 projection and the same state/reset counterexamples.
The projection has no controller inputs; its provenance is checked against the
complete [compressed raw trace](f03-boss-area-frames.csv.gz).

These are reference-emulator observations and offline model restarts, not proof
of arbitrary QuickNES snapshot restores during a positive fight. A separately
registered native replay must check actual restores, preserved legacy bytes,
and unchanged emulator state before this observer guides tape diagnosis.

`IsSlotTaken` can reuse a slot based on `$0405` even without a sampled inactive
status. Equal visible identity therefore does not alone prove one uninterrupted
enemy instance. `LF85A` initializes HP and the hit routine decrements it; there
are also bank-specific HP writes. This study has not established a global
no-reload or HP-write proof. Report **observed HP decreases**, not exact total
combat damage, a universal lower bound, or a complete campaign encounter count.
Positive lifecycle agreement on two fights cannot remove that limitation.

No new performance-search allocation follows from observer qualification alone.
The next scientific question is whether frozen surviving development tapes
reach a classified encounter and, if so, whether they show any comparable HP
changes. A small selected set cannot establish campaign-wide absence.
