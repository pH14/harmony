# SPDX-License-Identifier: AGPL-3.0-or-later
# Sourced by harness image Go build stages. Sets HARNESS_GO_LDFLAGS for every
# `go build -ldflags` and, on arm64, the toolchain settings that image
# admission needs.
case "$(uname -m)" in
    aarch64)
        harness_goroot=$(go env GOROOT)
        if [ ! -e "$harness_goroot/.harmony-arm64-patches" ]; then
            for harness_patch in /build/harness-go/*.patch; do
                (cd "$harness_goroot" && git apply -p1 "$harness_patch")
            done
            : >"$harness_goroot/.harmony-arm64-patches"
        fi
        export GOARM64=v8.1
        if [ -x /opt/harmony-musl/bin/harmony-cc ]; then
            export CC=/opt/harmony-musl/bin/harmony-cc
        fi
        HARNESS_GO_LDFLAGS="-buildid= -B none"
        ;;
    *)
        HARNESS_GO_LDFLAGS=
        ;;
esac
export HARNESS_GO_LDFLAGS
