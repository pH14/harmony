# Install Harmony

Install the command-line program and the matching Linux guest runtime. The CLI alone can inspect your host, but OCI execution also needs Harmony's kernel and `initramfs-oci.cpio.gz`.

## Requirements

Use a [supported host](../reference/compatibility.md). The guest build below targets **native Ubuntu 24.04 on x86-64 or Arm64**. It downloads and compiles a Linux kernel and userland, so allow substantial disk space and build time. Internet access is needed during installation and image acquisition.

Install [Rust with rustup](https://www.rust-lang.org/tools/install). The checkout selects Rust 1.97.0 automatically. There is no published binary release or verified Homebrew installation path at present.

## 1. Build the CLI

On Ubuntu 24.04, install the host tools:

```sh
sudo apt-get update
sudo apt-get install -y build-essential git pkg-config libssl-dev \
  flex bison libelf-dev bc cpio kmod bzip2 wget python3 \
  xz-utils patch curl ca-certificates rsync
```

On macOS with Apple silicon, install the Xcode command-line tools first:

```sh
xcode-select --install
```

Then, on either host:

```sh
git clone https://github.com/pH14/harmony.git
cd harmony
git rev-parse HEAD
cargo build --locked --release -p harmony-cli
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/harmony "$HOME/.local/bin/harmony"
export PATH="$HOME/.local/bin:$PATH"
harmony --help
```

Save the printed Git commit alongside your test inputs. Keep this checkout: the next step builds the guest runtime from the same source. Add the `PATH` export to your shell's startup file to keep it in new terminals.

On macOS, sign the installed executable so it can use Hypervisor.framework:

```sh
codesign --force --sign - --entitlements cli/harmony.entitlements \
  "$HOME/.local/bin/harmony"
```

Repeat signing whenever you replace the executable.

## 2. Build the Linux guest runtime

Run these commands from the checkout on **native Ubuntu 24.04**, using the same CPU architecture as the machine that will run Harmony:

```sh
case "$(uname -m)" in
  x86_64) guest_target=x86_64-unknown-linux-musl ;;
  aarch64) guest_target=aarch64-unknown-linux-musl ;;
  *) echo 'A native x86-64 or Arm64 Linux builder is required'; exit 1 ;;
esac
rustup toolchain install nightly-2026-06-16 --profile minimal \
  --component rust-src --target "$guest_target"
make -C consonance/harmony-linux fetch
```

On x86-64 Ubuntu 24.04, select the kernel instruction baselines for its compiler:

```sh
export HARMONY_RDTSC_ALLOWLIST="$PWD/consonance/harmony-linux/linux/rdtsc-allowlist-gha.txt"
export HARMONY_RDRAND_ALLOWLIST="$PWD/consonance/harmony-linux/linux/rdrand-allowlist-gha.txt"
```

Build and install the runtime:

```sh
consonance/harmony-linux/scripts/build-platform-runtime.sh
guest_arch=$(uname -m)
mkdir -p "$HOME/.local/share/harmony/guest/$guest_arch"
cp -R "consonance/harmony-linux/build/$guest_arch/." \
  "$HOME/.local/share/harmony/guest/$guest_arch/"
export HARMONY_GUEST_DIR="$HOME/.local/share/harmony/guest/$guest_arch"
```

The x86-64 directory must contain `bzImage` and `initramfs-oci.cpio.gz`. The Arm64 directory must contain `Image` and `initramfs-oci.cpio.gz`. Keep all artifacts from this build together. A stock distribution kernel or a plain fixture initramfs is not a substitute.

If a kernel instruction check fails, stop and retain its error output. Compiler differences can change the checked instructions; do not bypass the check to complete installation. Use the documented Ubuntu version and see [troubleshooting](troubleshooting.md).

### Use the runtime on macOS

Build on a native Arm64 Linux machine or Arm64 Linux VM using **the same Harmony commit** as the macOS CLI. Copy the complete `build/aarch64` directory to the Mac, for example as `$HOME/.local/share/harmony/guest/aarch64`, then set:

```sh
export HARMONY_GUEST_DIR="$HOME/.local/share/harmony/guest/aarch64"
```

The guest builder does not run directly on macOS. An x86-64 Linux build cannot supply the Arm64 runtime. Add `HARMONY_GUEST_DIR` to your shell's startup file if you keep artifacts outside the CLI's automatically discovered directory.

## 3. Check access to the hypervisor

On Linux, your user needs read/write access to `/dev/kvm`. If the device exists but access is denied on Ubuntu:

```sh
sudo usermod -aG kvm "$USER"
```

Log out and back in before trying again. If the device is absent, enable hardware virtualization, or enable nested virtualization in the machine hosting your Linux VM.

Now inspect all prerequisites:

```sh
harmony preflight
harmony preflight --json
```

`ready yes` means the CLI found a supported run loop, a host classified as proven, a hypervisor, and the required artifact filenames. It does not certify your application image. Some supported hosts are classified as `expected` and require the explicit OCI opt-in described in [system compatibility](../reference/compatibility.md).

## 4. Prepare to acquire images

Install and start [Docker](https://docs.docker.com/engine/install/) or [Podman](https://podman.io/docs/installation) if you will use registry image names. Confirm that your user can run `docker image ls` or `podman image ls`. A saved image archive or local OCI layout can be used without a running container engine.

Continue with [your first reproducible run](../tutorials/first-run.md).
