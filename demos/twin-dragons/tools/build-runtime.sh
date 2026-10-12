#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/../../.." && pwd)
demo=$root/demos/twin-dragons
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
    '-sEXPORTED_FUNCTIONS=["_malloc","_free","_nes_load","_nes_run","_nes_state_size","_nes_save","_nes_restore","_nes_pixels","_nes_width","_nes_height","_nes_ram","_nes_ram_size","_nes_audio_enable","_nes_audio_samples","_nes_audio_count","_nes_audio_clear"]' \
    -o "$demo/public/engine/quicknes.js"
"$root/workloads/nes/scripts/build-twin-dragons-rom.sh" "$build/twin-dragons"
cp "$build/twin-dragons/twin-dragons.nes" "$demo/public/twin-dragons.nes"
wasm-pack build "$demo/rust" --target web --release --out-dir pkg --no-opt
cp "$build/quicknes/LICENSE" "$demo/public/licenses/QuickNES.txt"
cp "$demo/CREDITS.md" "$demo/public/licenses/CREDITS.md"
# Ship only the upstream zlib license beside the altered ROM. The upstream
# archive also holds sound files whose rights are reserved, so the build reads
# level data, tiles and tables from it and never publishes it.
cp "$build/twin-dragons/UPSTREAM-LICENSE.txt" "$demo/public/licenses/twin-dragons-LICENSE.txt"
. "$root/workloads/nes/twin-dragons-versions.env"
curl --fail --location --retry 3 --output "$build/twin-dragons.tar.gz" "$TWIN_DRAGONS_URL"
test "$(sha256sum <"$build/twin-dragons.tar.gz" | cut -d ' ' -f 1)" = "$TWIN_DRAGONS_SHA256"
rm -rf "$build/twin-dragons-source"
mkdir -p "$build/twin-dragons-source"
upstream="twin-dragons-nes-$TWIN_DRAGONS_COMMIT"
tar -xzf "$build/twin-dragons.tar.gz" -C "$build/twin-dragons-source" \
    "$upstream/src/levels" "$upstream/src/graphics/twin_dragons.chr" \
    "$upstream/src/metatiles.mfk" "$upstream/src/levels.mfk" "$upstream/src/anims.mfk"
git -C "$build/quicknes" archive --format=tar.gz --output="$demo/public/licenses/quicknes-source.tar.gz" "$revision"
cp "$demo/tools/frontend.cpp" "$demo/public/licenses/frontend.cpp"
cp "$demo/tools/build-runtime.sh" "$demo/public/licenses/build-runtime.sh"

cargo vendor --locked --manifest-path "$demo/rust/Cargo.toml" "$build/rust-dependencies" > "$build/vendor-config.toml"
tar -czf "$demo/public/licenses/rust-dependencies.tar.gz" -C "$build" rust-dependencies
rm -rf "$build/rust-dependencies"

(cd "$demo" && node tools/build-panorama.mjs && node tools/build-sprites.mjs)
git -C "$root" archive --format=tar.gz --output="$demo/public/licenses/harmony-source.tar.gz" HEAD
