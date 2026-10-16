# Host executable mappings

On Linux, `mapped_executable_sha256` reads the bytes currently mapped with
execute permission in its own process through `/proc/self/mem`. It includes the
main program, dynamic loader, dependent libraries, and the vDSO. The digest
uses mapping permissions, file offsets, lengths, names, and mapped bytes;
absolute addresses are excluded so ordinary address randomization does not
change it. Repeated records retain multiple mappings of the same bytes.

Admission refuses anonymous or memfd executable memory, writable executable
memory, unrecognized special executable mappings, unreadable bytes, and changes
to the executable map list while measuring it. The vDSO's bytes are included.
The conventional x86-64 `--xp` `[vsyscall]` range has no readable user code;
only its exact kernel gateway shape and running kernel release are recorded.
Other kernel gateway shapes fail closed. A process with a JIT must use a
different identity policy. The digest does not reopen library paths or rely on
device and inode numbers to identify the mapped bytes, so pathname replacement
and Btrfs inode reuse cannot make
different mapped code equal at the measurement point.

This is an admission-time code-byte identity, measured under a quiescent loader
and memory map. It does not bind data or read-only non-executable library
segments, symbol bindings, constructor effects, or code loaded, unloaded, or
changed later. A second map-list read catches ordinary concurrent map changes
but cannot make a process snapshot atomic against adversarial mutation.
Constructors run before admission, and a process that can interpose the map or
memory reads can spoof this self-measurement; it is not an attestation boundary.

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
