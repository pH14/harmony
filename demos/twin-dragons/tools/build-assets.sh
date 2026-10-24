#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
demo=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$demo"
node tools/build-panorama.mjs
node tools/build-sprites.mjs
node tools/build-tobu-maps.mjs
node tools/build-famidash-music.mjs
node tools/build-famidash-maps.mjs
