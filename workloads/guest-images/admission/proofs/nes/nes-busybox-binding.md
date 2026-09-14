# nes-busybox: startup eager-binding proof

ELF SHA256 dd40538865c749943b671e11ef9f644f86dd6037abe35dcbbf7f7d67853ae3ee.
This is a static ELF with no PT_INTERP or DT_NEEDED. Static does not itself
exclude libc dlopen or IFUNC. The exact binary's _dl_non_dynamic_init writes
_dl_lazy at 0x47638b; dl_open_worker_begin reads it at 0x45b468.
D4 retained glibc2.41 elf/dl-support.c:307 initializes the global from startup
LD_BIND_NOW, and elf/dl-open.c:633–636 gates requested RTLD_LAZY with that global.
Thus LD_BIND_NOW=1 before libc startup gives _dl_lazy=0; ordinary later dlopen
cannot request lazy relocations even if it passes RTLD_LAZY. Changing the
process environment afterwards does not reset the initialized global. Each
new exec needs the invariant again. The one observed lazy-GOT pointer consumer
identified in nes-busybox-incoming.md is bypassed when lazy=0; it does not install
either reviewed XSAVE trampoline. IFUNC bodies still run eagerly and were
included in the complete executable-segment scan.

The agent's NOW flags are supporting evidence, not the argument for future
dlopen. BusyBox has no NOW dynamic flags; its proof relies on startup binding.
This conclusion is conditional on controlled-scope.md and the final composed
launch environment: no LD_PROFILE/LD_AUDIT/preload/loader overrides, no direct
calls to internal resolver addresses and no code/data corruption. It approves
neither unrestricted BusyBox commands nor arbitrary external libraries.
