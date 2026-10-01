#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
language=${1:?usage: prepare-ci-image.sh LANGUAGE}
key=$(python3 workloads/languages/image-key.py "$language")
base="harmony-language-$language-base:$key"
mkdir -p language-base language-image
if [[ ${REBUILD_IMAGES:-false} != true && -s language-base/base.tar ]]; then
    docker load --input language-base/base.tar
    docker image inspect "$base" >/dev/null
else
    options=()
    if [[ ${REBUILD_IMAGES:-false} == true ]]; then options+=(--no-cache --pull); fi
    docker build --file "workloads/languages/$language/Dockerfile" --target language-base \
        --tag "$base" "${options[@]}" .
    docker save --output language-base/base.tar "$base"
fi
runtime=$(bash workloads/languages/build-runtime.sh)
docker build --file workloads/languages/compose.Dockerfile \
    --build-arg "HARMONY_LANGUAGE_IMAGE=$base" --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" \
    --tag "harmony-language-$language:local" .
docker save --output "language-image/$language.tar" "harmony-language-$language:local"
