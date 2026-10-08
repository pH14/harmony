# SPDX-License-Identifier: AGPL-3.0-or-later
# The general-discovery PostgreSQL workload. It builds the same PostgreSQL
# 14.3 release as the focused postgres-cic-corruption case, from the same
# verified tarball and configure flags, and reuses that case's setup, node and
# readiness scripts. The seed table, configuration overlay and workload driver
# are this case's own. The build context is the repository root.

ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

# hadolint ignore=DL3029
FROM --platform=linux/amd64 ${DEBIAN_IMAGE} AS build

ARG PG_VERSION=14.3
ARG PG_SHA256=279057368bf59a919c05ada8f95c5e04abb43e74b9a2a69c3d46a20e07a9af38
ARG PG_SOURCE_BASE=https://ftp.postgresql.org/pub/source
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
    && make -C contrib/amcheck \
    && make -C contrib/amcheck install \
    && rm -rf /tmp/postgresql /tmp/postgresql.tar.bz2 \
    && test -x /usr/lib/postgresql/bin/postgres

COPY workloads/bugs/historical/postgres-cic-corruption/image/loopback-up.c /build/loopback-up.c
COPY workloads/bugs/historical/postgres-index-general/image/postgresql.conf.append \
     workloads/bugs/historical/postgres-index-general/image/seed.sql \
     workloads/bugs/historical/postgres-index-general/image/pg-general.c /build/
COPY workloads/bugs/historical/general/harmony_general.h /build/harmony_general.h
RUN cc -O2 -Wall -Werror -o /usr/local/bin/loopback-up /build/loopback-up.c \
    && cc -O2 -g -Wall -Wextra -Werror -std=gnu11 -I/build -I/usr/lib/postgresql/include \
        -o /usr/local/bin/pg-general /build/pg-general.c \
        -L/usr/lib/postgresql/lib -Wl,-rpath,/usr/lib/postgresql/lib -lpq

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
        -v ON_ERROR_STOP=1 -qtAX -f /build/seed.sql \
    && setpriv --reuid="${PG_UID}" --regid="${PG_UID}" --clear-groups \
        env LC_ALL=C TZ=UTC HOME=/var/lib/postgresql \
        /usr/lib/postgresql/bin/pg_ctl -D /var/lib/postgresql/data -w -t 120 \
        -m fast stop \
    && rm -rf /build

COPY workloads/bugs/historical/postgres-index-general/image/oracle-test.sh /test/oracle-test.sh
RUN setpriv --reuid="${PG_UID}" --regid="${PG_UID}" --clear-groups env HOME=/tmp \
        bash /test/oracle-test.sh /usr/local/bin/pg-general /var/lib/postgresql/data \
    && rm -rf /test

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

LABEL org.opencontainers.image.title="harmony postgres-index-general workload"
LABEL org.opencontainers.image.version="${PG_VERSION}"

SHELL ["/bin/sh", "-euxc"]

RUN groupadd --system --gid "${PG_UID}" postgres \
    && useradd --system --uid "${PG_UID}" --gid "${PG_UID}" \
        --home-dir /var/lib/postgresql --shell /bin/sh postgres

COPY --from=build /usr/lib/postgresql /usr/lib/postgresql
COPY --from=build /var/lib/postgresql /var/lib/postgresql
COPY workloads/bugs/historical/postgres-index-general/image/bundle /etc/harmony/bundle
COPY workloads/bugs/historical/postgres-cic-corruption/image/reviewed-x86_64.txt /etc/harmony/instruction-allowlist
COPY --from=build /usr/local/bin/loopback-up /opt/harmony/loopback-up
COPY --from=build /usr/local/bin/pg-general /opt/harmony/pg-general
COPY workloads/bugs/historical/postgres-cic-corruption/image/setup.sh \
     workloads/bugs/historical/postgres-cic-corruption/image/node.sh \
     workloads/bugs/historical/postgres-cic-corruption/image/ready.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh \
        /opt/harmony/ready.sh /opt/harmony/pg-general \
    && test -d /var/lib/postgresql/data \
    && test "$(stat -c '%u:%g' /var/lib/postgresql/data)" = "${PG_UID}:${PG_UID}"

CMD ["/opt/harmony/node.sh"]
