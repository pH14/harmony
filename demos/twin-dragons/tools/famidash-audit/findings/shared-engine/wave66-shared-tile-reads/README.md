# Steep slopes and spikes share each tile read

The normal-wave66 extension exposed duplicated work in the combined engine:
its slope helper reread both diagonal tiles before the spike sweep queried
them again. The added work repeated an Aftercatabath movement frame and changed
the subsequent input alignment. This integration feeds both collision kernels
from the same existing tile reads. It also extends the query gate to normal
wave, retaining diagonal queries when both endpoint tiles are empty.

These three-header patches apply to the named combined ceiling/Kappaclysm V2
prototype bases, with before/after file hashes in each manifest. They are
integration experiments, not pristine upstream patches. The separate
[both-size source proposals](../wave66-both-sizes/README.md) contain the isolated
pristine-source implementation and original-release normal-wave reproducer.
Level data repairs are separate proposals; these patches change no tile data.

`check_wave66_tile` consumes the sweep's collision class, tile origin and
bounded movement. It performs no map reads. History keeps its per-player mode,
size, reset, teleport and motion-bound guards. The spike helper retains its
selected bank; the new dispatch wrapper must be mapped with its caller.

All nine retained release negatives pass QuickNES and FCEUmm with unchanged
approaches and normal resets: normal-wave66, mini-wave66, both normal-wave45
corners, Aftercatabath, Xstep and three Kappaclysm cavities. The normal-wave66
combined input dies at 939 with all 938 preceding frames unchanged. Bank-aware
CPU review changes 32 surviving interior samples to zero; its death-frame
sample inside the fill remains explicitly classified as dead. Aftercatabath
again dies at 483 with all 482 preceding frames unchanged.

The actual polygon headers pass 64,850 rational clipping cases and 65,536
signed products each. Assembled query gates pass 131,072 combinations on release,
current standard and HUGE, including both sizes, players and empty-endpoint
diagonal crossings. Selected direct calls also pass physical PRG-bank checks.
Decoded geometry matches 109 release, 170 current-standard and 173 HUGE levels.
All ten build configurations preserve the complete sound-effect segment.

The retained current clearing control completes in 4390 standard frames and
4389 HUGE frames. Positive retiming preserves every mechanical transition:
standard adds two duplicate frames to the 4388-frame reference; HUGE adds one.
Full FCEUmm replay and bank-aware standard CPU review pass, with 529 wave probes,
no wave fill entries and no source mismatches. Mesen verifies HUGE completion
with only intro frame 6 differing, excluding physical level identity. Other
modes and objective routes remain under source intent review. Negative inputs
were never retimed. A new native-current normal-wave66 negative remains open.

The earlier current V7 HUGE integration had an introduced banking defect:
`check_spike_motion` in PRG bank 240 directly called a helper in bank 248.
The same CPU address instead entered the middle of the slope-motion routine,
reused the freshly updated cache and falsely killed at frame 795. Standard
linker segments shared a physical bank and hid the error. The corrected current
patch keeps the wrapper in `COLLISION_BANK`. This was an integration regression,
not a newly discovered upstream Famidash bug.

The retained opcode traces bind actual PRG mappings and have zero opcode/ROM
disagreements. The read-only Mesen hook patch applies to the pinned emulator;
build it from `Libretro` with `make -j2 TARGET=mesen_watch_libretro.so`.
`retro_famidash_set_opcode_callback` reports CPU address, mapped PRG offset,
opcode and registers without changing emulated state or cycles. The canonical
Mesen core, original CPU source and object were preserved. The bank rejection
log and new direct-call tests retain the failure rather than treating compilation
or equal CPU addresses as proof of correct mapping.

Release V7 and corrected current V8 are the qualified build configurations
for these checks. The 109-level Dissonance round 96 uses release V7 and is still
under assessment. Other source findings remain open, so this packet supplies
no clean-round credit or claim that the investigation is complete.
