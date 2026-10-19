# Host SHA qualification

The separate [native math corpus](native-math/README.md) retains one static
executable and exact arithmetic observations across AMD/Intel hosts. It reports
bounded agreements and differences while preserving standard math support and
advisory replay CPU warnings.

`python3 scripts/qualify-host-sha256.py --check` first builds `oci-support`, `unison`, and `searcher` independently, with no VMM feature
unification. It reads compiler-artifact features from each actual target build
and requires ARM acceleration on native ARM64 Linux/macOS, existing defaults on
x86, and no forced software backend. Removing a component's ARM declaration must
fail this check even when another component enables it.

It then builds the production Unison and searcher beside same-source copies
using a renamed software-only SHA crate, all in one executable. The software
source archive is verified against the lockfile checksum and compared with the
production SHA source. Unison machine state hashes, postcard result digests, and derived worker seeds must agree. These checks preserve the existing digest
algorithms, serialized input bytes, and seed derivation.

Omit `--check` for nine alternating native/software timing pairs per operation.
The Unison hash includes its complete 64 KiB machine state; postcard digests
include serialization and hex formatting. Inputs range from empty and SHA padding boundaries to
32 MiB. Every result is checked; there are no timing thresholds in CI. The output
records the executable digest and compiled SHA features. These operation costs
are not a measurement of whole-campaign throughput.

The compiler checks cover all independent dependency owners. Native NES and
other compositions inherit acceleration through these libraries, while VMM
compositions retain their existing explicit selection. The client's image hashes
are checked by its in-process feature, which already brings in the VMM, so the
client needs no additional acceleration declaration. Root and standalone
workspace lockfiles keep the same SHA version. No CPU policy exposed to a guest
changes.

Run `cargo fetch --locked` and
`cargo fetch --locked --manifest-path dissonance/Cargo.toml` before using the
offline qualifier. Both ARM host compatibility jobs and the x86 Snapshot and
Restore job run the bounded check. The VMM's separate SHA qualifier retains its
streaming, alignment, and full-state checks.

## Consonance execution cells

`consonance.json` declares the required production execution cells consumed by
`../qualify-consonance.py`. The hardware workflow builds one static X1 executable,
runs that retained artifact on two hosts, then requires every oracle log and
compares the execution digests. The binary embeds the guest bytes; identity
records bind it to source, instruction contract and registry. Receipts retain
host CPU/kernel identity, the DMI product UUID, and exact test logs. The runner
reads the firmware UUID into the required `--host-id-file`; duplicate instance
UUIDs are refused even if hostnames differ. Cloned runner images may reuse a
hostname. Distinct VM UUIDs establish separate virtual host instances, not
physical CPU diversity. Unknown fields, duplicate JSON
keys, missing oracles, repeated hosts, mismatched artifacts and contradictory
digests fail the check. A failed comparison records an inconclusive or divergent
verdict rather than a passing cell.

The X1 fixture explicitly initializes its code, stack and task-register segment
types before guest entry. KVM reset defaults differ between AMD and Intel;
retaining those defaults would compare different initial states. Checkpoint
hashes retain the segment fields without masking them.

Each oracle receives its own `X1_DIAGNOSTICS_DIR` below the report's
`diagnostics` directory. Numbered run directories retain the initial and setup
CPU states, each checkpoint's saved and logically canonical CPU states, exact
hashed state text, RAM bytes, virtual-time fields and hashes, followed by the
normalized log and interrupt schedule. Direct invocations must select one oracle
with `--exact` and set this variable to a distinct existing empty directory for
that oracle. Sharing one directory across tests is refused, as are existing run
directories, to preserve earlier witnesses. Files already emitted survive a
failed oracle process, and the hardware workflow uploads the report tree even
when execution fails. These files diagnose contradictions; they do not change
the compared digest or qualify a failed cell.

The registered cell names its finite instruction surface and checkpoint state.
It does not grant qualification to arbitrary instructions, other backends or
snapshot migration. Adding a relation requires a production adapter and evidence
for that relation; local repetitions alone cannot satisfy a cross-host cell.

## Composed Linux continuation

`linux-continuation.json` and `../qualify-linux-continuation.py` define a
separate x86 KVM Linux cell. Its retained static test executable drives the
production VMM with an exact kernel and owned continuation initramfs. The
bundle binds source, executable, both guest images, the guest CPU contract and
registry by SHA-256. Execution never rebuilds the test executable. Every
receipt retains the DMI instance UUID, CPU vendor and full `/proc/cpuinfo`,
host kernel, execution log, artifact hashes and exact cut file hashes.

The local arm first boots without capture, then compares a second boot with
capture-and-continue. Capture explicitly retires the outstanding hardware
completion through `prepare_snapshot`. Two fresh restores must produce the
same guest record, including one with a 32 MiB host working set touched
between guest steps. This varies host memory/cache pressure; it does not
claim globally cold caches or all possible host loads. A reused restore
first takes a guest `B` detour that changes memory, XMM15 and x87. It advances
to a completion-free idle exit before restoring the original cut. Intel must
reproduce the record. AMD's existing reused-vCPU admission refusal must be
exact, preserve the detoured state and RAM, and leave the guest usable.

The guest computes its own memory checksum, reads live SIMD and x87 values,
waits on a Linux virtual timer, and reads guest clock and entropy values.
Known initial memory and register constants prevent a broken fixture from
comparing two empty observations. Planted RAM corruption and an omitted SIMD
value in an otherwise valid canonical XSAVE image must each change the
guest record. These controls execute in the same retained executable as the
positive arms; snapshots and their shared state-hash inventory are not the
observation oracle.

The cell separates three acceptance obligations. O1 enables production VM-state
hashing and compares the complete modeled VMM hash at READY and DONE boundaries and at termination
across same-seed boots, capture, fresh/reused restore and pressure variation.
A planted byte outside the emitted memory buffer must change the hash while
preserving guest observations and continuation work. O2 checks the reviewed
4096-byte memory checksum and SIMD/x87 constants; it does not call the time or
entropy outputs specification goldens. O3 repeats seeds `0028c0ffee5eedc0` and
`0028c0ffee5fedc0`, compares each seed's complete state and output, requires
entropy to differ and every nonentropy readback to agree, and requires equal
positive continuation event counts and virtual nanoseconds. Both runs continue
past DONE to the exact `Terminal(Idle)` outcome after the owned guest requests
Linux `POWER_OFF`; arbitrary errors and time limits cannot count as completion.

Raw `VmState` capture restores logical machine state, not cumulative event
history. The additional production `ControlServer` path captures and exports
its full portable snapshot, imports and replays it in a fresh server, then
compares both complete VMM hashes and the separately preserved history count
and digest at READY, DONE and termination. All checkpoints retain absolute
V-time, and both the READY→DONE and DONE→termination intervals must have positive
event count and V-time deltas. Capture itself must preserve both. These paths retain distinct
oracles rather than copying an expected history into the raw VMM fixture.

The exporter retains RAM, encoded raw VM state, raw checkpoint oracles, the
composed portable snapshot and its oracles, the expected guest record and
source backend core class. Importers validate every file hash before running
both raw and composed portable restoration. A raw core-class mismatch must
produce the public `ContractMismatch` admission error, leave CPU, RAM, virtual
time and serial state unchanged, then successfully restore and continue an
admitted local snapshot in that same target. Arbitrary errors never count as
unsupported combinations. Accepted imports reproduce READY/DONE/terminal whole
state hashes, exact guest observations and continuation work, and the composed
portable path's cumulative event history. The aggregate rejects a changed hash
or history even if every guest-emitted field agrees. Missing oracles, a repeated
seed, stable entropy under changed seeds, zero/unequal work, or an unexpected
terminal outcome refuse qualification. A native planted control changes a
modeled POST-port write after DONE and restores the instruction before shutdown:
terminal state and output reconverge with equal count/time, while the terminal
history digest and composed oracle must distinguish the executions.

X3 registry, artifact identities, receipts and aggregate verdicts use schema 3.
The raw and composed oracle exports contain all three checkpoints plus both
work intervals; schema-2 exports/receipts cannot satisfy this extended scope.
This fixture schema is independent of the production EventHistory and portable
snapshot versions. Retained older hardware evidence keeps its original scope;
this extension does not resolve the cross-vendor state counterexample below.

Build and run on Linux x86-64 with Rust's musl target and `musl-gcc`:

```sh
bash consonance/harmony-linux/linux/build-continuation-initramfs.sh
python3 scripts/qualify-linux-continuation.py bundle x3-bundle \
  --kernel consonance/harmony-linux/build/bzImage \
  --initramfs consonance/harmony-linux/build/initramfs-continuation.cpio.gz
sudo cat /sys/class/dmi/id/product_uuid | tr '[:upper:]' '[:lower:]' > host-id
python3 scripts/qualify-linux-continuation.py run x3-bundle x3-export \
  --test linux_snapshot_continuation::composed_linux_snapshot_continuation \
  --host-id-file host-id --ignored
```

Transfer the unchanged bundle and exporter directory to distinct hosts with
the same source checkout. Each importer runs:

```sh
python3 scripts/qualify-linux-continuation.py run x3-bundle x3-import \
  --test linux_snapshot_continuation::composed_linux_snapshot_continuation \
  --host-id-file host-id --import-from x3-export --ignored
python3 scripts/qualify-linux-continuation.py compare x3-bundle \
  x3-export/receipt.json x3-qualification.json x3-imports/*/receipt.json
```

The hardware workflow runs six import replicas to sample both x86 vendors.
The aggregate requires at least two distinct import hosts and a successful
opposite-vendor import. An unlucky single-vendor allocation is inconclusive
and fails the aggregate; explicit refusals are retained but cannot satisfy
the supported cross-vendor requirement. Missing jobs, malformed receipts,
changed artifacts, substituted cuts and contradictory observations also
fail. Bounds that expire mean incomplete qualification, not a guest failure
classification. Logs already written survive failure and are uploaded.

The registry describes required oracles rather than recording a passing result.
At source `9673f33363395f0850cd2cb5e704432d33434180`,
[run 39159236856](https://github.com/pH14/harmony/actions/runs/39159236856)
passes the AMD export and five AMD imports; one hosted import matches READY and
guest readback but fails the raw DONE whole-state comparison. Nested Intel KVM
reproduces the same failure using the identical retained executable and AMD cut:
`536a76dab595357cf7e9e8d217d14a5ba06b535150964b1d2d8a98ca16b1ae25`
versus expected
`44e55f8d96e950e7c063464974e03532032f32ebae03989de2337a2db47dd126`.
Diagnostic source `e82aa7e03bb04fa0dbad1c84ce5c219e4ed8fd5e` retains
the exact DONE state separately from that qualification binary. Direct AMD
diagnostic results are retained by [run 39160291556](https://github.com/pH14/harmony/actions/runs/39160291556)
and compared with the independently verified nested Intel reproduction. The difference
is x87 FIP: Intel saves the post-cut `fstpl` address `0x4011ab` in CPU state,
encoded VM state and the guest kernel's FPU image in RAM; the AMD expected
state has zero. Changing only these three values in an offline diagnostic copy
reproduces the expected AMD hash. This localizes the failure; it is not an
allowed state normalization. KVM's
[x87 errata](https://docs.kernel.org/virt/kvm/x86/errata.html)
explain why CPUID absence flags do not force uniform native behavior.

Same-vendor receipts establish only their named finite workload and host pairs.
Cross-vendor whole-state identity is unqualified, and the unchanged strict
comparison and aggregate reject it. Equal guest readbacks do not close this gap.
CPU warnings remain advisory, standard libraries remain supported, and
[#733](https://github.com/pH14/harmony/issues/733) retains the broader CPU
portability limits. These dated results must not be relabeled as evidence for
another source or executable.

This cell qualifies only the named finite Linux workload and actual admitted
host pairs. It does not qualify arbitrary applications, nested virtualization,
other backends, reused AMD restore, Arm Linux continuation, or a complete
architectural register inventory. In particular, the Arm OS-lock regression
and its debug-register table do not establish `KVM_GET_REG_LIST` coverage.

## Runtime assurance requests

The [execution assurance contract](../../consonance/execution-contract/README.md)
keeps finite qualification separate from ordinary execution. Its closed
candidate assessments retain the failed cross-vendor continuation above and
UML's unavailable whole-state capability. No production whole-composition
contract currently has the complete required transformation matrix, so an
explicit qualification request refuses at Session/worker/UML admission. Ordinary
library execution remains available, with an unqualified assurance identity
bound through snapshots, cache namespaces and replay certificates. Existing
bounded passing receipts retain their stated scope; neither their existence nor
matching CPU metadata grants a stronger claim.
