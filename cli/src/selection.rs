// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::config::Result;
#[derive(Clone, Debug, clap::Args)]
pub struct Selection {
    pub run: String,
    #[arg(long)]
    pub finding: Option<usize>,
}
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Point {
    #[arg(long, conflicts_with_all=["rewind", "rewind_time"])]
    pub step: Option<usize>,
    #[arg(long, conflicts_with_all=["step", "rewind_time"])]
    pub rewind: Option<usize>,
    #[arg(long, conflicts_with_all=["step", "rewind"])]
    pub rewind_time: Option<String>,
}
impl Point {
    pub fn specified(&self) -> bool {
        self.step.is_some() || self.rewind.is_some() || self.rewind_time.is_some()
    }
    pub fn resolve(&self, boundaries: &[(usize, u64)], anchor: usize) -> Result<usize> {
        if let Some(step) = self.step {
            if boundaries.iter().any(|(n, _)| *n == step) {
                return Ok(step);
            }
            return Err(format!("step {step} is not a recorded boundary").into());
        }
        if let Some(n) = self.rewind {
            let step = anchor
                .checked_sub(n)
                .ok_or("rewind precedes the initial boundary")?;
            if !boundaries.iter().any(|(n, _)| *n == step) {
                return Err("rewind does not resolve to a recorded boundary".into());
            }
            return Ok(step);
        }
        if let Some(duration) = &self.rewind_time {
            let amount = millis(duration)?
                .checked_mul(1_000_000)
                .ok_or("duration overflow")?;
            let end = boundaries
                .iter()
                .find(|(n, _)| *n == anchor)
                .ok_or("selected endpoint has no time evidence")?
                .1;
            let time = end
                .checked_sub(amount)
                .ok_or("rewind precedes the initial boundary")?;
            return boundaries
                .iter()
                .rev()
                .find(|(n, t)| *n <= anchor && *t <= time)
                .map(|(n, _)| *n)
                .ok_or_else(|| "rewind precedes the initial boundary".into());
        }
        Ok(anchor)
    }
}
pub fn millis(text: &str) -> Result<u64> {
    let n = if let Some(ms) = text.strip_suffix("ms") {
        ms.parse()?
    } else {
        crate::config::duration(text)?
            .checked_mul(1000)
            .ok_or("duration overflow")?
    };
    if n == 0 {
        return Err("duration must be positive".into());
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offsets_use_the_selected_anchor_and_time_rounds_to_a_recorded_boundary() {
        let boundaries = [
            (0, 100_000_000),
            (1, 600_000_000),
            (2, 1_600_000_000),
            (3, 1_700_000_000),
        ];
        assert_eq!(
            Point {
                rewind: Some(1),
                ..Default::default()
            }
            .resolve(&boundaries, 2)
            .unwrap(),
            1
        );
        assert_eq!(
            Point {
                rewind_time: Some("1s".into()),
                ..Default::default()
            }
            .resolve(&boundaries, 3)
            .unwrap(),
            1
        );
        assert!(
            Point {
                rewind: Some(4),
                ..Default::default()
            }
            .resolve(&boundaries, 3)
            .is_err()
        );
        assert!(
            Point {
                step: Some(4),
                ..Default::default()
            }
            .resolve(&boundaries, 3)
            .is_err()
        );
        assert!(
            Point {
                rewind_time: Some("2s".into()),
                ..Default::default()
            }
            .resolve(&boundaries, 3)
            .is_err()
        );
    }
}
