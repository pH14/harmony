// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(any(target_os = "linux", test))]
use std::io::{self, ErrorKind};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GuardBytes {
    pub live_exec_read_bytes: u64,
    pub sealed_exec_read_bytes: u64,
    pub live_exec_hashed_bytes: u64,
    pub comparison_input_bytes: u64,
    pub sealed_file_read_bytes: u64,
    pub sealed_file_hashed_bytes: u64,
    pub code_page_entries: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuardMeasurement {
    pub attempts: u64,
    pub errors: u64,
    pub elapsed_ns: u64,
    pub kernel_state_checks: u64,
    pub static_signing_checks: u64,
    pub image_censuses: u64,
    pub bytes: Option<GuardBytes>,
}

impl Default for GuardMeasurement {
    fn default() -> Self {
        Self {
            attempts: 0,
            errors: 0,
            elapsed_ns: 0,
            kernel_state_checks: 0,
            static_signing_checks: 0,
            image_censuses: 0,
            bytes: cfg!(target_os = "linux").then(GuardBytes::default),
        }
    }
}

impl GuardMeasurement {
    pub fn merge(&mut self, other: Self) {
        self.attempts = self.attempts.saturating_add(other.attempts);
        self.errors = self.errors.saturating_add(other.errors);
        self.elapsed_ns = self.elapsed_ns.saturating_add(other.elapsed_ns);
        self.kernel_state_checks = self
            .kernel_state_checks
            .saturating_add(other.kernel_state_checks);
        self.static_signing_checks = self
            .static_signing_checks
            .saturating_add(other.static_signing_checks);
        self.image_censuses = self.image_censuses.saturating_add(other.image_censuses);
        match (&mut self.bytes, other.bytes) {
            (Some(total), Some(delta)) => {
                total.live_exec_read_bytes = total
                    .live_exec_read_bytes
                    .saturating_add(delta.live_exec_read_bytes);
                total.sealed_exec_read_bytes = total
                    .sealed_exec_read_bytes
                    .saturating_add(delta.sealed_exec_read_bytes);
                total.live_exec_hashed_bytes = total
                    .live_exec_hashed_bytes
                    .saturating_add(delta.live_exec_hashed_bytes);
                total.comparison_input_bytes = total
                    .comparison_input_bytes
                    .saturating_add(delta.comparison_input_bytes);
                total.sealed_file_read_bytes = total
                    .sealed_file_read_bytes
                    .saturating_add(delta.sealed_file_read_bytes);
                total.sealed_file_hashed_bytes = total
                    .sealed_file_hashed_bytes
                    .saturating_add(delta.sealed_file_hashed_bytes);
                total.code_page_entries = total
                    .code_page_entries
                    .saturating_add(delta.code_page_entries);
            }
            _ => self.bytes = None,
        }
    }
}

#[allow(clippy::disallowed_methods)]
pub(crate) fn measured<T>(
    work: &mut GuardMeasurement,
    operation: impl FnOnce(&mut GuardMeasurement) -> std::io::Result<T>,
) -> std::io::Result<T> {
    work.attempts = work.attempts.saturating_add(1);
    let started = std::time::Instant::now();
    let result = operation(work);
    work.elapsed_ns = work
        .elapsed_ns
        .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    if result.is_err() {
        work.errors = work.errors.saturating_add(1);
    }
    result
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn read_exact_counted_at(
    mut read: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
    mut bytes: &mut [u8],
    mut offset: u64,
    transferred: &mut u64,
) -> io::Result<()> {
    while !bytes.is_empty() {
        match read(bytes, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "executable read ended early",
                ));
            }
            Ok(count) if count <= bytes.len() => {
                *transferred = transferred.saturating_add(count as u64);
                bytes = &mut bytes[count..];
                offset = offset.checked_add(count as u64).ok_or_else(|| {
                    io::Error::new(ErrorKind::InvalidInput, "executable read offset overflow")
                })?;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "executable reader exceeded buffer",
                ));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_work_preserves_failed_attempts_and_independent_owners() {
        let mut first = GuardMeasurement::default();
        let second = GuardMeasurement::default();
        measured(&mut first, |_| Ok(())).unwrap();
        assert!(measured::<()>(&mut first, |_| Err(std::io::Error::other("refused"))).is_err());
        assert_eq!(first.attempts, 2);
        assert_eq!(first.errors, 1);
        assert_eq!(second.attempts, 0);
        let mut total = GuardMeasurement::default();
        total.merge(first);
        assert_eq!(total, first);
        assert_eq!(total.bytes.is_some(), cfg!(target_os = "linux"));
    }
    #[test]
    fn counted_exact_read_preserves_partial_success_on_eof_error_and_interruption() {
        for ending in [ErrorKind::UnexpectedEof, ErrorKind::PermissionDenied] {
            let mut calls = 0;
            let mut bytes = [0_u8; 8];
            let mut transferred = 0;
            let error = read_exact_counted_at(
                |output, offset| {
                    calls += 1;
                    match calls {
                        1 => {
                            assert_eq!(offset, 9);
                            Err(io::Error::from(ErrorKind::Interrupted))
                        }
                        2 => {
                            assert_eq!(offset, 9);
                            output[..3].copy_from_slice(&[1, 2, 3]);
                            Ok(3)
                        }
                        3 => {
                            assert_eq!(offset, 12);
                            if ending == ErrorKind::UnexpectedEof {
                                Ok(0)
                            } else {
                                Err(io::Error::from(ending))
                            }
                        }
                        _ => unreachable!(),
                    }
                },
                &mut bytes,
                9,
                &mut transferred,
            )
            .unwrap_err();
            assert_eq!(error.kind(), ending);
            assert_eq!(transferred, 3);
            assert_eq!(&bytes[..3], &[1, 2, 3]);
        }
        let mut transferred = 0;
        let mut output = [0_u8; 7];
        read_exact_counted_at(
            |bytes, _| {
                let count = bytes.len().min(2);
                bytes[..count].fill(4);
                Ok(count)
            },
            &mut output,
            0,
            &mut transferred,
        )
        .unwrap();
        assert_eq!(transferred, 7);
        assert_eq!(output, [4; 7]);
    }
}
