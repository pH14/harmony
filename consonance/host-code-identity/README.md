# Host executable mappings

On Linux, `sealed_static_executable_identity` is a separate, strict identity
for a static ELF host launched from a sealed executable memfd. It opens the live
`/proc/self/exe`, requires the write, grow, shrink, and seal seals, rejects an
ELF interpreter, needed libraries, text relocations, and executable stack or
writable code, then hashes the entire ELF file. The full-file hash includes
`PF_W` initializer bytes that the ordinary mapped-image identity excludes. It
also checks that the one live code mapping matches the sealed file, hashes the
live vDSO, records the fixed x86-64 vsyscall gateway when present, and rejects
every other executable mapping. A second executable-map census rejects ordinary
changes during measurement. The result has its own versioned domain and does
not share the dynamic-host identity format.

This primitive requires a static musl host and an executable memfd with all
four required seals. A normal on-disk binary, dynamic loader, shared library,
JIT, or manually created executable mapping fails closed. It is an admission
measurement of the current process, not an attestation against arbitrary
injected code or a guarantee about maps added after measurement. Callers must
control launch and recheck at relevant operation boundaries. The strict guard
retains the admission-time full-file digest only while the same device, inode,
length, and immutable seals remain. Each recheck still compares every live
executable byte with the sealed file, measures the vDSO, and repeats the
executable-map census. Public Session and Recording APIs require this strict
host boundary.

On Linux, `mapped_executable_sha256` reads the bytes currently mapped with
execute permission in its own process through `/proc/self/mem`. It includes the
main program, dynamic loader, dependent libraries, and the vDSO. It also reads
each loaded ELF image's executable and pure read-only `PT_LOAD` segments as one
ordered image record. This binds ordinary library `.rodata` to the code that
uses it; a one-byte read-only data change cannot keep the same identity while
changing a guest entropy response. Executable mapping records include their
permissions, file offsets, lengths, mapping kinds, and mapped bytes. ELF image
records include their segment layout and mapped bytes in loader order. File
paths and absolute addresses are excluded so pathname replacement and address
randomization do not change the identity of unchanged images. Repeated records
retain multiplicity.

Admission refuses anonymous or memfd executable memory, writable executable
memory, unrecognized special executable mappings, unreadable bytes, writable or
inaccessible pure read-only segments, text relocations, and changes to the
relevant maps or loader generation while measuring them. The vDSO's bytes are
included.
The conventional x86-64 `--xp` `[vsyscall]` range has no readable user code;
only its exact kernel gateway shape and running kernel release are recorded.
Other kernel gateway shapes fail closed. A process with a JIT must use a
different identity policy. The digest does not reopen library paths or rely on
device and inode numbers to identify the mapped bytes, so pathname replacement
and Btrfs inode reuse cannot make
different mapped code equal at the measurement point.

This is an admission-time identity, measured under a quiescent loader and
memory map. Pure read-only ELF segments exclude the `PF_W` load segment even
where the loader later protects its RELRO pages as `r--p`. Hashing those mapped
RELRO bytes would make otherwise identical processes differ under ASLR because
they contain relocated pointers. Writable library initializers, RELRO/GOT
values, symbol bindings, constructor effects, generic file-mapped non-ELF data,
and code loaded, unloaded, or changed later remain outside the identity. A
one-byte writable library initializer can still change guest behavior while
retaining this identity. The second relevant-map and loader census catches
ordinary concurrent changes but cannot make a process snapshot atomic against
adversarial mutation.
Constructors run before admission, and a process that can interpose the map or
memory reads can spoof this self-measurement; it is not an attestation boundary.

`loader_epoch` separately exposes the Linux loader's cumulative addition and
removal counters. A client can capture it before loaded-image admission and
compare it at operation boundaries to reject a library that was loaded and
unloaded between image digests. On glibc, the counters include loads in a
separate `dlmopen` namespace. They do not detect writable-data changes or code
writes to mappings that never leave the loader's image list.

On macOS, `mapped_executable_sha256` measures every image reported by dyld,
including the main program, system shared-cache images, and dynamically loaded
libraries. It also measures dyld itself, which is outside dyld's ordinary image
list. Each image contributes its Mach-O UUID, executable segment layout, and
the bytes currently mapped across every executable segment, including mapped
tails beyond file bytes. The shared cache UUID is included as well; the cache
file is not reopened. Paths and absolute load addresses are excluded so an
unchanged mapped image keeps its identity when a pathname is replaced.
Consonance combines this digest with the main program's
code-directory hash, while its worker launch check continues to compare main
program code-directory hashes across processes before the worker reports its
full loaded-image identity.

macOS admission rejects malformed Mach-O images, declared writable executable segments,
unreadable mappings, and dyld image-list changes observed while measuring. It
uses kernel-mediated reads so an image unloading during a census returns an
error rather than dereferencing a stale pointer. This binds dyld-managed Mach-O
images at admission; it does not cover anonymous executable mappings, data
segments, symbol bindings, constructor effects, or images loaded, unloaded, or
modified after admission. Callback and image-count checks cannot make a process
snapshot atomic against adversarial mutation.

On macOS, `loader_epoch` uses dyld's add/remove callback generation plus the
current image count. As on Linux, the epoch is a separate operation guard; it
does not change the admission-time loaded-image digest.

On macOS, `strict_macos_executable_identity` is a separate identity for a
hardened, signed host with the Hypervisor entitlement and an embedded library
constraint admitting only Apple platform libraries. It obtains the running
process's code-signing status and CDHash from the kernel, opens the executable
through a held file descriptor, strictly validates that file's signature, and
requires its signed CDHash to equal the kernel-loaded CDHash. It requires the
sole entitlement to be `com.apple.security.hypervisor = true`. It reads the
signed library-constraint slot and accepts only the exact version-one
`validation-category = 1` encoding. Missing, differently encoded, or
unavailable constraint information fails closed. A replacement executable at
the same pathname therefore cannot substitute its policy or identity for the
code already loaded by the kernel.

The strict census requires the main Mach-O image, dyld, and all other dyld
images to be in their expected roles. Dyld and every library must have their
headers inside the mapped Apple shared-cache range and carry the shared-cache
image flag. The identity binds the kernel-loaded executable CDHash and dyld
shared-cache UUID in a separate versioned domain. The OS-only library
constraint is the kernel-enforced admission rule; the cache census verifies
the mapped result. Hardened runtime and the exact entitlement set prevent
library-validation, JIT, or writable-executable exceptions. Recheck the
kernel signing status, loaded CDHash, dyld generation, and shared-cache census
at operation boundaries where loaded host code matters. The runtime guard
does not reopen or revalidate the path on each operation; admission and each
path-based worker launch perform the full static signature and policy check.

The `LibraryConstraints` field is an internal Security.framework signing-info
key, and the cache-range query is a dyld-private symbol. This implementation
accepts the observed macOS encoding exactly and refuses admission if those
interfaces change or disappear. It does not attest against a compromised
kernel or code that can subvert its own in-process checks. A separately mapped
executable page that dyld does not report remains outside this image census;
the hardened runtime without executable-memory exceptions is required to
prevent such code from entering this process.

`StrictExecutableGuard::capture_measured` and `check_measured` accept caller-owned
`GuardMeasurement` totals. Attempts, errors and elapsed host nanoseconds include
refusals; partial work remains available after an error. Linux byte fields count
successful bytes returned by the actual code/vDSO and sealed-file reads, including
partial reads before EOF or another error. Hash bytes count only buffers actually
fed to SHA-256. Comparison input bytes count each compared pair's length once
(N for two N-byte buffers), including a mismatching pair; they do not estimate CPU
comparison instructions. Admission-only whole-file reads/hashes are separate from
recheck code reads. ELF headers, proc-map text, metadata and digest framing are
excluded from executable-byte fields. The live vDSO is included. macOS byte fields
are unavailable (`None`), while kernel signing-state checks, static-signing checks
and image-census attempts count their actual stages. These observational totals
have no global owner and enter no executable identity, policy or snapshot bytes.
