#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
modules=${1:?missing module list}
boot=/opt/boot-jdk/bin
jmods=(/build/jdk/build/linux-*/images/jmods)
[ "${#jmods[@]}" -eq 1 ]
"$boot/jlink" --module-path "${jmods[0]}" --add-modules "$modules" --output /build/plain-java
mkdir -p /build/jmods
"$boot/javac" --add-exports java.base/jdk.internal.org.objectweb.asm=ALL-UNNAMED /build/ModuleHashes.java
while IFS=@ read -r module version; do
    extracted="/build/modules/$module"
    original="${jmods[0]}/$module.jmod"
    "$boot/jmod" extract --dir "$extracted" "$original"
    /build/plain-java/bin/java --add-exports java.base/jdk.internal.misc=ALL-UNNAMED \
        jdk.internal.misc.HarmonyRewriter "$extracted/classes"
    "$boot/java" --add-exports java.base/jdk.internal.org.objectweb.asm=ALL-UNNAMED \
        -cp /build ModuleHashes "$extracted/classes/module-info.class"
    args=(--class-path "$extracted/classes")
    for pair in libs:lib cmds:bin config:conf legal-notices:legal; do
        option=${pair%%:*}
        directory=${pair#*:}
        if [ -d "$extracted/$directory" ]; then
            args+=("--$option" "$extracted/$directory")
        fi
    done
    platform=$("$boot/jmod" describe "$original" | awk '$1 == "platform" {print $2}')
    if [ -n "$platform" ]; then
        args+=(--target-platform "$platform")
    fi
    "$boot/jmod" create "${args[@]}" "/build/jmods/$module.jmod"
done < <(/build/plain-java/bin/java --list-modules)
rewritten=(/build/jmods/*.jmod)
if [ "${#rewritten[@]}" -gt 1 ]; then
    "$boot/jmod" hash --module-path /build/jmods --hash-modules ".*" /build/jmods/java.base.jmod
fi
"$boot/jlink" --module-path /build/jmods --add-modules "$modules" --output /opt/java \
    --no-header-files --no-man-pages --strip-java-debug-attributes
/opt/java/bin/java -version 2>&1 | grep -q 'Server VM'
