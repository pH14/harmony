#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/../../.." && pwd)
demo=$root/demos/nova
build=$demo/.build
mkdir -p "$build" "$demo/public/engine" "$demo/public/licenses"
revision=26bb785c9deddb66a17717b21bb4e328f03ade32
if [ ! -d "$build/quicknes/.git" ]; then
    git clone --quiet https://github.com/libretro/QuickNES_Core.git "$build/quicknes"
fi
git -C "$build/quicknes" checkout --quiet --detach "$revision"
test "$(git -C "$build/quicknes" rev-parse HEAD)" = "$revision"
# Keep the browser core unmodified. Its runtime identity is distinct from the
# native Harmony build, which applies its own idle-loop optimization.
make -C "$build/quicknes" clean >/dev/null
make -C "$build/quicknes" -j4 platform=emscripten DEBUG=0 OPTIMIZE=-O2 \
    GIT_VERSION="$revision" CC=emcc CXX=em++ AR=emar TARGET=quicknes.a >"$build/quicknes-build.log" 2>&1
em++ -O2 -fno-exceptions -fno-rtti "$demo/tools/frontend.cpp" \
    "$build/quicknes/quicknes.a" -I"$build/quicknes/libretro/libretro-common/include" \
    -sMODULARIZE=1 -sEXPORT_ES6=1 -sENVIRONMENT=web,worker,node \
    '-sEXPORTED_RUNTIME_METHODS=["HEAPU8"]' -sALLOW_MEMORY_GROWTH=1 -sFILESYSTEM=0 -sINITIAL_MEMORY=16777216 \
    '-sEXPORTED_FUNCTIONS=["_malloc","_free","_nova_load","_nova_run","_nova_state_size","_nova_save","_nova_restore","_nova_pixels","_nova_width","_nova_height","_nova_ram","_nova_ram_size","_nova_sram","_nova_sram_size","_nova_audio_enable","_nova_audio_samples","_nova_audio_count","_nova_audio_clear"]' \
    -o "$demo/public/engine/quicknes.js"
PATH="${CC65_BIN_DIR:-}:$PATH" "$root/workloads/nes/scripts/build-nova-rom.sh" "$build/nova"
cp "$build/nova/nova.nes" "$demo/public/nova.nes"
wasm-pack build "$demo/rust" --target web --release --out-dir pkg --no-opt
cp "$build/quicknes/LICENSE" "$demo/public/licenses/QuickNES.txt"
cp "$demo/CREDITS.md" "$demo/public/licenses/CREDITS.md"
. "$root/workloads/nes/nova-versions.env"
curl --fail --location --retry 3 --output "$demo/public/licenses/nova-source.tar.gz" "$NOVA_URL"
printf '%s  %s\n' "$NOVA_SHA256" "$demo/public/licenses/nova-source.tar.gz" | sha256sum --check --status
# Include the full GPL text shipped with upstream source, plus corresponding
# emulator source and the exact browser frontend/build recipe in the site.
tar -xzf "$demo/public/licenses/nova-source.tar.gz" -C "$build"
cp "$build/NovaTheSquirrel-$NOVA_COMMIT/LICENSE.txt" "$demo/public/licenses/GPL-3.0.txt"
git -C "$build/quicknes" archive --format=tar.gz --output="$demo/public/licenses/quicknes-source.tar.gz" "$revision"
cp "$demo/tools/frontend.cpp" "$demo/public/licenses/frontend.cpp"
cp "$demo/tools/build-runtime.sh" "$demo/public/licenses/build-runtime.sh"

cargo vendor --locked --manifest-path "$demo/rust/Cargo.toml" "$build/rust-dependencies" > "$build/vendor-config.toml"
tar -czf "$demo/public/licenses/rust-dependencies.tar.gz" -C "$build" rust-dependencies
rm -rf "$build/rust-dependencies"

(cd "$demo" && node tools/build-panorama.mjs && node tools/build-sprites.mjs)
git -C "$root" archive --format=tar.gz --output="$demo/public/licenses/harmony-source.tar.gz" HEAD
