// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub(crate) enum CopySite {
    Encode,
    Status,
    Decode,
}
#[derive(Debug)]
pub(crate) struct CopyWork {
    copies: [AtomicU64; 3],
    mappings: [[AtomicU64; 4]; 2],
}
impl Default for CopyWork {
    fn default() -> Self {
        Self {
            copies: std::array::from_fn(|_| AtomicU64::new(0)),
            mappings: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        }
    }
}
fn add(counter: &AtomicU64, value: usize) {
    let value = u64::try_from(value).unwrap_or(u64::MAX);
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
        Some(old.saturating_add(value))
    });
}
impl CopyWork {
    pub(crate) fn copied(&self, site: CopySite, bytes: usize) {
        add(&self.copies[site as usize], bytes);
    }
    pub(crate) fn mapping_attempt(&self, writable: bool) {
        add(&self.mappings[usize::from(writable)][0], 1);
    }
    pub(crate) fn mapping_result(&self, writable: bool, result: Option<(usize, usize)>) {
        let values = &self.mappings[usize::from(writable)];
        if let Some((range, span)) = result {
            add(&values[2], range);
            add(&values[3], span);
        } else {
            add(&values[1], 1);
        }
    }
    pub(crate) fn counters(&self, prefix: &str) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for (name, counter) in [
            "encoded_payload_bytes",
            "status_payload_bytes",
            "decoded_payload_bytes",
        ]
        .into_iter()
        .zip(&self.copies)
        {
            out.push((format!("{prefix}.{name}"), counter.load(Ordering::Relaxed)));
        }
        for (name, values) in ["import_mapping", "export_mapping"]
            .into_iter()
            .zip(&self.mappings)
        {
            for (leaf, counter) in ["attempts", "errors", "range_bytes", "span_bytes"]
                .into_iter()
                .zip(values)
            {
                out.push((
                    format!("{prefix}.{name}.{leaf}"),
                    counter.load(Ordering::Relaxed),
                ));
            }
        }
        out
    }
}
