// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use crate::fenwick::Fenwick;

#[derive(Clone, Debug, Default)]
struct Value {
    groups: Vec<u32>,
    weights: Fenwick,
}

#[derive(Clone, Debug, Default)]
struct Stratum {
    index: BTreeMap<u64, usize>,
    values: Vec<Value>,
    means: Fenwick,
}

#[derive(Clone, Debug, Default)]
pub struct Strata {
    scalars: Vec<Stratum>,
    slots: Vec<Vec<(usize, usize)>>,
}

impl Strata {
    pub fn width(&self) -> usize {
        self.scalars.len()
    }

    pub fn clear(&mut self) {
        self.scalars.clear();
        self.slots.clear();
    }

    pub fn push(&mut self, key: &[u64], weight: f64) {
        if self.scalars.len() < key.len() {
            self.scalars.resize_with(key.len(), Stratum::default);
        }
        let g = self.slots.len() as u32;
        let mut slots = Vec::with_capacity(key.len());
        for (stratum, &v) in self.scalars.iter_mut().zip(key) {
            let vi = match stratum.index.get(&v) {
                Some(&vi) => vi,
                None => {
                    let vi = stratum.values.len();
                    stratum.index.insert(v, vi);
                    stratum.values.push(Value::default());
                    stratum.means.push(0.0);
                    vi
                }
            };
            let value = &mut stratum.values[vi];
            let gi = value.groups.len();
            value.groups.push(g);
            value.weights.push(weight);
            let mean = value.weights.total() / value.groups.len() as f64;
            stratum.means.set(vi, mean);
            slots.push((vi, gi));
        }
        self.slots.push(slots);
    }

    pub fn set(&mut self, g: usize, weight: f64) {
        for (stratum, &(vi, gi)) in self.scalars.iter_mut().zip(&self.slots[g]) {
            let value = &mut stratum.values[vi];
            value.weights.set(gi, weight);
            let mean = value.weights.total() / value.groups.len() as f64;
            stratum.means.set(vi, mean);
        }
    }

    pub fn pick(&self, scalar: usize, unit_value: f64, unit_group: f64) -> u32 {
        let stratum = &self.scalars[scalar];
        let vi = stratum.means.find(unit_value * stratum.means.total());
        let value = &stratum.values[vi];
        let gi = value.weights.find(unit_group * value.weights.total());
        value.groups[gi]
    }
}
