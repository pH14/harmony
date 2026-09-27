# Search an NES workload

The shared `harmony search --package nes` command supports the recognized **Super Mario Bros. (SMB)** and **Nova the Squirrel** ROMs. It identifies the game from the ROM hash; an arbitrary game or a different ROM revision is not automatically supported.

You need a ROM you are entitled to use and a matching host QuickNES library. Harmony does not distribute commercial ROMs. This is an optional workload; it is not required for testing your own Linux services.

## Prepare a host QuickNES core

On Linux or macOS with Git, Make, and a C/C++ compiler installed, run this from your Harmony checkout:

```sh
mkdir -p "$HOME/harmony-nes"
scripts/build-quicknes-core.sh "$HOME/harmony-nes/quicknes_libretro.so"
export HARMONY_QUICKNES_CORE="$HOME/harmony-nes/quicknes_libretro.so"
```

The helper checks out the pinned QuickNES revision, builds the host library, copies it to your chosen filename, and prints its hash. The `.so` output filename is also accepted on macOS; the helper copies the macOS dynamic library into that path.

For a source-built Nova ROM on Ubuntu, install `cc65` and use the included recipe:

```sh
sudo apt-get install -y cc65
workloads/nes/scripts/build-nova-rom.sh "$HOME/harmony-nes/nova"
```

Use the ROM path printed by the recipe. Retain the ROM and library bytes for future replay. Do not substitute an unrelated QuickNES build for a recorded run.

## Run a native campaign

Given your ROM and host library:

```sh
harmony search --package nes ./smb.nes \
  --core /absolute/path/to/quicknes_libretro.so \
  --seed 7 --workers 1 --executions 100 --actions 32 --out nes-run
```

Use the actual path you chose when building the shared library. `HARMONY_QUICKNES_CORE` can supply the default path. Native search does not use KVM/HVF or the Linux guest runtime, so a failing OCI preflight is not necessarily a blocker.

Inspect `nes-run/report.json` and keep the entire directory together with the ROM and core. The shared CLI does not implement NES replay through its `--replay` option; that option belongs to the faults package.

## Run inside a VM

This path requires a supported **Linux KVM** host, matching Harmony guest runtime, and an NES OCI image containing the static play agent and matching QuickNES core. On native Linux x86-64, after building the guest runtime, build the static core and image from your checkout:

```sh
mkdir -p "$HOME/harmony-nes"
HARMONY_QUICKNES_STATIC_OUTPUT="$HOME/harmony-nes/libquicknes_libretro.a" \
  scripts/build-quicknes-core.sh "$HOME/harmony-nes/quicknes_libretro.so"
HARMONY_QUICKNES_STATIC_LIB="$HOME/harmony-nes/libquicknes_libretro.a" \
  workloads/nes-guest/build-base-image.sh
```

The resulting layout is `consonance/harmony-linux/build/nes.oci`. The command below assumes you copy that directory to `./nes.oci` or substitute its actual path. This recipe is for x86-64; an Arm64 VM image needs the architecture-specific static toolchain and instruction qualification, and is not provided as a turnkey recipe here.

```sh
harmony search --package nes --backend consonance ./smb.nes \
  --image ./nes.oci --seed 7 --workers 1 \
  --executions 100 --actions 32 --out nes-vm-run
```

Guest kernel and initramfs discovery uses `HARMONY_GUEST_DIR`. Supply explicit paths with `--kernel` and `--base-initramfs` if necessary. `HARMONY_NES_IMAGE` can supply the image path.

The VM path supplies the ROM to the image as a read-only input. A VM result and a native result have different machine-state identities; do not compare their state digests as if they were the same execution.
