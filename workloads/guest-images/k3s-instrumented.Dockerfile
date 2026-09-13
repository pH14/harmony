# syntax=docker/dockerfile:1.7
ARG GOLANG
FROM ${GOLANG} AS build

ARG ANTITHESIS_SDK_VERSION
ARG BUILD_DATE
ARG COMMIT
ARG GIT_TAG

RUN apk -U --no-cache add bash git gcc musl-dev file curl ca-certificates jq linux-headers \
    zlib-dev tar zip squashfs-tools coreutils openssl-dev libffi-dev libseccomp libseccomp-dev \
    libseccomp-static make libuv-static sqlite-dev sqlite-static libselinux libselinux-dev \
    zstd pigz alpine-sdk btrfs-progs-dev btrfs-progs-static gawk yq apparmor-dev

WORKDIR /go/src/github.com/k3s-io/k3s
COPY . .

RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}" \
    && make -C .harmony/libvoidstar BUILD_DIR=/opt/harmony all \
    && sdk_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && test -d "$sdk_dir" \
    && sed -i "s|buildDate=\$(date -u '+%Y-%m-%dT%H:%M:%SZ')|buildDate=${BUILD_DATE}|" scripts/build \
    && sed -i 's|CGO_ENABLED=1 "${GO}" build $BLDFLAGS|CGO_ENABLED=1 "${GO}" build -toolexec=/go/bin/antithesis-go-toolexec $BLDFLAGS|' scripts/build \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ./scripts/download \
    && mkdir -p /opt/harmony/symbols \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ANTITHESIS_SDK_MODULE_DIR="$sdk_dir" \
       ANTITHESIS_SYMBOLS_DIR=/opt/harmony/symbols ANTITHESIS_SYMBOL_PREFIX=k3s-server \
       ./scripts/build \
    && NO_DAPPER=true GIT_TAG="$GIT_TAG" COMMIT="$COMMIT" TREE_STATE=clean DIRTY= \
       STATIC_BUILD=false ./scripts/package-cli \
    && test -x dist/artifacts/k3s \
    && set -- /opt/harmony/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation bin/k3s \
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
