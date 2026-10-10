# Executable user examples

The counter tutorial reuses `workloads/bugs/category/lost-update/lost_update.c`
and `workloads/bugs/interleaving.h` from this revision. `main.c` supplies the
assertion forwarding needed when linking that existing fixture through the
standard dynamically loaded C runtime. The vendor coverage shim stays unchanged.

| Service | Language and build | Installed program | Symbols | Startup and readiness |
| --- | --- | --- | --- | --- |
| writer-0 | C, standard pinned Clang language recipe | `/opt/harmony/application` | `/symbols/application`, generated native symbol table | `writer /tmp/counter 0`; shared `ready` command |
| writer-1 | Same executable | Same | Same | `writer /tmp/counter 1`; shared `ready` command |
| setup | Same executable | Same | Same | `init /tmp/counter` before nodes start |
| investigation shell | Prepared image's shell and utilities | `/bin/sh` | Image admission | On demand through branch command channel |

All application loops receive the recipe's coverage callbacks. The standard C
language qualification owns timer progress, same-input identity, and park
progress; Documentation Examples additionally requires a confirmed lost-update
assertion and actual branch restoration. The image uses the current composed
libvoidstar, retained symbols and executable attestations. `prepare` must pass
admission before `check` and search.

The files displayed on the site are the files this runner executes, not copies
of commands maintained in a separate test. See `docs/SITE.md` for ownership,
adding examples, and CI checks.
