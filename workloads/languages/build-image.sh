#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
language=${1:?usage: build-image.sh c|rust [docker build options]}
shift
case "$language" in c|rust) ;; *) echo "unsupported language: $language" >&2; exit 2 ;; esac
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
key=$(python3 workloads/languages/image-key.py "$language")
base="harmony-language-$language-base:$key"
docker build --file "workloads/languages/$language/Dockerfile" \
    --target language-base --tag "$base" "$@" .
runtime=$(bash workloads/languages/build-runtime.sh)
docker build --file workloads/languages/compose.Dockerfile \
    --build-arg "HARMONY_LANGUAGE_IMAGE=$base" --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" \
    --tag "harmony-language-$language:local" .
