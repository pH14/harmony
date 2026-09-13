# syntax=docker/dockerfile:1.7
ARG GOLANG
FROM ${GOLANG} AS build

ARG ANTITHESIS_SDK_VERSION
ARG BUILD_DATE
ARG COMMIT
ARG GIT_TAG

COPY .harmony/k3s-builder-apk.lock /tmp/k3s-builder-apk.lock

RUN apk -U --no-cache add bash git gcc musl-dev file curl ca-certificates jq linux-headers \
    zlib-dev tar zip squashfs-tools coreutils openssl-dev libffi-dev libseccomp libseccomp-dev \
    libseccomp-static make libuv-static sqlite-dev sqlite-static libselinux libselinux-dev \
    zstd pigz alpine-sdk btrfs-progs-dev btrfs-progs-static gawk yq \
    && apk info -v | sort > /tmp/k3s-builder-apk.actual \
    && cmp /tmp/k3s-builder-apk.lock /tmp/k3s-builder-apk.actual

WORKDIR /go/src/github.com/k3s-io/k3s
COPY . .

RUN go mod download "github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && sdk_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && go_cache="$(go env GOCACHE)" \
    && test -d "$sdk_dir" \
    && chmod -R u+w "$sdk_dir" \
    && patch -d "$sdk_dir" -p1 < .harmony/antithesis-go-toolexec-reproducible.patch \
    && (cd "$sdk_dir" \
       && go test ./tools/antithesis-go-toolexec \
          -run 'Test(PackageVariableCanCallInstrumentedCode|SelectedPackagePreservesFirstSourceIndex)$' \
       && go install ./tools/antithesis-go-toolexec) \
    && make -C .harmony/libvoidstar BUILD_DIR=/opt/harmony all \
    && patch -p1 < .harmony/k3s-source-pins.patch \
    && go mod edit -require="github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && go mod edit -replace="github.com/antithesishq/antithesis-sdk-go=$sdk_dir" \
    && sed -i "s|buildDate=\$(date -u '+%Y-%m-%dT%H:%M:%SZ')|buildDate=${BUILD_DATE}|" scripts/build \
    && sed -i 's|CGO_ENABLED=1 "${GO}" build $BLDFLAGS|CGO_ENABLED=1 "${GO}" build -toolexec=/go/bin/antithesis-go-toolexec $BLDFLAGS|' scripts/build \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ./scripts/download \
    && mkdir -p /opt/harmony/symbols \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ANTITHESIS_SYMBOLS_DIR=/opt/harmony/symbols \
       ANTITHESIS_INSTRUMENT=github.com/k3s-io,k8s.io \
       ANTITHESIS_SYMBOL_PREFIX=k3s-server \
       ./scripts/build \
    && sha256sum bin/k3s bin/cni > /tmp/k3s-build-first.sha256 \
    && rm -rf "$go_cache" /opt/harmony/symbols/* \
    && rm -f bin/k3s bin/k3s-* bin/kubectl bin/containerd bin/crictl bin/ctr bin/cni \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ANTITHESIS_SYMBOLS_DIR=/opt/harmony/symbols \
       ANTITHESIS_INSTRUMENT=github.com/k3s-io,k8s.io \
       ANTITHESIS_SYMBOL_PREFIX=k3s-server \
       ./scripts/build \
    && sha256sum --check /tmp/k3s-build-first.sha256 \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ./scripts/package-cli \
    && test -x dist/artifacts/k3s \
    && set -- /opt/harmony/symbols/*.sym.tsv \
    && test -s "$1" \
    && shard_dir="$go_cache/antithesis-symbols" \
    && for package_index in \
         'github.com%k3s-io%k3s%*.pkg' \
         'github.com%k3s-io%kine%*.pkg' \
         'k8s.io%kubernetes%*.pkg'; do \
         set -- "$shard_dir"/$package_index; \
         if ! test -s "$1"; then \
           echo "missing instrumented K3s package index matching $package_index in $shard_dir" >&2; \
           find "$shard_dir" -maxdepth 1 -type f -name '*.pkg' -print | sort | head -100 >&2; \
           exit 1; \
         fi; \
       done \
    && sha256sum bin/k3s > /opt/harmony/instrumented-server.sha256 \
    && mkdir -p /opt/harmony/runtime/usr/lib \
    && cp /opt/harmony/libvoidstar.so /opt/harmony/runtime/usr/lib/libvoidstar.so \
    && for executable in bin/k3s bin/containerd-shim-runc-v2 bin/runc bin/cni; do \
         ldd "$executable" | awk '/=> \// {print $3} /^\// {print $1}' ; \
       done | sort -u > /tmp/runtime-libraries \
    && while read -r library; do cp -L --parents "$library" /opt/harmony/runtime; done </tmp/runtime-libraries

FROM scratch
COPY --from=build /go/src/github.com/k3s-io/k3s/dist/artifacts/k3s /k3s
COPY --from=build /opt/harmony/runtime /runtime
COPY --from=build /opt/harmony/symbols /symbols
COPY --from=build /opt/harmony/instrumented-server.sha256 /instrumented-server.sha256
