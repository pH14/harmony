# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.27-bookworm@sha256:5cf287a799e6b94384bad13d16b14904c531f51ba65792237e122ce42b392f61

ARG HARMONY_RUNTIME_IMAGE=harmony-language-runtime:local
FROM ${GO_IMAGE} AS harness-build

ARG TARGETARCH
ENV GOTOOLCHAIN=local

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

WORKDIR /build
COPY workloads/harness/kit/go.mod workloads/harness/kit/go.sum kit/
COPY workloads/harness/targets/consul/go.mod workloads/harness/targets/consul/go.sum targets/consul/
RUN (cd kit && go mod download) && (cd targets/consul && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/consul/main.go targets/consul/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/consul && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/consul-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/consul-harness \
    && test -x /out/consul-harness

FROM ${GO_IMAGE} AS instrumented-build

ARG TARGETARCH
ARG CONSUL_VERSION=v2.1.0
ARG CONSUL_SOURCE_SHA256=
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG CONSUL_SOURCE_BASE=https://github.com/hashicorp/consul/archive/refs/tags
ENV CGO_ENABLED=1 GOFLAGS=-trimpath GOTOOLCHAIN=local

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl gcc libc6-dev make patch \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

WORKDIR /build
RUN sha256="${CONSUL_SOURCE_SHA256}" \
    && if [ -z "${sha256}" ]; then \
        case "${CONSUL_VERSION}" in \
            v2.1.0) sha256=208087b8f4d546cd186849ebfa162b5a34703dbdc5ee75f96d061177690f7b8d ;; \
            *) echo "pass CONSUL_SOURCE_SHA256 for ${CONSUL_VERSION}" >&2; exit 1 ;; \
        esac; \
    fi \
    && curl --fail --silent --show-error --location --retry 3 \
       --output consul-source.tar.gz \
       "${CONSUL_SOURCE_BASE}/${CONSUL_VERSION}.tar.gz" \
    && echo "${sha256}  consul-source.tar.gz" | sha256sum --check --strict - \
    && mkdir consul \
    && tar -xf consul-source.tar.gz -C consul --strip-components=1 \
    && rm consul-source.tar.gz \
    && (cd consul && go mod download)

RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"

COPY workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
     /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch
COPY workloads/languages/go/antithesis-go-toolexec-v0.8.0-stdlib.patch /build/stdlib.patch
COPY workloads/languages/go/configure-instrumentation.sh /build/configure-instrumentation.sh

RUN sdk_cache_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && test -d "${sdk_cache_dir}" \
    && mkdir -p /build/antithesis-sdk-go \
    && cp -a "${sdk_cache_dir}/." /build/antithesis-sdk-go/ \
    && chmod -R u+w /build/antithesis-sdk-go \
    && patch --batch --forward --fuzz=0 -d /build/antithesis-sdk-go -p1 \
       < /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
    && patch --batch --forward --fuzz=0 -d /build/antithesis-sdk-go -p1 < /build/stdlib.patch \
    && (cd /build/antithesis-sdk-go \
        && gofmt -w tools/antithesis-go-toolexec/config.go tools/antithesis-go-toolexec/policy.go tools/antithesis-go-toolexec/harmony_std_test.go \
        && go test ./tools/antithesis-go-toolexec && go install ./tools/antithesis-go-toolexec) \
    && grep -F -q 'linux && (amd64 || arm64) && cgo' \
       /build/antithesis-sdk-go/internal/voidstar_handler.go

RUN mkdir -p /out/consul /out/symbols /out/stdlib \
    && sdk_dir=/build/antithesis-sdk-go \
    && (cd /build/consul \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           bash /build/configure-instrumentation.sh "${sdk_dir}" /out/stdlib . > /out/stdlib/instrumentation.env \
        && source /out/stdlib/instrumentation.env \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=consul \
           go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/consul/consul .) \
    && test -x /out/consul/consul \
    && set -- /out/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/consul/consul \
    && sha256sum /out/consul/consul \
       | awk '{print $1 "  /opt/consul/consul"}' > /out/harmony-instrumented-events \
    && cp /build/consul/LICENSE /out/CONSUL-LICENSE \
    && cp "${sdk_dir}/LICENSE" /out/ANTITHESIS-SDK-GO-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG CONSUL_VERSION=v2.1.0
SHELL ["/bin/sh", "-euxc"]
# Image paths must stay distinct on case-insensitive hosts. The harness uses
# none of the iptables extensions whose names differ only in case. OpenSSL
# arrives only through Kerberos, which ip, tc and iptables never load, and its
# random-number instructions are outside the reviewed set.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends iproute2 iptables \
    && rm -rf /var/lib/apt/lists/* \
    && dpkg --purge --force-depends libssl3 \
    && ip -V && tc -V && iptables -V \
    && for dir in /usr/lib/*/xtables; do \
        ls "$dir" | tr A-Z a-z | sort | uniq -d | while read -r name; do \
            find "$dir" -maxdepth 1 -iname "$name" -delete; \
        done; \
    done
COPY workloads/languages/reviewed/bookworm-x86_64.txt /tmp/harmony-base-review
RUN if [ "$(uname -m)" = x86_64 ]; then \
        mkdir -p /etc/harmony && cp /tmp/harmony-base-review /etc/harmony/instruction-allowlist; \
    fi && rm /tmp/harmony-base-review

COPY --from=instrumented-build /out/consul/consul /opt/consul/consul
COPY --from=harness-build /out/consul-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/*.sym.tsv /symbols/
COPY --from=instrumented-build /out/stdlib /symbols/stdlib
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/CONSUL-LICENSE /licenses/consul.txt
COPY --from=instrumented-build /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt

RUN chmod 0755 /opt/consul/consul /opt/harmony/consul-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/consul/consul \
    && test -x /opt/harmony/consul-harness \
    && set -- /symbols/*.sym.tsv \
    && test -s "$1" \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/consul.txt \
    && test -s /licenses/antithesis-sdk-go.txt

LABEL org.opencontainers.image.title="harmony Consul harness workload"
LABEL org.opencontainers.image.version="${CONSUL_VERSION}"
LABEL org.opencontainers.image.description="upstream Consul source built through the Antithesis Go instrumentation pipeline, driven by the Harmony harness kit"

ARG HARNESS_BUNDLE=bundle
COPY workloads/harness/targets/consul/${HARNESS_BUNDLE} /etc/harmony/bundle
COPY workloads/harness/targets/consul/setup.sh workloads/harness/targets/consul/node.sh /opt/harmony/
COPY workloads/harness/targets/consul/consul.hcl /opt/consul/consul.hcl

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
