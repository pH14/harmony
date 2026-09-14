# nes-play-agent: startup eager-binding proof

ELF SHA256 4cc1dacf6ef2aed5638e5eb6eceb7130eca532ed351e3a5b692da0a903a6349a.
This is a static ELF with no PT_INTERP or DT_NEEDED. Static does not itself
exclude libc dlopen or IFUNC. The exact binary's _dl_non_dynamic_init writes
_dl_lazy at 0x20c58b; dl_open_worker_begin reads it at 0x1ff4e8.
D4 retained glibc2.41 elf/dl-support.c:307 initializes the global from startup
LD_BIND_NOW, and elf/dl-open.c:633–636 gates requested RTLD_LAZY with that global.
Thus LD_BIND_NOW=1 before libc startup gives _dl_lazy=0; ordinary later dlopen
cannot request lazy relocations even if it passes RTLD_LAZY. Changing the
process environment afterwards does not reset the initialized global. Each
new exec needs the invariant again. The one observed lazy-GOT pointer consumer
identified in nes-play-agent-incoming.md is bypassed when lazy=0; it does not install
either reviewed XSAVE trampoline. IFUNC bodies still run eagerly and were
included in the complete executable-segment scan.

The agent's NOW flags are supporting evidence, not the argument for future
dlopen. BusyBox has no NOW dynamic flags; its proof relies on startup binding.
This conclusion is conditional on controlled-scope.md and the final composed
launch environment: no LD_PROFILE/LD_AUDIT/preload/loader overrides, no direct
calls to internal resolver addresses and no code/data corruption. It approves
neither unrestricted BusyBox commands nor arbitrary external libraries.
