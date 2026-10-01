# SPDX-License-Identifier: AGPL-3.0-or-later
ARG HARMONY_LANGUAGE_IMAGE
ARG HARMONY_RUNTIME_IMAGE
FROM ${HARMONY_RUNTIME_IMAGE} AS runtime
FROM ${HARMONY_LANGUAGE_IMAGE}
COPY workloads/languages/reviewed/bookworm-x86_64.txt /tmp/harmony-base-review
RUN if [ "$(uname -m)" = x86_64 ]; then \
        mkdir -p /etc/harmony && cp /tmp/harmony-base-review /etc/harmony/instruction-allowlist; \
    fi \
    && rm /tmp/harmony-base-review
COPY --from=runtime /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=runtime /out/park-launcher /opt/harmony/park-launcher
COPY --from=runtime /out/gcc-fixture /opt/harmony/gcc-fixture
COPY --from=runtime /out/gcc.sym.tsv /symbols/gcc.sym.tsv
COPY --from=runtime /out/gcc-attestation /tmp/gcc-attestation
RUN cat /tmp/gcc-attestation >> /symbols/harmony-instrumented-events \
    && rm /tmp/gcc-attestation
