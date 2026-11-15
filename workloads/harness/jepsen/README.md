<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Jepsen NATS baseline

This directory runs Jepsen's own NATS JetStream test
([jepsen-io/nats](https://github.com/jepsen-io/nats)) against the NATS
versions the `targets/nats` harness tests, so the two tools can be compared on
the same system.

## Layout

| path | contents |
|---|---|
| `control.Dockerfile` | Jepsen control node: Temurin 21, Leiningen 2.11.2, `jepsen-io/nats` at a pinned commit with `nats.patch`, built into `/jepsen/jepsen-nats.jar` |
| `node.Dockerfile` | database node: Debian bookworm, sshd, the Debian packages `jepsen.os.debian` installs, lazyfs 0.3.0, and the official `nats-server` 2.10.20, 2.10.23 and 2.12.1 release binaries checked by SHA-256 under `/usr/local/lib/nats-bin/<version>` |
| `nats.patch` | changes to the upstream test, listed below |
| `run.sh` | builds both images, starts one control node and three database nodes `n1`-`n3` on their own network, installs the control node's SSH key on the nodes and runs tests |
| `summarize.py` | one JSON line per test from a Jepsen store |

## Changes to the upstream test

- `install!` copies the binary for `--version` from `/usr/local/lib/nats-bin` instead of
  fetching `binaries.nats.dev` and an amd64-only `natscli` package. The node
  image installs `natscli` 0.2.2 for its architecture.
- Setup no longer runs `nats-server` once without a config file. With a
  release binary that call starts a server and never returns.
- Kill, pause and resume use `pkill -x nats-server`. Jepsen's `grepkill!`
  runs `pgrep -f nats-server | xargs kill`, and `pgrep -f` can match the
  forked shell of its own pipeline before that shell runs `xargs`. A `SIGSTOP`
  then hangs the nemesis, and a `SIGKILL` crashes the test.
- The file-corruption nemesis is set up only when a corruption fault is
  selected. Its `corrupt-file --clear-snapshots` step aborts with a glibc
  double free on arm64 and crashed every test.
- `io.jepsen/antithesis` is pinned to release 0.1.0 instead of
  `0.1.0-SNAPSHOT`, which `lein uberjar` refuses.

## Running

`JEPSEN_STORE` must be a directory outside the repository. It receives the
Jepsen store and `runs.tsv` (start, end and exit status of each test).

```sh
export JEPSEN_STORE=/tmp/jepsen-nats JEPSEN_CPUSET=0-1 JEPSEN_PROJECT=jepsen-nats-2-10-20
workloads/harness/jepsen/run.sh 2.10.20 3 \
  --nemesis kill,pause,partition --rate 100 --time-limit 600 --final-time-limit 120
python3 workloads/harness/jepsen/summarize.py "$JEPSEN_STORE"
workloads/harness/jepsen/run.sh down
```

`run.sh VERSION COUNT [OPTIONS]` passes the options to `jepsen.nats.cli test`;
`--lazyfs` mounts the data directory on lazyfs so a kill loses unsynced
writes, and `--initial-quiet-period` sets the seconds before the first fault
(default 10). `run.sh down` removes the containers and the network of
`JEPSEN_PROJECT`. A later `run.sh` with the same project reuses running
containers, so run `down` before changing `JEPSEN_STORE` or `JEPSEN_TMPFS`.
`JEPSEN_TOOL` picks `podman` or `docker` (default `podman` when installed).
With podman the network has its DNS server off, because on hosts without
`aardvark-dns` it answers no names, and Jepsen downloads its `bitflip` tool
from GitHub at setup. `run.sh` writes the node addresses into each
container's `/etc/hosts`.
`JEPSEN_CPUSET` pins every container (default `0-5`),
`JEPSEN_PROJECT` prefixes the container and network names so several clusters can run at
once, `JEPSEN_HEAP` sets the control JVM heap (default `3g`), and a nonempty
`JEPSEN_TMPFS` mounts `/opt` on the nodes as tmpfs, so the NATS store lives in
memory as it does in the harness image. Nodes get `NET_ADMIN` for iptables and
`tc`, and `SYS_ADMIN` with `/dev/fuse` for lazyfs. They run unprivileged:
Jepsen's clock nemesis runs `ntpdate -b` on every node at setup, and a
privileged container would set the clock of the shared container VM. Both
images run `tini` as process 1, which reaps the daemons Jepsen starts. The node
image tells apt to accept release files dated in the future, because that VM
clock can be skewed.

Jepsen checks the history only after the test ends: the main phase, a recovery
phase that restarts and resumes every node, and a final drain that reads the
stream for up to `--final-time-limit` seconds. `summarize.py` reports the
verdict, acknowledged and read counts, lost acknowledged publishes, the test
times of the first and last lost publish, the first fault and its targets,
and the number of whole-cluster outages (all three nodes killed at once)
before the last lost publish. A test whose final read returned nothing and
lost every acknowledged publish lost the stream.
