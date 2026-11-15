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
COPY workloads/harness/targets/xline/go.mod workloads/harness/targets/xline/go.sum targets/xline/
RUN (cd kit && go mod download) && (cd targets/xline && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/xline/main.go targets/xline/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/xline && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/xline-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/xline-harness \
    && test -x /out/xline-harness

FROM ${RUST_IMAGE} AS instrumented-build

ARG XLINE_COMMIT=71479a4fabdd12fe67eec0bee95e652976937541
ARG XLINE_SOURCE_SHA256=c89409f3f9cbf2f16cb3c4b05dee400c48fe87cb144d2fbd0aad4491eafd837f
ARG XLINE_PROTO_COMMIT=769ab7e09ea3976f5b95ca101326a424b4abd08e
ARG XLINE_PROTO_SHA256=3d1a9961874563328b99b705a0ec18b3d74ec9f3dc0dbb5d7e0e309db9f8e668
ARG CURP_PROTO_COMMIT=19cfc8d48da30c190e240a477802b2b7f2a14633
ARG CURP_PROTO_SHA256=84e3cdf6feaf6f4bb6b1c0205c3076a443e014c894d6a07ed51c69e66b5d83d9

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends clang libclang-dev libprotobuf-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
RUN fetch() { \
        curl --fail --silent --show-error --location --retry 3 --output "$1.tar.gz" "$2" \
        && echo "$3  $1.tar.gz" | sha256sum --check --strict - \
        && mkdir -p "$4" \
        && tar -xf "$1.tar.gz" -C "$4" --strip-components=1 \
        && rm "$1.tar.gz"; \
    } \
    && fetch xline "https://github.com/xline-kv/xline/archive/${XLINE_COMMIT}.tar.gz" "${XLINE_SOURCE_SHA256}" xline \
    && fetch xline-proto "https://github.com/xline-kv/xline-proto/archive/${XLINE_PROTO_COMMIT}.tar.gz" "${XLINE_PROTO_SHA256}" xline/crates/xlineapi/proto \
    && fetch curp-proto "https://github.com/xline-kv/curp-proto/archive/${CURP_PROTO_COMMIT}.tar.gz" "${CURP_PROTO_SHA256}" xline/crates/curp/proto/common \
    && rm xline/rust-toolchain.toml

# The executable references the instrumentation crate, which forwards the
# coverage callbacks to /usr/lib/libvoidstar.so. Adding it changes only the
# new crate's entries in the lock file. Cargo also rewrites the lock file in
# format 4, so the check compares it after mapping it back to format 3.
RUN cd xline \
    && sed -i '/^\[dependencies\]$/a antithesis-instrumentation = "=0.1.0"' crates/xline/Cargo.toml \
    && grep -q '^antithesis-instrumentation = "=0.1.0"$' crates/xline/Cargo.toml \
    && printf '\nuse antithesis_instrumentation as _;\n' >> crates/xline/src/main.rs \
    && cp Cargo.lock /build/Cargo.lock.upstream \
    && cargo fetch \
    && sed -e 's/^version = 4$/version = 3/' -e 's/%2F/\//g' Cargo.lock > /build/Cargo.lock.normalized \
    && { diff /build/Cargo.lock.upstream /build/Cargo.lock.normalized > /build/Cargo.lock.diff || test $? -eq 1; } \
    && ! grep -q '^<' /build/Cargo.lock.diff

# The pinned source targets Rust 1.74, and newer compilers add lints that its
# deny attributes would turn into errors.
ENV RUSTFLAGS="--cap-lints=warn -Ccodegen-units=1 -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3 -Cllvm-args=-sanitizer-coverage-trace-pc-guard -Clink-args=-Wl,--build-id"
RUN mkdir -p /out/xline /out/symbols \
    && target="$(rustc -vV | sed -n 's/^host: //p')" \
    && (cd xline && cargo build --release --locked --target "${target}" -p xline --bin xline) \
    && cp "xline/target/${target}/release/xline" /out/xline/xline \
    && nm --defined-only /out/xline/xline | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/xline.sym.tsv \
    && test -s /out/symbols/xline.sym.tsv \
    && grep -q __sanitizer_cov_trace_pc_guard /out/symbols/xline.sym.tsv \
    && sha256sum /out/xline/xline | awk '{print $1 "  /opt/xline/xline"}' > /out/harmony-instrumented-events \
    && cp xline/LICENSE /out/XLINE-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG XLINE_COMMIT=71479a4fabdd12fe67eec0bee95e652976937541
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

COPY --from=instrumented-build /out/xline/xline /opt/xline/xline
COPY --from=harness-build /out/xline-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/xline.sym.tsv /symbols/
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/XLINE-LICENSE /licenses/xline.txt

RUN chmod 0755 /opt/xline/xline /opt/harmony/xline-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/xline/xline \
    && test -x /opt/harmony/xline-harness \
    && test -s /symbols/xline.sym.tsv \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/xline.txt

LABEL org.opencontainers.image.title="harmony Xline harness workload"
LABEL org.opencontainers.image.version="${XLINE_COMMIT}"
LABEL org.opencontainers.image.description="upstream Xline source built with Rust trace-pc-guard coverage, driven by the Harmony harness kit"

COPY workloads/harness/targets/xline/bundle /etc/harmony/bundle
COPY workloads/harness/targets/xline/setup.sh workloads/harness/targets/xline/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
