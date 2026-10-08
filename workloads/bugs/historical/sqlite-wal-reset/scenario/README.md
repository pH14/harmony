# Authored SQLite WAL regression

An ordinary Python regression test drives `harmony prepare`, `branch`, `show`,
and `search`.
The affected and fixed versions run the same authored schedule. Every result is
a normal branch that can be inspected, rewound, opened in a shell, or searched.

## Services and preparation

| Service | Language and version | Build system | Runtime artifact path | Symbol files |
| --- | --- | --- | --- | --- |
| checkpointer and writer | CPython 3.14.5 | shared instrumented Python recipe | `/opt/python/bin/python3.14` | `/symbols/native/opt/python/**` |
| SQLite native library | 3.51.2 / 3.51.3 | pinned amalgamation, Clang coverage | `/usr/local/lib/harmony/libsqlite3.so.0` | `/symbols/sqlite/libsqlite3.so.0.sym.tsv` |
| Python SQLite extension | CPython 3.14.5 | same pinned CPython sources, Clang coverage | `/opt/python/lib/python3.14/lib-dynload/_sqlite3.so` | `/symbols/sqlite/_sqlite3.so.sym.tsv` |

Both actors use `workload.py`, selected by the TOML node commands. Setup prepares
the database and signals setup completion; readiness is `/bin/true`. Hooks release
the two actors. The CLI generates the supervisor bundle from each recipe.
SQLite revisions are `3ce53bc469dcef8d8c2d90eb59a7d13184e782e5` (affected) and
`ac1a538a559d94801b29a96f96fd8f9e0943f88f` (fixed). CPython and SDK pins come from
`workloads/languages/python/Dockerfile`. Runtime composition uses the shared recipe.
The image retains instrumentation attestations and native symbols. Python has no
JIT or ctypes callbacks; SQLite generates no machine code. The regression inserts
one named coverage callback at the vulnerable checkpoint call, without changing
SQLite's checkpoint or write logic.

## Run and investigate

From the repository root with Python 3.11+, the built `harmony` on `PATH`, and
Docker (or `HARMONY_CONTAINER_TOOL=podman`):

```sh
python3 workloads/bugs/historical/sqlite-wal-reset/scenario/test_wal_reset.py --out evidence/sqlite
harmony branch evidence/sqlite/before-failure --shell
```

Recipes build and admit both versions through `harmony prepare`. Custom local
kernel/initramfs or UML assets can be selected in each recipe's `runner.options`.
The host script contains the test; `workload.py` contains guest actors and the
single source instrumentation hook. The race parks the checkpointer after it
reads a stale WAL header, lets the writer commit, then resumes checkpointing.
Every repeated execution must show the actual park, both writer progress and the
final fresh-connection read. The affected version must violate the committed-write
oracle, the fixed version must remain clean, and the unparked control must remain
clean. Repeats compare saved state and oracle evidence. The test then checks the
affected logs and timeline, saves a pre-failure branch, and runs a four-execution
search from it. The full test takes about three minutes on a KVM host.

The schedule waits 35 seconds of guest time before starting the writer. Under
full instrumentation, the checkpointer reaches the parked checkpoint call about
28 seconds after `start-checkpoint`. The 20-second hold covers the writer's
commit. `writer-finished-during-pause` holds only when the writer had not
committed before the park and had committed when the checkpoint resumed.
