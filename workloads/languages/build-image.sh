#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
export BUILDAH_FORMAT=docker
language=${1:?usage: build-image.sh LANGUAGE [container build options]}
shift
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
key=$(python3 workloads/languages/image-key.py "$language")
base="harmony-language-$language-base:$key"
"${HARMONY_CONTAINER_TOOL:-docker}" build --file "workloads/languages/$language/Dockerfile" \
    --target language-base --tag "$base" "$@" .
runtime=$(bash workloads/languages/build-runtime.sh)
"${HARMONY_CONTAINER_TOOL:-docker}" build --file workloads/languages/compose.Dockerfile \
    --build-arg "HARMONY_LANGUAGE_IMAGE=$base" --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" \
    --tag "harmony-language-$language:local" .
