# guest/ — test payloads and the minimal Linux guest image

Two independent deliverables (task 04), both runnable under stock QEMU and
deliberately independent of the hypervisor:

- **Part A — `payloads/`**: tiny bare-metal Multiboot v1 payloads with fully
  known, timing-independent output, checked byte-for-byte against committed
  goldens in `golden/`.
- **Part B — `linux/`**: a pinned, reproducibly built minimal Linux kernel +
  BusyBox initramfs that boots to `GUEST_READY` on the serial console and
  powers off.

## Entry points

```sh
make -C guest fetch           # download + sha256-verify tarballs into guest/dl/ (network, once)
make -C guest test-payloads   # Part A gate (macOS or Linux)
make -C guest test-linux      # Part B gate (Linux only; fails fast elsewhere)
make -C guest test            # both; on a non-Linux host Part B fails fast with instructions
```

`make -C guest kernel|initramfs|image` build the Part B pieces individually
(Linux only). Artifacts land in `guest/build/` (gitignored); kernel and
busybox build trees live at `/tmp/hypervizor-guest-build` (override with
`GUEST_BUILD_ROOT=...`) — a fixed native-filesystem path, because the repo may
be bind-mounted from a case-insensitive macOS filesystem that a kernel tree
cannot be extracted onto, and because identical absolute paths are one of the
reproducibility requirements.

The AA-5(c) ARM spike has separate native-Altra entry points:

```sh
make -C guest arm64-image
```

They require Linux/aarch64 and publish `Image` plus `initramfs.cpio.gz` under
`guest/build/arm64/`, so they cannot overwrite the established x86 artifacts.
The kernel build applies the pinned ARM work-derived-clock patch and refuses
publication unless the final `vmlinux` contains zero live generic-counter
reads. These artifacts are spike evidence, not an AA-5 certification by
themselves; the live pinned-box gates remain authoritative.

## Prerequisites

Everywhere: Rust via rustup (the repo's `rust-toolchain.toml` applies), plus

```sh
rustup target add x86_64-unknown-none
```

macOS (Part A only):

```sh
brew install qemu coreutils    # coreutils provides gtimeout
brew install shellcheck        # only to run the script lint locally
```

Linux / the Part B container (Debian package names):

```sh
apt-get install -y build-essential flex bison libelf-dev libssl-dev bc cpio \
                   kmod wget qemu-system-x86 xz-utils bzip2 shellcheck
```

(`xz-utils` unpacks the kernel tarball, `bzip2` the busybox tarball; both are
additions to the list in docs/BUILDING.md.)

On macOS, run Part B in a linux/amd64 container (from the repo root):

```sh
docker run --rm -it --platform linux/amd64 -v "$PWD":/work -w /work debian:stable bash
# inside: apt-get update && apt-get install -y <the Debian list above>
make -C guest fetch        # if not already done; needs network once
make -C guest test-linux
```

## Part A: payload boot & I/O contract

- Multiboot v1 ELF, entered in 32-bit protected mode; the shared `common`
  crate's shim builds identity-mapped page tables (first GiB, 2 MiB pages)
  and enters long mode before calling `payload_main()`.
  The Multiboot header uses the address-override fields (flag bit 16), so
  loaders treat the file as flat and never parse the ELF container — QEMU's
  multiboot ELF path rejects ELF64 images.
- Console: polled 8250 UART at `0x3F8`, no UART interrupts.
- Exit: `u8` code to port `0xF4` (`isa-debug-exit`); QEMU's process exit
  status becomes `(code << 1) | 1`, so payload code 0 ⇒ QEMU status 1.
- Output: first line `PAYLOAD <name> START`, last line `PAYLOAD <name> PASS`
  or `PAYLOAD <name> FAIL <reason>`; deterministic lines in between; never
  timing-, address- or environment-dependent values.

`payloads/run-tests.sh` boots every payload **twice** (under TCG two runs
must already be byte-identical) and compares the serial output against
`golden/<name>.txt` from the payload's `PAYLOAD <name> START` banner onward
(which must also be the stream's first `PAYLOAD ` bytes) — SeaBIOS/iPXE
print version banners and PMM addresses on the serial console before the
payload runs, and those are environment-dependent by nature. It also runs
compute-core's host-side test, which recomputes the `compute` digest with the
same shared code and checks it against the golden.

### Adding a new payload

1. `cp -r payloads/hello payloads/<name>`; edit `Cargo.toml` (`name = "<name>"`)
   and `src/main.rs` (use `common::payload::{start, ok, pass, fail}`; keep
   `build.rs` as is — it injects the shared linker script).
2. Add `"<name>"` to `members` in `payloads/Cargo.toml`.
3. Add `<name>` to the `PAYLOADS` list in `payloads/run-tests.sh`.
4. Build and run it once to eyeball the output, then commit it as the golden:
   ```sh
   cd guest/payloads && cargo build --release
   qemu-system-x86_64 -m 256 -nographic -no-reboot \
     -device isa-debug-exit,iobase=0xf4,iosize=0x04 -serial mon:stdio \
     -kernel target/x86_64-unknown-none/release/<name> | tee /tmp/<name>.raw
   # trim everything before the first 'PAYLOAD ' byte, then:
   #   cp <trimmed> guest/golden/<name>.txt
   ```
5. `make -C guest test-payloads` must pass — including the second run; if it
   doesn't, the payload's output is timing-dependent, which is a bug in the
   payload, not in the gate.

Rules: output must satisfy the protocol above; no raw TSC values, no
interrupt counts, no pointers; `unsafe` is fine (this is bare metal) but keep
what's reusable in `common`.

## Part B: minimal Linux guest

Pins live in `linux/versions.lock` (kernel 6.18.35 LTS, busybox 1.38.0,
sha256 each). `linux/config-fragment` (every line commented with its
rationale) is merged on top of `make ARCH=x86_64 tinyconfig`; the build
asserts that the determinism-critical options survived the merge.

The initramfs is packed with the kernel's own `gen_init_cpio` from a spec
file — entry order is the spec order, owner is 0:0, `-t 0` fixes every mtime
— and compressed with `gzip -n`. `/init` mounts proc and sys, prints
`GUEST_READY`, and powers off (ACPI is enabled in the fragment exactly so
that QEMU exits).

`linux/run-tests.sh` (= `make -C guest test-linux`):

1. **Reproducibility**: `clean-artifacts` + full rebuild, twice; both
   `bzImage` and `initramfs.cpio.gz` must hash identically; writes
   `linux/MANIFEST.sha256`. This proves same-machine/same-toolchain
   reproducibility; cross-machine reproducibility additionally needs the
   pinned container from docs/BUILDING.md.
2. **Boot**: QEMU with `-no-reboot -machine hpet=off
   -append "console=ttyS0 panic=-1 random.trust_cpu=off"` — the two runtime
   flags apply the HPET/RDRAND mitigations the config-fragment documents;
   `GUEST_READY` must appear within 120 s and QEMU must exit (guest
   poweroff). A failing /init makes the kernel panic and QEMU exit nonzero,
   so the gate cannot hang on a broken image.
