// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Debug, Default)]
pub struct Fenwick {
    tree: Vec<f64>,
    values: Vec<f64>,
}

impl Fenwick {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn push(&mut self, value: f64) {
        let i = self.values.len();
        self.values.push(0.0);
        self.tree.push(0.0);
        let k = i + 1;
        let low = k - (k & k.wrapping_neg());
        let mut sum = 0.0;
        let mut j = k - 1;
        while j > low {
            sum += self.tree[j - 1];
            j -= j & j.wrapping_neg();
        }
        self.tree[i] = sum;
        self.set(i, value);
    }

    pub fn set(&mut self, i: usize, value: f64) {
        let delta = value - self.values[i];
        self.values[i] = value;
        let mut k = i + 1;
        while k <= self.tree.len() {
            self.tree[k - 1] += delta;
            k += k & k.wrapping_neg();
        }
    }

    pub fn total(&self) -> f64 {
        let mut k = self.tree.len();
        let mut sum = 0.0;
        while k > 0 {
            sum += self.tree[k - 1];
            k -= k & k.wrapping_neg();
        }
        sum
    }

    pub fn find(&self, mut target: f64) -> usize {
        let n = self.tree.len();
        let mut pos = 0;
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next - 1] <= target {
                target -= self.tree[next - 1];
                pos = next;
            }
            step >>= 1;
        }
        let mut i = pos.min(n.saturating_sub(1));
        while i > 0 && self.values[i] <= 0.0 {
            i -= 1;
        }
        while i + 1 < n && self.values[i] <= 0.0 {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matches_linear_scan() {
        let mut f = Fenwick::default();
        let weights = [0.5, 0.0, 2.0, 1.0, 0.25, 3.0, 0.0, 1.5];
        for w in weights {
            f.push(w);
        }
        f.set(1, 0.75);
        let mut values = weights;
        values[1] = 0.75;
        let total: f64 = values.iter().sum();
        assert!((f.total() - total).abs() < 1e-9);
        for k in 0..1000 {
            let t = total * f64::from(k) / 1000.0;
            let mut acc = 0.0;
            let mut want = 0;
            for (i, v) in values.iter().enumerate() {
                if t < acc + v {
                    want = i;
                    break;
                }
                acc += v;
            }
            assert_eq!(f.find(t), want, "target {t}");
        }
    }
}
