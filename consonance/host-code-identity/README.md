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
