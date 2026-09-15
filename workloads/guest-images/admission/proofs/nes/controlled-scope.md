# Controlled NES execution review and remaining composition obligations

Reviewed exact QuickNES commit 26bb785c9deddb66a17717b21bb4e328f03ade32
(refetched without building/executing source). quicknes-source-hashes.json binds
all reviewed C/C++ headers/sources. Nes_Cpu.cpp:117 implements run; line188
switches on an opcode fetched as data through READ_PROG. libretro.cpp:1425–1539
passes RetroGameInfo bytes through Mem_File_Reader to load_ines. The executable
memory/process-loading API search has only five generic libretro.h documentation
hits and no implementation hits. This is a trusted interpreter, not a JIT.
Repository scripts/build-quicknes-core.sh pins this revision and applies only
C/C++ header/namespace substitutions for the static archive. Its allocation
shim does not generate code. The exact resulting agent digest is separately
bound; this source review does not approve a different build.

workloads/nes-guest/build.sh selects static-quicknes. In main.rs:552–610 that
feature selects direct linked retro_* functions and compiles out application
dlopen/dlsym. run_nes:321–323 reads ROM bytes into Vec; lines618–631 retain those
bytes and pass data/size to retro_load_game. workloads/nes/src/prepare.rs:23–33
selects direct argv [/opt/harmony/play-agent,--nes-payload,--rom,/game.nes].
ROM contents are emulated CPU data, not native ELF/code. Only a reviewed known
ROM digest and input sequence qualify; this is not a memory-safety proof for
arbitrary malformed ROMs.

The exact OCI manifest/config/layer digests are in oci-config-evidence.json.
Its environment contains only PATH, empty entrypoint/Cmd, cwd/, root UID/GID.
Current bundle.rs:273–280 adds LD_BIND_NOW=1 after removing conflicting values.
No loader override/profile/audit variables occur in this OCI config. There is
no shell in the NES launch argv; BusyBox is retained image surface, not an
additional required process. Any later allowed BusyBox applet must inherit
startup binding and remain within reviewed commands; arbitrary shell commands
are outside this review.

Final admission must additionally bind the actual prepared execution JSON,
ROM hash, platform initramfs and rootfs hashes, and validate the final child
environment before every exec. Current OCI root.readonly=false: immutable input
identity and trusted absence of executable-file modification are assumptions,
not enforced read-only mounting. No JIT/generated code, executable anonymous
mappings, or code mutation is allowed in this controlled workload. Static
segment checks cannot enforce these runtime obligations. Until final composition
is attached, proposed-artifact-entries.json is not a complete approval baseline.
