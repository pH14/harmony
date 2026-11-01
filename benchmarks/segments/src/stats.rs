// SPDX-License-Identifier: AGPL-3.0-or-later

use rand_chacha::{
    ChaCha8Rng,
    rand_core::{RngCore, SeedableRng},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation {
    pub time: f64,
    pub event: bool,
}

pub fn km_median(observations: &[Observation]) -> f64 {
    let mut sorted = observations.to_vec();
    sorted.sort_by(|a, b| a.time.total_cmp(&b.time));
    let mut at_risk = sorted.len() as f64;
    let mut survival = 1.0;
    let mut i = 0;
    while i < sorted.len() {
        let time = sorted[i].time;
        let mut deaths = 0.0;
        let mut removed = 0.0;
        while i < sorted.len() && sorted[i].time == time {
            if sorted[i].event {
                deaths += 1.0;
            }
            removed += 1.0;
            i += 1;
        }
        if deaths > 0.0 {
            survival *= 1.0 - deaths / at_risk;
            if survival <= 0.5 + 1e-12 {
                return time;
            }
        }
        at_risk -= removed;
    }
    f64::INFINITY
}

pub const RESAMPLES: usize = 2000;

pub fn bootstrap_interval(observations: &[Observation], seed: u64) -> (f64, f64) {
    let n = observations.len();
    if n == 0 {
        return (f64::INFINITY, f64::INFINITY);
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut medians = Vec::with_capacity(RESAMPLES);
    let mut sample = Vec::with_capacity(n);
    for _ in 0..RESAMPLES {
        sample.clear();
        for _ in 0..n {
            sample.push(observations[(rng.next_u64() % n as u64) as usize]);
        }
        medians.push(km_median(&sample));
    }
    medians.sort_by(f64::total_cmp);
    let low = medians[RESAMPLES * 25 / 1000];
    let high = medians[(RESAMPLES * 975).div_ceil(1000) - 1];
    (low, high)
}

pub fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.265_512_23
        + t * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let value = t * poly.exp();
    if x >= 0.0 { value } else { 2.0 - value }
}

pub fn logrank(a: &[Observation], b: &[Observation]) -> (f64, f64) {
    let mut times: Vec<f64> = a
        .iter()
        .chain(b)
        .filter(|o| o.event)
        .map(|o| o.time)
        .collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    let mut observed = 0.0;
    let mut expected = 0.0;
    let mut variance = 0.0;
    let at_risk =
        |group: &[Observation], t: f64| group.iter().filter(|o| o.time >= t).count() as f64;
    let deaths = |group: &[Observation], t: f64| {
        group.iter().filter(|o| o.event && o.time == t).count() as f64
    };
    for t in times {
        let (n1, n2) = (at_risk(a, t), at_risk(b, t));
        let (d1, d2) = (deaths(a, t), deaths(b, t));
        let (n, d) = (n1 + n2, d1 + d2);
        if n == 0.0 {
            continue;
        }
        observed += d1;
        expected += d * n1 / n;
        if n > 1.0 {
            variance += n1 * n2 * d * (n - d) / (n * n * (n - 1.0));
        }
    }
    if variance <= 0.0 {
        return (0.0, 1.0);
    }
    let chi2 = (observed - expected).powi(2) / variance;
    (chi2, erfc((chi2 / 2.0).sqrt()))
}

pub fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some((values[(n - 1) / 2] + values[n / 2]) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(pairs: &[(f64, bool)]) -> Vec<Observation> {
        pairs
            .iter()
            .map(|&(time, event)| Observation { time, event })
            .collect()
    }

    fn aml() -> (Vec<Observation>, Vec<Observation>) {
        let maintained = obs(&[
            (9.0, true),
            (13.0, true),
            (13.0, false),
            (18.0, true),
            (23.0, true),
            (28.0, false),
            (31.0, true),
            (34.0, true),
            (45.0, false),
            (48.0, true),
            (161.0, false),
        ]);
        let nonmaintained = obs(&[
            (5.0, true),
            (5.0, true),
            (8.0, true),
            (8.0, true),
            (12.0, true),
            (16.0, false),
            (23.0, true),
            (27.0, true),
            (30.0, true),
            (33.0, true),
            (43.0, true),
            (45.0, true),
        ]);
        (maintained, nonmaintained)
    }

    #[test]
    fn km_median_with_censoring() {
        let (maintained, nonmaintained) = aml();
        assert_eq!(km_median(&maintained), 31.0);
        assert_eq!(km_median(&nonmaintained), 23.0);
        assert_eq!(
            km_median(&obs(&[(3.0, true), (1.0, true), (2.0, true), (4.0, true)])),
            2.0
        );
        assert_eq!(
            km_median(&obs(&[
                (1.0, true),
                (2.0, false),
                (5.0, true),
                (5.0, false)
            ])),
            5.0
        );
        assert_eq!(
            km_median(&obs(&[
                (1.0, true),
                (5.0, false),
                (5.0, false),
                (5.0, false)
            ])),
            f64::INFINITY
        );
    }

    #[test]
    fn logrank_matches_the_aml_example() {
        let (maintained, nonmaintained) = aml();
        let (chi2, p) = logrank(&maintained, &nonmaintained);
        assert!((chi2 - 3.396_388_698_977_598).abs() < 1e-9, "{chi2}");
        assert!((p - 0.065_339_322_040_505).abs() < 1e-6, "{p}");
    }

    #[test]
    fn logrank_hand_worked() {
        let (chi2, _) = logrank(
            &obs(&[(1.0, true), (3.0, true)]),
            &obs(&[(2.0, true), (4.0, true)]),
        );
        assert!((chi2 - 8.0 / 13.0).abs() < 1e-12);
    }

    #[test]
    fn erfc_is_accurate() {
        for (x, want) in [
            (0.0, 1.0),
            (0.1, 0.887_537_083_981_715_2),
            (0.5, 0.479_500_122_186_953_5),
            (1.0, 0.157_299_207_050_285_16),
            (2.0, 0.004_677_734_981_047_264_5),
            (3.0, 2.209_049_699_858_543_8e-5),
            (5.0, 1.537_459_794_428_035e-12),
            (-1.0, 1.842_700_792_949_714_8),
        ] {
            let got = erfc(x);
            assert!(
                ((got - want) / want).abs() < 2e-7,
                "erfc({x}) = {got}, want {want}"
            );
        }
    }

    #[test]
    fn bootstrap_reproduces_and_allows_infinite_bounds() {
        let (maintained, _) = aml();
        let first = bootstrap_interval(&maintained, 9);
        assert_eq!(first, bootstrap_interval(&maintained, 9));
        assert!(first.0 <= 31.0 && first.1 >= 31.0);
        let mostly_censored = obs(&[(1.0, true), (9.0, false), (9.0, false), (9.0, false)]);
        assert_eq!(bootstrap_interval(&mostly_censored, 1).1, f64::INFINITY);
    }
}
