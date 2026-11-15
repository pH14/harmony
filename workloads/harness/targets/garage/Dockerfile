# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.27-bookworm@sha256:5cf287a799e6b94384bad13d16b14904c531f51ba65792237e122ce42b392f61
ARG RUST_IMAGE=docker.io/library/rust:1.97.0-bookworm@sha256:8fa55b2f3ddf97471ab6a767bfa3f37e6bad0986ba823e75fea57e2a2a5c3073

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
COPY workloads/harness/targets/garage/go.mod workloads/harness/targets/garage/go.sum targets/garage/
COPY workloads/harness/targets/garage/cpuid-xgetbv.sh /build/cpuid-xgetbv.sh
RUN (cd kit && go mod download) && (cd targets/garage && go mod download && bash /build/cpuid-xgetbv.sh /build/cpuid)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/garage/main.go targets/garage/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/garage && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/garage-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/garage-harness \
    && test -x /out/garage-harness

FROM ${RUST_IMAGE} AS instrumented-build

ARG GARAGE_VERSION=v2.4.1
ARG GARAGE_SOURCE_SHA256=
ARG GARAGE_SOURCE_BASE=https://git.deuxfleurs.fr/Deuxfleurs/garage/archive

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

WORKDIR /build
RUN sha256="${GARAGE_SOURCE_SHA256}" \
    && if [ -z "${sha256}" ]; then \
        case "${GARAGE_VERSION}" in \
            v2.4.1) sha256=9149931f0e567a66cd96c33b6349e8f1900279ec6ae57c60bfd29d450104c553 ;; \
            *) echo "pass GARAGE_SOURCE_SHA256 for ${GARAGE_VERSION}" >&2; exit 1 ;; \
        esac; \
    fi \
    && curl --fail --silent --show-error --location --retry 3 \
       --output garage-source.tar.gz \
       "${GARAGE_SOURCE_BASE}/${GARAGE_VERSION}.tar.gz" \
    && echo "${sha256}  garage-source.tar.gz" | sha256sum --check --strict - \
    && mkdir garage \
    && tar -xf garage-source.tar.gz -C garage --strip-components=1 \
    && rm garage-source.tar.gz

# The executable references the instrumentation crate, which forwards the
# coverage callbacks to /usr/lib/libvoidstar.so. Adding it changes only the
# new crate's entries in the lock file.
RUN cd garage \
    && sed -i '/^\[dependencies\]$/a antithesis-instrumentation = "=0.1.0"' src/garage/Cargo.toml \
    && grep -q '^antithesis-instrumentation = "=0.1.0"$' src/garage/Cargo.toml \
    && printf '\nuse antithesis_instrumentation as _;\n' >> src/garage/main.rs \
    && cp Cargo.lock /build/Cargo.lock.upstream \
    && cargo fetch \
    && ! diff /build/Cargo.lock.upstream Cargo.lock | grep '^<'

COPY workloads/harness/targets/garage/sodium-xgetbv.sh /build/sodium-xgetbv.sh
RUN bash /build/sodium-xgetbv.sh

ENV RUSTFLAGS="-Ccodegen-units=1 -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3 -Cllvm-args=-sanitizer-coverage-trace-pc-guard -Clink-args=-Wl,--build-id"
RUN mkdir -p /out/garage /out/symbols \
    && target="$(rustc -vV | sed -n 's/^host: //p')" \
    && (cd garage && cargo build --release --locked --target "${target}" -p garage --bin garage) \
    && cp "garage/target/${target}/release/garage" /out/garage/garage \
    && nm --defined-only /out/garage/garage | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/garage.sym.tsv \
    && test -s /out/symbols/garage.sym.tsv \
    && grep -q __sanitizer_cov_trace_pc_guard /out/symbols/garage.sym.tsv \
    && sha256sum /out/garage/garage | awk '{print $1 "  /opt/garage/garage"}' > /out/harmony-instrumented-events \
    && cp garage/LICENSE /out/GARAGE-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG GARAGE_VERSION=v2.4.1
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

COPY --from=instrumented-build /out/garage/garage /opt/garage/garage
COPY --from=harness-build /out/garage-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/garage.sym.tsv /symbols/
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/GARAGE-LICENSE /licenses/garage.txt

RUN chmod 0755 /opt/garage/garage /opt/harmony/garage-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/garage/garage \
    && test -x /opt/harmony/garage-harness \
    && test -s /symbols/garage.sym.tsv \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/garage.txt

LABEL org.opencontainers.image.title="harmony Garage harness workload"
LABEL org.opencontainers.image.version="${GARAGE_VERSION}"
LABEL org.opencontainers.image.description="upstream Garage source built with Rust trace-pc-guard coverage, driven by the Harmony harness kit"

ARG HARNESS_BUNDLE=bundle
COPY workloads/harness/targets/garage/${HARNESS_BUNDLE} /etc/harmony/bundle
COPY workloads/harness/targets/garage/setup.sh workloads/harness/targets/garage/node.sh workloads/harness/targets/garage/ready.sh /opt/harmony/
COPY workloads/harness/targets/garage/garage.toml.in /opt/garage/garage.toml.in

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh /opt/harmony/ready.sh

CMD ["/opt/harmony/node.sh", "1"]
