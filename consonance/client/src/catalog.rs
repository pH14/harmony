// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
pub struct StateCatalog {
    declared: bool,
    names: BTreeMap<String, u32>,
    values: BTreeMap<u32, u64>,
}

impl StateCatalog {
    pub fn observe(&mut self, event_id: u32, bytes: &[u8]) -> Result<(), String> {
        if event_id == 0 {
            if self.declared {
                return Err("multiple SDK publishers are ambiguous on this event wire".into());
            }
            let names = decode(bytes)?;
            self.names = names;
            self.declared = true;
        } else if event_id >> 24 == 2 {
            let id = event_id & 0x00ff_ffff;
            if !self.declared || !self.names.values().any(|value| *value == id) {
                return Err(format!("SDK state register {id} was not declared"));
            }
            let [operation, tail @ ..] = bytes else {
                return Err("SDK state event is empty".into());
            };
            let value = u64::from_le_bytes(
                tail.try_into()
                    .map_err(|_| "SDK state value has wrong length")?,
            );
            match operation {
                0 => {
                    self.values.insert(id, value);
                }
                1 => {
                    self.values
                        .entry(id)
                        .and_modify(|v| *v = (*v).max(value))
                        .or_insert(value);
                }
                _ => return Err("unknown SDK state operation".into()),
            }
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<u64, String> {
        let id = self
            .names
            .get(name)
            .ok_or_else(|| format!("SDK state {name:?} is not declared"))?;
        self.values
            .get(id)
            .copied()
            .ok_or_else(|| format!("SDK state {name:?} has no published value"))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains_key(name)
    }
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], String> {
    let (value, rest) = input.split_at_checked(len).ok_or("truncated SDK catalog")?;
    *input = rest;
    Ok(value)
}

fn decode(mut bytes: &[u8]) -> Result<BTreeMap<String, u32>, String> {
    if take(&mut bytes, 5)? != b"SDKC\x01" {
        return Err("unsupported SDK catalog".into());
    }
    let count = u32::from_le_bytes(take(&mut bytes, 4)?.try_into().map_err(|_| "SDK count")?);
    let mut names = BTreeMap::new();
    let mut all_names = BTreeSet::new();
    let mut coordinates = BTreeSet::new();
    for _ in 0..count {
        let kind = take(&mut bytes, 1)?[0];
        let id = u32::from_le_bytes(take(&mut bytes, 4)?.try_into().map_err(|_| "SDK id")?);
        let len = u16::from_le_bytes(
            take(&mut bytes, 2)?
                .try_into()
                .map_err(|_| "SDK name length")?,
        );
        let name = std::str::from_utf8(take(&mut bytes, usize::from(len))?)
            .map_err(|_| "SDK name is not UTF-8")?
            .to_owned();
        let namespace = match kind {
            0..=3 => 1,
            4 => 2,
            5 => 3,
            _ => return Err("unknown SDK point kind".into()),
        };
        if id > 0x00ff_ffff
            || !coordinates.insert((namespace, id))
            || !all_names.insert(name.clone())
        {
            return Err("ambiguous SDK catalog coordinate or name".into());
        }
        if kind == 4 {
            names.insert(name, id);
        }
    }
    if !bytes.is_empty() {
        return Err("trailing SDK catalog bytes".into());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_points(points: &[(u8, u32, &str)]) -> Vec<u8> {
        let mut bytes = b"SDKC\x01".to_vec();
        bytes.extend((points.len() as u32).to_le_bytes());
        for (kind, id, name) in points {
            bytes.push(*kind);
            bytes.extend(id.to_le_bytes());
            bytes.extend((name.len() as u16).to_le_bytes());
            bytes.extend(name.as_bytes());
        }
        bytes
    }

    fn catalog(id: u32) -> Vec<u8> {
        catalog_points(&[(4, id, "gpa")])
    }

    fn state(operation: u8, value: u64) -> Vec<u8> {
        let mut bytes = vec![operation];
        bytes.extend(value.to_le_bytes());
        bytes
    }

    #[test]
    fn register_numbers_are_discovered_and_max_is_monotone() {
        let mut c = StateCatalog::default();
        c.observe(0, &catalog(97)).unwrap();
        for (op, value) in [(0, 42_u64), (1, 40), (1, 50)] {
            c.observe(0x0200_0061, &state(op, value)).unwrap();
        }
        assert_eq!(c.get("gpa").unwrap(), 50);
        assert!(c.observe(0, &catalog(97)).is_err());
    }

    #[test]
    fn state_events_require_a_declared_register() {
        let mut c = StateCatalog::default();
        assert!(c.observe(0x0200_0001, &state(0, 1)).is_err());

        c.observe(0, &catalog(97)).unwrap();
        assert!(c.observe(0x0200_0062, &state(0, 1)).is_err());
    }

    #[test]
    fn contains_distinguishes_declared_state_names() {
        let mut c = StateCatalog::default();
        c.observe(0, &catalog(97)).unwrap();
        assert!(c.contains("gpa"));
        assert!(!c.contains("missing"));
    }

    #[test]
    fn non_state_point_kinds_are_valid_but_not_state_registers() {
        let points = [
            (0, 11, "counter"),
            (1, 12, "flag"),
            (2, 13, "histogram"),
            (3, 14, "timer"),
            (5, 15, "blob"),
        ];
        let mut c = StateCatalog::default();
        c.observe(0, &catalog_points(&points)).unwrap();
        for (_, _, name) in points {
            assert!(!c.contains(name));
            assert!(c.get(name).is_err());
        }
    }

    #[test]
    fn catalog_enforces_id_bounds_and_unique_coordinates() {
        const MAX_ID: u32 = 0x00ff_ffff;
        assert!(decode(&catalog_points(&[(4, MAX_ID, "max")])).is_ok());
        assert!(decode(&catalog_points(&[(4, MAX_ID + 1, "too-high")])).is_err());
        assert!(decode(&catalog_points(&[(4, 7, "first"), (4, 7, "second")])).is_err());
        assert!(decode(&catalog_points(&[(4, 7, "same"), (4, 8, "same")])).is_err());
    }

    #[test]
    fn truncations_and_undeclared_values_fail() {
        let b = catalog(1);
        for end in 0..b.len() {
            assert!(StateCatalog::default().observe(0, &b[..end]).is_err());
        }
        assert!(
            StateCatalog::default()
                .observe(0x0200_0001, &[0; 9])
                .is_err()
        );
        let mut trailing = b.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        assert!(decode(&catalog(0x0100_0000)).is_err());
    }
}
