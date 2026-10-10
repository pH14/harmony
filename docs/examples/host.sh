# SPDX-License-Identifier: AGPL-3.0-or-later
# example: build-cli
cargo build --locked --release -p harmony-cli
export PATH="$PWD/target/release:$PATH"
harmony --help
# endexample

# example: install-runtime
arch=$(uname -m)
if [ "$arch" = arm64 ]; then arch=aarch64; fi
mkdir -p .harmony-install/bin .harmony-install/share/harmony/guest/"$arch"
cp target/release/harmony .harmony-install/bin/harmony
cp consonance/harmony-linux/build/"$arch"/initramfs-oci.cpio.gz \
  .harmony-install/share/harmony/guest/"$arch"/
if [ "$arch" = aarch64 ]; then kernel=Image; else kernel=bzImage; fi
cp consonance/harmony-linux/build/"$arch"/"$kernel" \
  .harmony-install/share/harmony/guest/"$arch"/
export PATH="$PWD/.harmony-install/bin:$PATH"
harmony --help
# endexample
