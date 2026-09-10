-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Build-time seed for the CREATE INDEX CONCURRENTLY workload. Run once by the
-- Dockerfile through a throwaway postmaster so every boot starts from the same
-- on-disk bytes. :seed_rows and :fillfactor come from psql -v.

CREATE DATABASE faultlab;
\c faultlab
CREATE EXTENSION amcheck;

CREATE TABLE cic(id int PRIMARY KEY, k int, pad text)
    WITH (fillfactor = :fillfactor);
INSERT INTO cic
    SELECT g, g % 1000, repeat('x', 40) FROM generate_series(1, :seed_rows) g;
CREATE INDEX cic_k_idx ON cic(k);

-- The churn hook's loop, kept inside the server so each slice commits without
-- a client round trip; a round trip costs a few milliseconds of guest time and
-- would stretch the cycle past a heap scan. See hooks.sh for what the churn is
-- for. The value is rewritten at constant width so the versions stay HOT.
CREATE PROCEDURE churn(row_count int, slice_count int, round_count int)
LANGUAGE plpgsql AS $$
DECLARE
    stride int := greatest((SELECT max(id) FROM cic) / row_count, 1);
BEGIN
    FOR r IN 1..round_count LOOP
        FOR i IN 0..slice_count - 1 LOOP
            UPDATE cic SET pad = md5(pad)
                WHERE id = ANY (ARRAY(SELECT n * stride + stride
                                      FROM generate_series(i, row_count - 1, slice_count) n));
            COMMIT;
        END LOOP;
    END LOOP;
END
$$;

VACUUM ANALYZE cic;
CHECKPOINT;
