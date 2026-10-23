<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# machine

`machine` supplies execution drivers for the NES workload package. The
`Machine` trait exposes snapshot, drop, branch, replay, run, and read
operations. Environments and decision answers are opaque byte blobs, so a
NES adapter can drive either backend through the same execution vocabulary.

`StopReason` distinguishes terminal deadline, quiescence, and crash outcomes
from surfaced decision, snapshot-point, and assertion events. `StopMask` selects
which non-terminal classes a run returns; terminal classes always surface.
`Moment` and `DecisionId` identify points on one machine instance and are not
serialized as host timestamps.

`nes` defines the versioned controller-action reproducer format: each action is
`[buttons, hold_frames]`, with holds normalized to `1..=120` frames. The
NES targets interpret this format; Dissonance sees typed workload actions.

## QuickNES adapter

On Unix, `quicknes::QuickNesMachine` loads a private copy of the pinned
libretro QuickNES shared object, validates its revision and supplied SHA-256,
and exposes the core's 2 KiB system RAM. The dynamic loader receives a path to
the descriptor of the exact copy that was hashed. Linux seals an anonymous
in-memory copy before loading it. macOS keeps an owner-only, read-only private
file named through the load, then unlinks it. The descriptor stays open while
its library handle is live, so another core cannot reuse the same descriptor
pathname during that load. Linux checks the loader cache before opening a
private core and checks for a retained image after every close. If a core
stays loaded after `dlclose`, including NODELETE or GNU_UNIQUE cases,
preparation reports an error and the process refuses further private QuickNES
loads. A replacement of the private file name cannot redirect the descriptor.
Each run records system RAM after every frame, and a caller can also name a cartridge
RAM range to record after every frame. Search runs with audio and video disabled.
Replay-only callers can capture video and stereo PCM. A caller that stops
reading partway through a run can keep the video and audio of the run's first
frames and drop the rest.

`prepared_execution_sha256` identifies the code that the native loader actually
mapped before a campaign starts. It includes the hashed private core, the owner
and relative address of every libretro entrypoint, and the mapped read-only
image bytes of the host executable and system libraries. Workers compute the
same identity from their own loaded core before initializing a ROM and reject a
mismatch. This native mode accepts the pinned core's macOS system libraries or
64-bit Linux system libraries under `/usr/lib`, `/usr/lib64`, `/lib`, or
`/lib64`, including libc, libstdc++, and libgcc_s. It rejects any loaded
non-system shared library, including a helper supplied beside the core, as well as loader preload
and audit injection. An ordinary library search path is allowed only when the
resulting mapped images satisfy that policy. The image census is intentionally
conservative: an unrelated third-party module in the process also prevents
native QuickNES admission.

Private core loads and closes check the loader's cumulative image additions,
removals and visible image count before and after the load or close, under one
process-wide lock, and advance one shared approved generation only for
persistent images; any other loader event between two of these checks rejects
the operation and poisons the guard. Every core teardown, on normal drop or
after a failed operation, makes the same check. Every other libretro call
reads one atomic status word before and after the call and refuses when the
guard is unregistered or poisoned; it takes no lock and does not walk the
loader, because a Metroid search makes about three such calls per emulated
frame on every worker and the earlier per-call walk through `dl_iterate_phdr`
under glibc's loader lock cost 4% of worker CPU and most of the workers'
context switches. The exposure window is the span between one lifecycle check
and the next: a library loaded or unloaded through the dynamic loader after a
worker's identity census and before its next core open, close or teardown is
undetected while it is mapped, and frames run and snapshots taken inside that
span are accepted. The loader's cumulative addition and removal counters
record the event even after the library is unloaded, so the next lifecycle
check in the process poisons the guard, every later libretro call refuses,
and no further private core opens. `QuickNesMachine::close` runs the
teardown and the library close and returns that failure; a machine that is
dropped instead keeps the poisoned status and the refusal surfaces on the
next call or open in the process. Code that enters the process without the dynamic
loader is outside both the old and the new check and is the subject of
[issue #679](https://github.com/pH14/harmony/issues/679). A first core may
load system dependencies before any worker is active, and its prepared
execution identity binds those mapped images.

`scripts/build-quicknes-core.sh` patches the pinned core to skip idle loops.
NES games spend much of each frame in a loop that waits for the next
interrupt. At a backward branch or jump, the patched CPU compares the target
address, the registers, and a count of effects with the previous visit. Effects
are memory writes, reads of the $2000-$7FFF register and cartridge range, and
interrupt-flag changes. A match means the loop iteration had no effect and read
only memory it did not change, so every later iteration repeats it until
`clock_limit`, the next interrupt or frame end. The CPU then advances its clock
by whole iterations up to that limit, so emulated state and timing match the
unpatched core. Interrupts are taken between `run` calls, and each call starts
with no recorded visit. The skip is 1.04x to 1.7x faster in search across the
NES games.

The script also patches how the core reads a saved nametable block. A
four-screen cartridge such as Famidash saves 4 KiB of nametables, and the
pinned reader fails on the second half of that block, so such states could not
be restored. The patched reader reads the block in one call. States of other
cartridges read the same bytes as before.

A third patch changes how the CPU dispatches instructions. The pinned core
dispatches every 6502 instruction through one `switch`, so the host branch
predictor sees a single indirect jump and misses about once every four
instructions. The patched core jumps through a table of handler addresses and
repeats that jump at the end of every handler, so each handler has its own
prediction history. GCC merges those copies back unless cross-jumping and GCSE
are off, so the patched file turns both off. Operand reads from RAM and ROM
index memory directly instead of calling the read function. Emulated state and
timing are unchanged. The patch is about 1.08x faster on a Metroid tape built
with gcc 11, and the core produces the same bytes on every frame.

Snapshots contain a format marker, the QuickNES revision, core hash, prepared
execution hash, fixed serialized-state length, and canonicalized core state.
The current marker is `HQNESST3`; older snapshots are rejected. Restore rejects
a different loaded execution, state size, or non-canonical state. The adapter keeps
snapshots in an ordered handle table and resets staged input when restoring.

Portable export builds shared chunks directly from the retained snapshot bytes,
without cloning the full snapshot first. Import moves the exactly sized
materialized buffer into the handle table rather than copying it again. Each
nonempty transfer eliminates one snapshot-sized temporary allocation and copy;
retained bytes, chunk sharing, validation, and handle ordering are unchanged.
The opt-in `DISSONANCE_BENCHMARK_PORTABLE_TRANSFER=1` unit benchmark compares
both paths with their former copying implementations on synthetic snapshots.
Its transfer timings do not measure whole-search throughput.

Portable serialization visits the shared chunks directly, trimming padding in
the last chunk and retaining the same byte-sequence format as a materialized
buffer. It avoids a snapshot-sized temporary allocation during JSON reports
and checkpoint encoding. The opt-in
`DISSONANCE_BENCHMARK_CHUNK_SERIALIZATION=1` unit benchmark compares JSON and
Postcard serialization against the materialized implementation, including
output-buffer allocation and disposal. These are encoding measurements, not
whole-search throughput measurements.

The libretro FFI is Unix-specific. The pure machine types and test loopback
allow the boundary and its bounds checks to run without a shared object.
The Linux musl build includes the NES Consonance adapter but refuses native
QuickNES before loading a core: its loader-generation guard requires the glibc
ABI. A separate GNU frontend remains the native QuickNES execution host.

The consonance adapter delegates boot, setup, branch, replay, run, read, SDK
catalog, and sparse snapshot operations to `consonance-client::Session`. Its
public evidence remains the action observations and portable snapshot state;
host-only control traces are not part of the machine contract. The adapter
defers virtual-time checkpoint hashes until requested, avoiding full-memory
hashing for unused trace entries while preserving explicit state-hash checks.
It discovers the kernel-owned NES observation by its opaque SDK handle, reads it
through `Session::read_observation` at stopped action boundaries, and keeps
only billboard decoding, cached observation state, and the workload-specific
action payload codec.


## Consonance adapter

With the `consonance` feature on Linux x86-64 or arm64, the NES driver runs a
prepared OCI ROM and QuickNES agent in one single-vCPU Consonance guest. It discovers
the publication handle by SDK names, validates the shared `nes-protocol` codec, and reads
observations at stopped action boundaries. Workload initialization and evaluation
belong to the adapters in `nes-workload`.

Setup and each action allow 2 seconds of virtual time on x86-64 and 20 seconds
on arm64. These architecture-specific budgets are passed through the client
session configuration and included in the execution identity. The NES adapter's
outer execution identity derives from that exact session configuration and its
ordered setup payloads. It therefore includes the loaded Harmony executable
used by the session and fails closed when that executable cannot be identified.

Native snapshots preserve their existing serialized emulator format. Consonance
snapshots use the explicit `consonance-whole-vm-v2` shape owned by
`consonance-client`: setup base, execution identity, sparse pages, and opaque
sidecar bytes. Imports require the same execution identity; cross-backend
comparisons reconstruct state through actions. The sidecar uses portable
snapshot format 3, recorded separately in execution identity; older portable
formats are rejected explicitly.

Cartridge work RAM is exposed at `$6000` for workloads that need it. The
`nes::with_cartridge_ram` helper sets the iNES declaration on an in-memory
execution copy, preserving the original ROM identity. QuickNES's declared
cartridge RAM is initialized to `0xff` before gameplay, matching its undeclared
RAM initialization and preventing allocator contents from entering power-on
state. The same checked FFI path runs through the Miri loopback tests.

