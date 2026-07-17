# Harmony

Harmony is a deterministic test environment designed to source, and then perfectly reproduce, difficult-to-catch bugs. It is heavily (_heavily_) inspired by Antithesis, the pioneer of the autonomous testing space.

Harmony is the composition of two components:

`consonance`: the deterministic hypervisor that can (in theory!) run arbitrary Linux workloads with perfect reproducibility

`dissonance`: the state explorer that injects entropy into those workloads

Together, the progression back and forth between `consonance` and `dissonance` allows the system to bring the principles of deterministic simulation testing to systems that weren't designed with such testing in mind.

## Disclaimers

* As you can probably tell, this project's development is heavily (_heavily_) assisted by AI. The process of building such a system in this way is as much of an experiment as Harmony itself.

* It might not work at all :)

## Development

Before contributing, configure the repository's local hooks and credential-leak checks as
described in [`docs/SECRET-HYGIENE.md`](docs/SECRET-HYGIENE.md).

## License

Harmony is free software, licensed under the GNU Affero General Public License v3.0 or later (`AGPL-3.0-or-later`) — see [`LICENSE`](LICENSE).
