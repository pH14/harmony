# Building & testing

How to build and test each component, and where. Rule of thumb: **everything runs locally on
a Mac** (Apple Silicon included) except the guest Linux kernel build, which needs a Linux
environment (a container is fine). Nothing in tasks 01–05 needs `/dev/kvm` or any special
hardware; the KVM/PMU work lives in vmm-core, later, on the Linux box.

### Environment capability matrix

| Work | macOS | Linux container | Linux bare-metal Intel |
|---|---|---|---|
| Delegated crates (01, 02, 03, 05) | ✅ | ✅ | ✅ |
| Guest payloads (04 Part A) | ✅ QEMU TCG via brew | ✅ | ✅ |
| Guest Linux image (04 Part B) | 🐳 linux/amd64 container only | ✅ | ✅ |
| vmm-core KVM bring-up | ❌ | ❌ | ✅ needs VMX + `/dev/kvm` |
| PMU / perf_event precise-count spike | ❌ | ❌ | ✅ needs PMU access (`perf_event_paranoid` or root) |
| KVM snapshot / memslot / userfaultfd spike | ❌ | ⚠️ userfaultfd-only parts | ✅ |
| Deterministic Linux integration (Phases 1+) | ❌ | ❌ | ✅ |

Extra needs for the rows that run on a Mac: task 01 wants the `x86_64-unknown-none` target
(compile-only check); 04 Part A wants QEMU + that target; 02/03/05 want Rust only. Nested
virtualization is **not** a substitute for the bare-metal column — PMU behavior under
nesting is exactly what we can't trust.

## One-time setup

Everywhere (macOS and Linux):

```sh
# rustup (https://rustup.rs) — rust-toolchain.toml selects the toolchain automatically
rustup target add x86_64-unknown-none
```

macOS only:

```sh
brew install qemu coreutils   # qemu: task 04 only; coreutils: provides gtimeout
```

Linux (incl. containers / the future server), for task 04:

```sh
apt-get install -y build-essential flex bison libelf-dev libssl-dev bc cpio kmod \
                   wget qemu-system-x86 shellcheck
```

## Standard cargo gates (tasks 01, 02, 03, 05)

Run from the repo root:

```sh
cargo build  -p <your-crate> --all-features
cargo test   -p <your-crate> --all-features
cargo clippy -p <your-crate> --all-features --all-targets -- -D warnings
cargo fmt    -p <your-crate> -- --check
```

**These crates must pass on both macOS and Linux.** That implies a hard portability rule: no
Linux-only syscalls or APIs (`memfd_create`, `userfaultfd`, `io_uring`, `/proc`, epoll, …) in
any delegated crate. Where you need file-backed or mapped memory, use the portable layers:
`tempfile` + `memmap2`. Don't write `#[cfg(target_os)]` forks of your logic — if you think
you need one, the design is wrong; ask in the PR instead. (vmm-core will be Linux-only; your
crate is not vmm-core.)

Task 01 has one extra compile-only gate (works on any host, no execution):

```sh
cargo build -p hypercall-proto --no-default-features --features guest \
            --target x86_64-unknown-none
```

## Task 04

**Part A — payloads.** Builds and tests locally on macOS or Linux: payloads cross-compile to
`x86_64-unknown-none` (rust-lld links ELF on any host) and run under
`qemu-system-x86_64` TCG emulation — no KVM. Notes:

- On Apple Silicon, TCG-emulating x86 is slow; budget a few minutes for the payload suite.
  The 60 s per-payload timeout in the gate script is generous for this reason.
- Shell scripts must be portable across macOS/Linux: use
  `timeout` where available, falling back to `gtimeout` (declare the helper once in the
  script); no GNU-only flags to `sed`/`stat`/etc.; `shellcheck` clean.

```sh
make -C consonance/acceptance-suite test-payloads
```

**Part B — Linux kernel + initramfs.** Requires Linux. On macOS, run it in a linux/amd64
container (Docker Desktop, OrbStack, or colima all work):

```sh
docker run --rm -it --platform linux/amd64 -v "$PWD":/work -w /work debian:stable bash
# inside: apt-get update && apt-get install -y <the Linux dep list above>
make -C harmony-linux fetch        # downloads to harmony-linux/dl/, hash-verified; network needed once
make -C harmony-linux test-linux   # boot gate + reproducibility gate
```

`make -C harmony-linux test` runs Part A everywhere and Part B only when on Linux; on macOS it must
fail fast for Part B with a clear "run this in a container" message, never skip silently.

## Worker orchestration

Delegated tasks run as interactive Claude Code sessions inside tmux, one per task, driven
by the foreman via `tmux send-keys`:

- `scripts/agent-spawn.sh <task-slug> [--engine deepseek] [--yolo]` — creates the worktree
  + branch per conventions and launches the worker in tmux session `agent-<slug>` with the
  task spec as its prompt.
- `scripts/agent-send.sh <slug> "message"` — types + submits a message to the worker.
- `scripts/agents-status.sh` — sessions, lifecycle markers, task-branch heads at a glance.

Completion signaling: the repo's `.claude/settings.json` ships `Stop`/`SessionEnd` hooks
(`scripts/agent-hooks/turn-marker.sh`) that touch
`/tmp/harmony-agents/harmony-task-<slug>.stop` whenever a worker finishes a turn —
the orchestrator watches for that file instead of polling the terminal. `agent-send.sh`
clears the marker before sending, so its reappearance always means "responded to the
latest message".

## Hygiene

- Network access: crates.io, plus the hash-pinned downloads in `harmony-linux/dl/` (task 04 only).
  Nothing else.
- Install no global tools beyond this document; if you need one, say so in your PR
  description instead of installing it.
- If a gate cannot run in your environment, state that explicitly in your
  `IMPLEMENTATION.md` with the reason — a gate that didn't run is a gate that failed, unless
  the integrator can see why.
