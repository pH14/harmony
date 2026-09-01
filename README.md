# Harmony

Harmony is a deterministic test environment designed to source, and then perfectly reproduce, difficult-to-catch bugs. It is heavily (_heavily_) inspired by Antithesis, the pioneer of the autonomous testing space.

Harmony is the composition of two components:

`consonance`: the deterministic hypervisor that can (in theory!) run arbitrary Linux workloads with perfect reproducibility

`dissonance`: the state explorer that injects entropy into those workloads

Together, the progression back and forth between `consonance` and `dissonance` allows the system to bring the principles of deterministic simulation testing to systems that weren't designed with such testing in mind.

## Try it

```bash
brew tap ph14/harmony
brew install harmony-cli
harmony preflight
```

Then run PostgreSQL under the deterministic hypervisor. The script below starts
a real `postgres:16-alpine` server, hammers it with four concurrent writers
inserting 20,000 rows of `random()` and `clock_timestamp()`, draws lottery
numbers, and fingerprints the whole table — everything a normal machine cannot
reproduce:

```bash
curl -fsSL https://raw.githubusercontent.com/pH14/harmony/main/demo/pg-demo.sh -o pg-demo.sh
harmony oci run postgres:16-alpine --ram-mib 1024 -- /bin/sh -c "$(cat pg-demo.sh)"
```

```
=== postgres 16.15 under a deterministic hypervisor ===
--- four concurrent writers, 20000 rows ---
  who  | count |    avg_random
-------+-------+-------------------
 alice |  5000 | 0.499175922134935
 ...
--- the server's clock and 'random' draws ---
          server_now           |      lottery
-------------------------------+-------------------
 1970-01-01 00:02:52.819654+00 | 24 23 15 26 31 25

--- whole-table fingerprint (every timestamp, every random(), every row order) ---
55d8d366ab3f229ca5ec4a01886110fe

digest      6047f6d5dbcf383dd4460843f93b3b9c375385cd56dc889abc20169ca7a0b449
```

Run it again: the lottery numbers, every one of the 20,000 random values, the
server's clock, the writer interleaving, and the final digest come back
byte-identical. The same script under plain `docker run` produces a different
fingerprint every time. The digest is a function of the image bytes, the guest
artifacts, and the seed, and is architecture-scoped (an arm64 run and an x86-64
run are separate universes).

## Disclaimers

* As you can probably tell, this project's development is heavily (_heavily_) assisted by AI. The process of building such a system in this way is as much of an experiment as Harmony itself.

* It might not work at all :)

## Development

Before contributing, configure the repository's local hooks and credential-leak checks as
described in [`docs/SECRET-HYGIENE.md`](docs/SECRET-HYGIENE.md).

## License

Harmony is free software, licensed under the GNU Affero General Public License v3.0 or later (`AGPL-3.0-or-later`) — see [`LICENSE`](LICENSE).
