#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
mkdir -p language-bases language-images language-images-evidence
runtime=$(bash workloads/languages/build-runtime.sh)
for language in c rust; do
    key=$(python3 workloads/languages/image-key.py "$language")
    base="harmony-language-$language-base:$key"
    if [[ ${REBUILD_IMAGES:-false} != true && -s "language-bases/$language.tar" ]]; then
        docker load --input "language-bases/$language.tar"
        docker image inspect "$base" >/dev/null
    else
        options=()
        if [[ ${REBUILD_IMAGES:-false} == true ]]; then options+=(--no-cache --pull); fi
        docker build --file "workloads/languages/$language/Dockerfile" --target language-base \
            --tag "$base" "${options[@]}" . 2>&1 | tee "language-images-evidence/$language-build.log"
        docker save --output "language-bases/$language.tar" "$base"
    fi
    docker build --file workloads/languages/compose.Dockerfile \
        --build-arg "HARMONY_LANGUAGE_IMAGE=$base" --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" \
        --tag "harmony-language-$language:local" .
    docker save --output "language-images/$language.tar" "harmony-language-$language:local"
done
(cd language-images && sha256sum -- *.tar >MANIFEST.sha256)
