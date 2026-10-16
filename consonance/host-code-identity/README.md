# Host executable mappings

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
