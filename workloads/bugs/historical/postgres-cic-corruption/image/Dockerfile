# SPDX-License-Identifier: AGPL-3.0-or-later
# The PostgreSQL CREATE INDEX CONCURRENTLY workload image
# (workloads/bugs/historical/postgres-cic-corruption).
#
# One file, two arms: the default build args give 14.3, which carries the bug,
# and the control arm passes PG_VERSION=14.4 with that release's sha256. The
# two images then differ in the PostgreSQL sources alone.
#
# PostgreSQL is built from the official tarball rather than taken from a distro
# package because the oracle needs the contrib amcheck extension plus the
# pg_amcheck client, and because 14.3 has no current distro build at all.
#
# The platform is fixed because the guest kernel is x86_64: an arm64 builder
# would otherwise produce an image whose binaries the guest cannot run.

ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

# hadolint ignore=DL3029
FROM --platform=linux/amd64 ${DEBIAN_IMAGE} AS build

ARG PG_VERSION=14.3
ARG PG_SHA256=279057368bf59a919c05ada8f95c5e04abb43e74b9a2a69c3d46a20e07a9af38
ARG PG_SOURCE_BASE=https://ftp.postgresql.org/pub/source

# Rows in the churned table, and the fillfactor that leaves room for HOT
# versions. The low fillfactor also spreads the rows over about 2200 pages, so
# a build's heap scan takes longer than one churn cycle.
ARG SEED_ROWS=20000
ARG FILLFACTOR=10

# The guest postgres uid/gid. It is baked here so the runtime stage inherits
# the ownership through COPY --from rather than re-chowning a whole cluster.
ARG PG_UID=70

SHELL ["/bin/sh", "-euxc"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        bison \
        build-essential \
        ca-certificates \
        curl \
        flex \
    && rm -rf /var/lib/apt/lists/*

RUN curl --fail --silent --show-error --location --retry 3 \
        --output /tmp/postgresql.tar.bz2 \
        "${PG_SOURCE_BASE}/v${PG_VERSION}/postgresql-${PG_VERSION}.tar.bz2" \
    && echo "${PG_SHA256}  /tmp/postgresql.tar.bz2" | sha256sum --check --strict - \
    && mkdir -p /tmp/postgresql \
    && tar -xf /tmp/postgresql.tar.bz2 -C /tmp/postgresql --strip-components=1 \
    && cd /tmp/postgresql \
    && ./configure --prefix=/usr/lib/postgresql \
        --without-readline --without-zlib --disable-nls --without-icu \
    && make -j"$(nproc)" \
    && make install \
    # amcheck is a contrib extension and is not built by the top-level target;
    # the pg_amcheck client that drives it lives in src/bin and is installed
    # above.
    && make -C contrib/amcheck \
    && make -C contrib/amcheck install \
    && rm -rf /tmp/postgresql /tmp/postgresql.tar.bz2 \
    && test -x /usr/lib/postgresql/bin/postgres \
    && test -x /usr/lib/postgresql/bin/pg_amcheck

COPY postgresql.conf.append seed.sql /build/

# initdb runs here, not in the guest: the cluster system identifier it mints
# from time, pid and random bytes is snapshotted into the image, so no
# initdb-time nondeterminism is left at run time. The seed goes in through a
# throwaway postmaster, and the cluster is shut down cleanly before the layer
# is committed.
RUN groupadd --system --gid "${PG_UID}" postgres \
    && useradd --system --uid "${PG_UID}" --gid "${PG_UID}" \
        --home-dir /var/lib/postgresql --shell /bin/sh postgres \
    && mkdir -p /var/lib/postgresql/data /build/sock \
    && chown -R "${PG_UID}:${PG_UID}" /var/lib/postgresql /build \
    && cd /var/lib/postgresql \
    && setpriv --reuid="${PG_UID}" --regid="${PG_UID}" --clear-groups \
        env LC_ALL=C TZ=UTC HOME=/var/lib/postgresql \
        /usr/lib/postgresql/bin/initdb -D /var/lib/postgresql/data \
        --locale=C --encoding=UTF8 -A trust -U postgres -N \
    && cat /build/postgresql.conf.append >>/var/lib/postgresql/data/postgresql.conf \
    && setpriv --reuid="${PG_UID}" --regid="${PG_UID}" --clear-groups \
        env LC_ALL=C TZ=UTC HOME=/var/lib/postgresql \
        /usr/lib/postgresql/bin/pg_ctl -D /var/lib/postgresql/data -w -t 120 \
        -o "-k /build/sock -c listen_addresses=''" -l /build/seed.log start \
    && setpriv --reuid="${PG_UID}" --regid="${PG_UID}" --clear-groups \
        env LC_ALL=C TZ=UTC HOME=/var/lib/postgresql \
        /usr/lib/postgresql/bin/psql -h /build/sock -U postgres -d postgres \
        -v ON_ERROR_STOP=1 -qtAX \
        -v seed_rows="${SEED_ROWS}" -v fillfactor="${FILLFACTOR}" \
        -f /build/seed.sql \
    && setpriv --reuid="${PG_UID}" --regid="${PG_UID}" --clear-groups \
        env LC_ALL=C TZ=UTC HOME=/var/lib/postgresql \
        /usr/lib/postgresql/bin/pg_ctl -D /var/lib/postgresql/data -w -t 120 \
        -m fast stop \
    && rm -rf /build

# The whole image is unpacked into guest RAM on every boot, so the tree ships
# only what the bundle runs. initdb goes too: the cluster it made is already in
# the image.
RUN rm -rf \
        /usr/lib/postgresql/include \
        /usr/lib/postgresql/lib/bitcode \
        /usr/lib/postgresql/lib/pgxs \
        /usr/lib/postgresql/share/doc \
        /usr/lib/postgresql/share/man \
    && find /usr/lib/postgresql/lib -name '*.a' -delete \
    && for binary in /usr/lib/postgresql/bin/*; do \
        case "$(basename "${binary}")" in \
            postgres|psql|pg_isready|pg_amcheck|pg_ctl) ;; \
            *) rm -f "${binary}" ;; \
        esac; \
    done \
    && find /usr/lib/postgresql -type f \( -perm -u+x -o -name '*.so' \) \
        -exec strip --strip-unneeded {} + 2>/dev/null || true

# hadolint ignore=DL3029
FROM --platform=linux/amd64 ${DEBIAN_IMAGE}

ARG PG_VERSION=14.3
ARG PG_UID=70

LABEL org.opencontainers.image.title="harmony postgres-cic-corruption workload"
LABEL org.opencontainers.image.version="${PG_VERSION}"

SHELL ["/bin/sh", "-euxc"]

# iproute2 is the one addition to the base: the setup script brings loopback up
# so the PostgreSQL 14 statistics collector gets its socket.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends iproute2 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid "${PG_UID}" postgres \
    && useradd --system --uid "${PG_UID}" --gid "${PG_UID}" \
        --home-dir /var/lib/postgresql --shell /bin/sh postgres

COPY --from=build /usr/lib/postgresql /usr/lib/postgresql
COPY --from=build /var/lib/postgresql /var/lib/postgresql
COPY bundle /etc/harmony/bundle
COPY setup.sh node.sh ready.sh hooks.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh \
        /opt/harmony/ready.sh /opt/harmony/hooks.sh \
    && test -d /var/lib/postgresql/data \
    && test "$(stat -c '%u:%g' /var/lib/postgresql/data)" = "${PG_UID}:${PG_UID}"

CMD ["/opt/harmony/node.sh"]
