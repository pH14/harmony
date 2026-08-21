# Sol World 8-4 p153 class-uniform room harvest v1

Status: preregistered after the registered p153 room-count mask census
result commit and before implementation sealing, recipe materialization, ROM
loading, or live emulation.

## Question and scope

The room-count mask census showed that the twelve registered archives hold
5,210 states on pages 5 through 9, two thirds of them on the ground, and that
exactly one of those states stands on an enterable pipe, which leads back to
page 1 of the same area. The concentrated selector spends three quarters of
its draws on the deepest progress band, so states at other pages and heights
are rarely expanded.

This harvest changes parent selection only. Under the new library policy
`ClassUniform`, each frontier draw first chooses one occupied state class
uniformly among the classes that still hold an unexhausted member, then
applies the unchanged concentrated recency draw within that class. A class is
the key tuple `(world, level, rooms, progress, player_y_bucket)`, which is
made only of values the archive has already observed. No class, button,
position, or route is preferred. The one-in-four uniform draw over all active
entries, the exhaustion threshold, and the counter reset are unchanged. The
integrator approved this selector on 2026-08-21 as the diversity-first
counterpart of the promoted concentrated selector; the promoted selector
remains the base draw inside every class.

It asks whether a bounded continuation from p153 with diversity-first
selection and the room-count term yields a final-active live endpoint with a
strictly greater full watermark. It is a harvest, not a policy comparison.
ADOPT carries one exact verified champion; STOP authorizes no relaxation or
rerun.

## Frozen provenance and source

- Code base and registered mask-census result commit before this experiment:
  `034f666b796b486c77efbcecf03e42c27480db2e`.
- Authorizing mask-census preregistration
  `89be84fa5f8cbf9450293350b310037a3bab95c5`, implementation
  `d9e6de7f49f3bab919a70cb22209f27d2750f7ea`, report SHA-256
  `a7dae996ae01666f07b730734c245bfa7bb7eea11c17777708b20c5e7a5bff19`,
  and registered-result document SHA-256
  `ee336b8474ffb33dfd2830a90c0a95d5c8375a914d35e72ce06bcf3fe4336975`.
- Launch-only source path, source hashes, source endpoint, Frozen key with
  `rooms:1`, source room `(3,5)`, WRAM/snapshot hashes, final chord,
  milestones, ROM hash, and the room term: unchanged from the room-count
  harvest preregistration.

The binary may read only the compact source, the ROM, and its current
executable. It must not read any prior report, recipe, snapshot, or other
campaign/canary artifact. Provenance values are constants only.

## Frozen recipes and mechanics

Seed label `sol-restart-w8-4-p153-class-uniform-room-harvest-v1` has SHA-256
`d20ef06bde7241821019e6c05c375a3c51dc630ea1b03af1695febaf85c96836`;
its first eight bytes interpreted little-endian are master seed
`9385909398036025042`.

Lanes, draws, lane seeds, source occurrence indices, selector seeds, the
opaque source-occurrence chord draw, recipe identity, and lane projections
are derived exactly as in the room-count harvest preregistration with this
master seed. The registered recipe identity SHA-256 is
`124369c31f3f09c39a3275cba6169331a40a9a3d03d149b0e63ddc1186a7ab1e`
over 400,257 bytes. Require all 12 projection byte vectors pairwise
distinct; collision is integrity STOP without retry.

Baseline replay, source probe, per-lane archive initialization, action
limit 4,096, archive limit 513, Frozen key extended by `rooms`,
ProbeAtAdmission45 masks `[00,01,81]`, FewestActions replacement, absent
waypoint/snapback, the draw loop, admission, room-set rule, and selector
accounting are unchanged from the room-count harvest. Parent selection uses
the real library policy `ClassUniform` with no pin. Entries at progress 153
are ordinary classes and may be selected.

## Bounds and frozen decision

Bounds are unchanged: maximum lineage `3,576+512=4,088 < 4,096`; action
work at most 737,280 and probe work at most 829,440 frames; source replay
168,594, source probe 45, one baseline plus 12 worker setups 4,693. The
checked hard total is **1,740,052 frames**. Wall time has no authority.

Eligibility, ranking by full watermark `(world, level, rooms, progress)`
descending then action count, semantic input SHA, lane, and id, the ADOPT
rule against `(7,3,1,153)`, integrity STOP, the no-rerun rule, and the
completion disclaimer are exactly those of the room-count harvest
preregistration. Separately and diagnostically, report the number of draws
whose endpoint room value differs from `(3,5)` while still in `(7,3)`
and the number of distinct selected classes per lane.

ADOPT authorizes only the exact champion as the next source. STOP closes this
exact combination from p153 without rerun or enlargement and does not by
itself promote or reject the class-uniform selector.

Emit create-new canonical NDJSON with header, baseline, recipes, 12 lane
records, classification, and summary, binding prereg/source/ROM/executable/
bin/module/config/recipe/trace/body/whole-file hashes. Paths, timestamps, and
completion order must not enter canonical bytes.

## Registered result

Preregistration commit `f9eda846ba749f2fedcf680723998ab0bd1c1487` and
implementation commit `1b739de2` (full hash in git) used module SHA-256
`c219cadd969bfb5d529198f189795aa82780b070388577383818ffbf7a8d571d`,
bin-source SHA-256
`7c9d0785f6212f53e147d850174c4ec2ef9e369b333e064ab55d5e4476dcbc6c`,
and release-executable SHA-256
`a75865d5b330d2582fb842b72478d8a5a5ec131dc28d1cdccb84fc1b69f1e675`,
built once offline and locked from sealed source archive SHA-256
`da8938287e0d67c1a8fa0b886a7b74d2dd7aaa9b9c4575449170b142b0915251`
under `/root/harmony-smb-sol-w8-4-p153-class-uniform-1b739de2`; the sealed
tree matched the implementation commit file for file. The sealed recipe was
400,257 bytes with SHA-256
`124369c31f3f09c39a3275cba6169331a40a9a3d03d149b0e63ddc1186a7ab1e`. The sole
run (systemd unit `harmony-smb-sol-w8-4-p153-class-uniform-1b739de2`,
`Restart=no`, exit 0, no restarts) produced 17 NDJSON lines in registered
order, 812,566,867 bytes, whole-file SHA-256
`c41a4fda904677fec6b761b2256ed1e7d7e6e8438f75cab71564bf68fd766356`,
and body SHA-256
`189e317a03a31566b668a78be7616eacef710f788ebe4b47e1a1fe0c2ef2c245`.
Standard error was empty and standard output bound the same report hash.
The baseline reproduced every registered source datum.

The registered verdict is **STOP**. All 6,144 scheduled candidates executed
and were selected and accounted exactly once; 5,880 final-active ordinary
entries were eligible and none exceeded `(7,3,1,153)`. The best eligible
entry reached `(7,3,1,124)`. Checked work was 4,693 setup + 168,594 source
replay + 45 source probe + 281,165 action + 274,896 probe = **729,393
frames**, below the 1,740,052-frame cap.

Diagnostic only. No draw left room `(3,5)`; 33 endpoints were dead. Each
lane selected 65 to 78 distinct classes; uniform-path draws were 113 to 143
per lane and class draws 369 to 399, with no counter reset. Live endpoints
sat on page 5 (1,642), page 6 (3,951), and page 7 (518). Final entries
covered player y buckets 5 through 11 on pages 5 and 6 and reached page 7
only 508 times, never page 8 or 9. Spreading draws evenly across classes
removed the depth pressure that previously carried lineages back to pages 8
and 9, so the one known pipe at progress 124 and any states beyond it were
not reached within 512 draws per lane.
