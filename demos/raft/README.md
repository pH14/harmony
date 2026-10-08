<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Raft browser lab

The lab explains an acknowledged write disappearing after a leader change.
It starts with immediately playable evidence from real Harmony CLI runs.
The live mode boots an x86 Linux machine in QEMU-Wasm, then runs the same
Harmony CLI and Consonance UML workload locally. GitHub Pages only serves
static files. No request executes a workload on a hosted compute service.

## Workload

`workload/raft.c` is a deliberately buggy teaching subset of Raft: three
processes, one log entry, fixed membership, elections and Unix datagram
replication. It is not a production Raft implementation. The initial leader
is A. The fault sequence isolates A, submits a write and kills A. A wrongly
acknowledges its local append without waiting for a majority. B becomes
leader without that write, and the service emits the real SDK durability
assertion at election time.

| Role | Process / command | Behavior |
| --- | --- | --- |
| Setup | `/app/control setup` | Creates the shared experiment directory. |
| Replicas | `/app/control node 0`, `1`, `2` | Independently supervised, instrumented Linux processes. |
| Ready | `/app/control ready` | Requires all three initial state files. |
| Fault hook 1 | `/app/control partition` | Enables packet dropping in the teaching transport around A. |
| Client hook 2 | `/app/control write` | Submits the single counter write to A. |
| Recovery hook 3 | `/app/control heal` | Disables transport packet dropping. |
| Invariant | `check()` in the elected leader | Emits `raft-ack-durability` if an acknowledged write is absent. |

The native image uses the composed fault runtime, the unmodified SDK
forwarding shim, sanitizer coverage instrumentation, symbols, and a binary
attestation. Its small scratch rootfs contains only the application, shell,
inspection tools and required pinned Debian libraries. It needs no reviewed
hardware-entropy instruction exceptions. The control files and state
files are intentional teaching interfaces, visible from the real shell.

## Investigation

The displayed commands are actual CLI commands:

```sh
harmony debug run --actions /demo/actions.json --name failure
harmony branch failure --step 1 --exec '/app/control trace' --name investigation
harmony branch failure --step 1 --exec '/app/control require-quorum' --name quorum
harmony branch failure --step 1 --shell --name inspected
```

`--exec` restores the selected prefix, runs the guest command, captures its
mutation and replays the original suffix. `--shell` stops at the branch
point; exiting saves the altered machine. The UI assigns unique branch names
for repeated live experiments. Shell tips insert text; Enter executes it.

The source heat is derived from emitted `RAFT` events with real source-line
numbers and guest timestamps. It is **event activity**, not full line
coverage. Recent activity cools exponentially, while visited event sites
remain tinted. Logs and cluster roles come from those events; phase controls
come from recorded action boundaries. The right pane never invents log
lines. `public/evidence` is populated from checksum-pinned release assets containing the original CLI reports, including runtime
identity, image hashes, assertions, action history and captured console tails.

The quorum example demonstrates this one counterfactual. It does not prove
that the teaching implementation satisfies the complete Raft protocol.
Native and browser machines have different host identities; a native
checkpoint is never silently imported into the browser. Live mode generates
its own baseline and branches there.

## Build and verification

Use Node 24 or newer for the frontend. The runtime assembly tools require
x86 Linux, Podman, GNU tar, cpio, gzip and a native Harmony build. Build the
UML profile from this checkout, including the legacy FPU frame fix in
`consonance/harmony-linux/linux/patches/um`, and build the current platform
initramfs with the repository's runtime scripts.

```sh
podman build --format docker -f demos/raft/workload/Dockerfile \
  -t harmony-raft-browser:local .
# Fill runtime paths in a copy of workload/harmony.toml.
bash demos/raft/tools/record-evidence.sh /absolute/path/to/harmony \
  /absolute/path/to/harmony.toml /tmp/raft-evidence
bash demos/raft/tools/build-busybox.sh /path/to/pinned/busybox-source /tmp/browser-busybox
bash demos/raft/tools/assemble-runtime.sh /absolute/path/to/harmony \
  /path/to/uml-profile /path/to/base.cpio.gz /tmp/browser-busybox/busybox \
  localhost/harmony-raft-browser:local /tmp/browser-runtime
```

`record-evidence.sh` requires a fresh output directory. It validates admission,
the original violation, tracing, the quorum counterfactual, two fresh replay
matches, and preservation of a shell-created file through a subsequent branch.
The UML regression script `consonance/harmony-linux/uml/verify-legacy-fpu.sh`
checks real nested boot both with and without XSAVE. The normal unprivileged
UML launch/replay/checkpoint qualification suites remain applicable.

The outer Linux image contains the CLI, its dynamic libraries, GNU tar,
static BusyBox, the UML profile, current guest base and the OCI image. Its
filesystem and all live runs are volatile and disappear when the page closes.
The frontend uses an actual PTY. Private framing around CLI reports is hidden
from the visible terminal; ordinary CLI output, terminal controls and guest
shell interaction remain real.

GitHub Pages cannot set isolation headers directly. `isolate-sw.js` adds them
within the `/raft/` service-worker scope when live mode is requested, then
reloads once. This does not change `/nova/`. Desktop Chrome is the tested
live browser. Boot and restoration under software emulation can take minutes;
recorded playback remains the immediate introduction.

## Static frontend and publishing

```sh
cd demos/raft
npm ci
npm run build
npm test
npx playwright install chromium
npm run preview
# In another terminal:
npm run test:browser
CHROME=1 npm run test:live
```

`build` verifies every runtime input against `runtime-lock.json` and produces
`dist/` with only the required files. The lock pins the tested QEMU-Wasm
8.2.0 amd64-alpine distribution; the other upstream x86_64 distribution did
not successfully execute this CLI. Runtime sources and third-party licenses
are linked in `NOTICES.md`. Browser images are static release artifacts,
separate from product releases. Live verification can take several minutes
per restoration and is intentionally separate from the bounded PR UI checks.
`test:live` executes a new failure, a tracing branch, and a real interactive
shell mutation saved on exit. `DEMO_URL` selects a deployed site; `CHROME=1`
uses installed desktop Chrome instead of Playwright Chromium.

For Pages-like testing use `ISOLATE=0 npm run preview`. The first live request
installs the scoped isolation worker and reloads before booting. All runtime
assets are same-origin. Recorded playback needs no isolation or downloads
of the machine image.

The shared `gh-pages` branch hosts `/nova/` and `/raft/`. Publish the dist tree
with `tools/publish-pages.sh`; it updates only `/raft/`, the shared landing
page and Raft's deployment metadata, preserving Nova and unrelated content.
The script requires an explicitly chosen source revision and a clean gh-pages
checkout. It does not merge either demo's implementation pull request.

The manual **Runtime Acceptance** job rebuilds the teaching OCI image, validates
it using the pinned CLI/UML runtime, records fresh evidence, exercises nested
boot with both FPU formats, and assembles a new browser image. Its reports and
image are uploaded as CI artifacts. Raw run reports are release/build inputs,
not repository history. Enable `verify_runtime` on the Raft Browser workflow
to run this check; normal PRs keep the bounded static checks.
