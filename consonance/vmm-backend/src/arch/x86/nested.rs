// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::error::{BackendError, Result};

pub const NESTED_HEADER_LEN: usize = 128;
pub const VMX_NESTED_MAX_LEN: usize = NESTED_HEADER_LEN + 2 * 4096;
pub const SVM_NESTED_MAX_LEN: usize = NESTED_HEADER_LEN + 4096;
pub const SVM_GIF_SET: u16 = 1 << 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum NestedFormat {
    Vmx = 0,
    Svm = 1,
}

impl NestedFormat {
    pub fn from_state(bytes: &[u8]) -> Result<Self> {
        match bytes.get(2..4) {
            Some([0, 0]) => Ok(Self::Vmx),
            Some([1, 0]) => Ok(Self::Svm),
            _ => Err(BackendError::Internal("unsupported nested state format")),
        }
    }

    pub fn maximum_len(self) -> usize {
        match self {
            Self::Vmx => VMX_NESTED_MAX_LEN,
            Self::Svm => SVM_NESTED_MAX_LEN,
        }
    }

    pub fn validate(self, bytes: &[u8], maximum: usize) -> Result<()> {
        validate_nested_state(bytes, maximum)?;
        if Self::from_state(bytes)? != self {
            return Err(BackendError::Internal("nested state vendor mismatch"));
        }
        Ok(())
    }
}

fn validate_nested_shape(bytes: &[u8], maximum: usize) -> Result<()> {
    if bytes.len() < NESTED_HEADER_LEN || bytes.len() > maximum {
        return Err(BackendError::Internal(
            "nested state length outside host capability",
        ));
    }
    let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let format = NestedFormat::from_state(bytes)?;
    if size != bytes.len() || bytes.len() > format.maximum_len() {
        return Err(BackendError::Internal(
            "invalid nested state format or size",
        ));
    }
    if format == NestedFormat::Svm {
        let flags = u16::from_le_bytes(bytes[..2].try_into().unwrap());
        let expected = if flags & 1 != 0 {
            SVM_NESTED_MAX_LEN
        } else {
            NESTED_HEADER_LEN
        };
        if size != expected
            || flags & !(3 | SVM_GIF_SET) != 0
            || (flags & 1 == 0 && bytes[8..NESTED_HEADER_LEN].iter().any(|&byte| byte != 0))
        {
            return Err(BackendError::Internal("invalid nested SVM state shape"));
        }
    }
    Ok(())
}

pub fn validate_nested_state(bytes: &[u8], maximum: usize) -> Result<()> {
    validate_nested_shape(bytes, maximum)?;
    let flags = u16::from_le_bytes(bytes[..2].try_into().unwrap());
    let allowed = match NestedFormat::from_state(bytes)? {
        NestedFormat::Vmx => 0,
        NestedFormat::Svm => SVM_GIF_SET,
    };
    if flags & !allowed != 0 {
        return Err(BackendError::Internal(
            "nested snapshot requires L1 outside guest mode",
        ));
    }
    Ok(())
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn nested_probe(maximum: usize) -> Result<Vec<u8>> {
    if !(NESTED_HEADER_LEN..=VMX_NESTED_MAX_LEN).contains(&maximum) {
        return Err(BackendError::Internal("unsupported nested capability size"));
    }
    let mut bytes = vec![0; maximum];
    bytes[4..8].copy_from_slice(&(maximum as u32).to_le_bytes());
    Ok(bytes)
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64", not(miri))))]
pub(crate) fn finish_nested_probe(mut bytes: Vec<u8>) -> Result<Vec<u8>> {
    let maximum = bytes.len();
    let size = u32::from_le_bytes(
        bytes
            .get(4..8)
            .ok_or(BackendError::Internal("short nested state probe"))?
            .try_into()
            .unwrap(),
    ) as usize;
    if !(NESTED_HEADER_LEN..=maximum).contains(&size) {
        return Err(BackendError::Internal(
            "KVM returned invalid nested state size",
        ));
    }
    bytes.truncate(size);
    validate_nested_shape(&bytes, maximum)?;
    Ok(bytes)
}

pub fn inactive_nested_state(format: NestedFormat) -> Vec<u8> {
    let mut bytes = vec![0; NESTED_HEADER_LEN];
    bytes[4..8].copy_from_slice(&(NESTED_HEADER_LEN as u32).to_le_bytes());
    bytes[2..4].copy_from_slice(&(format as u16).to_le_bytes());
    match format {
        NestedFormat::Vmx => bytes[8..24].fill(0xff),
        NestedFormat::Svm => bytes[..2].copy_from_slice(&SVM_GIF_SET.to_le_bytes()),
    }
    bytes
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn drain_dirty_pages_with_nested_reprotection(
    configuration: Option<(NestedFormat, usize)>,
    read: impl FnOnce(usize) -> Result<Vec<u8>>,
    drain: impl FnOnce() -> Result<Vec<u64>>,
    reload: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<Vec<u64>> {
    let nested = match configuration {
        Some((format, maximum)) => {
            let bytes = read(maximum)?;
            format.validate(&bytes, maximum)?;
            Some(bytes)
        }
        None => None,
    };
    let dirty = drain()?;
    if let Some(bytes) = nested {
        reload(&bytes)?;
    }
    Ok(dirty)
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn canonicalize_nested_metadata(bytes: &mut [u8]) -> Result<()> {
    if NestedFormat::from_state(bytes)? == NestedFormat::Svm {
        validate_nested_shape(bytes, SVM_NESTED_MAX_LEN)?;
        return Ok(());
    }
    if bytes.len() == NESTED_HEADER_LEN {
        return Ok(());
    }
    let vmcs = bytes
        .get_mut(NESTED_HEADER_LEN..)
        .ok_or(BackendError::Internal("short nested VMCS12"))?;
    if vmcs.len() < 824 || u32::from_le_bytes(vmcs[..4].try_into().unwrap()) != 0x11e5_7ed0 {
        return Err(BackendError::Internal("unsupported nested VMCS12 revision"));
    }
    let info = u32::from_le_bytes(vmcs[816..820].try_into().unwrap());
    if info & (1 << 31) == 0 {
        vmcs[816..820].fill(0);
        vmcs[820..824].fill(0);
    } else if info & (1 << 11) == 0 {
        vmcs[820..824].fill(0);
    }
    Ok(())
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn reload_nested_memory_slots<T: Copy>(
    slots: &[T],
    remove: impl Fn(T) -> T,
    mut install: impl FnMut(T) -> Result<()>,
) -> Result<()> {
    for &slot in slots {
        install(remove(slot))?;
        install(slot)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_slot_reload_preserves_mappings_and_stops_after_any_ioctl_failure() {
        let slots = [(1, 0x1000, 0x2000, 0x4000), (2, 0x5000, 0x8000, 0x3000)];
        let remove = |(slot, gpa, address, _)| (slot, gpa, address, 0);
        let expected = [remove(slots[0]), slots[0], remove(slots[1]), slots[1]];
        for stop in 0..=4 {
            let mut calls = Vec::new();
            let result = reload_nested_memory_slots(&slots, remove, |slot| {
                calls.push(slot);
                if calls.len() == stop {
                    Err(BackendError::InvalidState)
                } else {
                    Ok(())
                }
            });
            if stop == 0 {
                assert!(result.is_ok());
                assert_eq!(calls, expected);
            } else {
                assert!(result.is_err());
                assert_eq!(calls, expected[..stop]);
            }
        }
    }

    #[test]
    fn svm_preserves_gif_and_payload_but_rejects_guest_mode_and_vendor_mixing() {
        for flags in [0, SVM_GIF_SET] {
            let mut bytes = inactive_nested_state(NestedFormat::Svm);
            bytes[..2].copy_from_slice(&flags.to_le_bytes());
            NestedFormat::Svm
                .validate(&bytes, SVM_NESTED_MAX_LEN)
                .unwrap();
            assert!(
                NestedFormat::Vmx
                    .validate(&bytes, VMX_NESTED_MAX_LEN)
                    .is_err()
            );
            assert_eq!(finish_nested_probe(bytes.clone()).unwrap(), bytes);
            let original = bytes.clone();
            canonicalize_nested_metadata(&mut bytes).unwrap();
            assert_eq!(bytes, original);
        }
        let mut bytes = inactive_nested_state(NestedFormat::Svm);
        for flags in [1_u16, 2, 4, 8, 0x200] {
            bytes[..2].copy_from_slice(&flags.to_le_bytes());
            assert!(validate_nested_state(&bytes, SVM_NESTED_MAX_LEN).is_err());
        }
        bytes.resize(SVM_NESTED_MAX_LEN, 0x5a);
        bytes[4..8].copy_from_slice(&(SVM_NESTED_MAX_LEN as u32).to_le_bytes());
        bytes[..2].copy_from_slice(&(1 | SVM_GIF_SET).to_le_bytes());
        assert_eq!(finish_nested_probe(bytes.clone()).unwrap(), bytes);
        assert!(validate_nested_state(&bytes, SVM_NESTED_MAX_LEN).is_err());
        bytes[..2].copy_from_slice(&SVM_GIF_SET.to_le_bytes());
        assert!(finish_nested_probe(bytes).is_err());
    }

    #[test]
    fn nested_probe_preserves_every_returned_payload_byte_and_rejects_bad_sizes() {
        let mut bytes = nested_probe(VMX_NESTED_MAX_LEN).unwrap();
        bytes[NESTED_HEADER_LEN..].fill(0x5a);
        assert_eq!(finish_nested_probe(bytes.clone()).unwrap(), bytes);
        for size in [0, 127, VMX_NESTED_MAX_LEN as u32 + 1] {
            bytes[4..8].copy_from_slice(&size.to_le_bytes());
            assert!(finish_nested_probe(bytes.clone()).is_err());
        }
        assert!(nested_probe(0).is_err());
        assert!(nested_probe(VMX_NESTED_MAX_LEN + 1).is_err());
    }

    #[test]
    fn inactive_header_and_live_root_state_are_distinct_and_mode_flags_fail_closed() {
        let inactive = inactive_nested_state(NestedFormat::Vmx);
        validate_nested_state(&inactive, VMX_NESTED_MAX_LEN).unwrap();
        let mut live = inactive.clone();
        live[8..16].copy_from_slice(&0x3000_u64.to_le_bytes());
        assert_ne!(live, inactive);
        validate_nested_state(&live, VMX_NESTED_MAX_LEN).unwrap();
        for (offset, value) in [(0, 1), (0, 2), (0, 4), (0, 8), (2, 1)] {
            let mut invalid = live.clone();
            invalid[offset] = value;
            assert!(validate_nested_state(&invalid, VMX_NESTED_MAX_LEN).is_err());
        }
    }

    #[test]
    fn vmcs12_exit_info_canonicalizes_only_inactive_metadata_and_rejects_unknown_layouts() {
        let inactive = inactive_nested_state(NestedFormat::Vmx);
        let mut bytes = inactive.clone();
        canonicalize_nested_metadata(&mut bytes).unwrap();
        assert_eq!(bytes, inactive);
        let mut bytes = inactive_nested_state(NestedFormat::Vmx);
        bytes.resize(NESTED_HEADER_LEN + 4096, 0x5a);
        let length = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[NESTED_HEADER_LEN..NESTED_HEADER_LEN + 4]
            .copy_from_slice(&0x11e5_7ed0_u32.to_le_bytes());
        for (info, expected_info, expected_error) in [
            (0xed_u32, 0_u32, 0_u32),
            (0x8000_030e, 0x8000_030e, 0),
            (0x8000_0b0e, 0x8000_0b0e, 0x1234_5678),
        ] {
            bytes[NESTED_HEADER_LEN + 816..NESTED_HEADER_LEN + 820]
                .copy_from_slice(&info.to_le_bytes());
            bytes[NESTED_HEADER_LEN + 820..NESTED_HEADER_LEN + 824]
                .copy_from_slice(&0x1234_5678_u32.to_le_bytes());
            let mut expected = bytes.clone();
            expected[NESTED_HEADER_LEN + 816..NESTED_HEADER_LEN + 820]
                .copy_from_slice(&expected_info.to_le_bytes());
            expected[NESTED_HEADER_LEN + 820..NESTED_HEADER_LEN + 824]
                .copy_from_slice(&expected_error.to_le_bytes());
            let mut actual = bytes.clone();
            canonicalize_nested_metadata(&mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        bytes[NESTED_HEADER_LEN] ^= 1;
        let before = bytes.clone();
        assert!(canonicalize_nested_metadata(&mut bytes).is_err());
        assert_eq!(bytes, before);
        for length in [0, 127, 129, NESTED_HEADER_LEN + 823] {
            let mut bytes = vec![0; length];
            let before = bytes.clone();
            assert!(canonicalize_nested_metadata(&mut bytes).is_err());
            assert_eq!(bytes, before);
        }
    }

    #[test]
    fn nested_dirty_reprotection_reloads_the_complete_payload_after_the_drain() {
        use std::cell::RefCell;

        let mut bytes = inactive_nested_state(NestedFormat::Vmx);
        bytes.resize(NESTED_HEADER_LEN + 4096, 0x5a);
        let length = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[8..16].copy_from_slice(&0x3000_u64.to_le_bytes());
        let calls = RefCell::new(Vec::new());
        let dirty = drain_dirty_pages_with_nested_reprotection(
            Some((NestedFormat::Vmx, VMX_NESTED_MAX_LEN)),
            |maximum| {
                assert_eq!(maximum, VMX_NESTED_MAX_LEN);
                calls.borrow_mut().push("read");
                Ok(bytes.clone())
            },
            || {
                calls.borrow_mut().push("drain");
                Ok(vec![3, 9])
            },
            |reloaded| {
                calls.borrow_mut().push("reload");
                assert_eq!(reloaded, bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(dirty, [3, 9]);
        assert_eq!(*calls.borrow(), ["read", "drain", "reload"]);
        assert_eq!(
            drain_dirty_pages_with_nested_reprotection(
                None,
                |_| panic!("ordinary guests cannot read nested state"),
                || Ok(vec![7]),
                |_| panic!("ordinary guests cannot reload nested state"),
            )
            .unwrap(),
            [7]
        );
    }

    #[test]
    fn svm_dirty_reprotection_preserves_gif_and_rejects_wrong_vendor_before_drain() {
        use std::cell::RefCell;
        let bytes = inactive_nested_state(NestedFormat::Svm);
        let calls = RefCell::new(Vec::new());
        let dirty = drain_dirty_pages_with_nested_reprotection(
            Some((NestedFormat::Svm, SVM_NESTED_MAX_LEN)),
            |_| {
                calls.borrow_mut().push("read");
                Ok(bytes.clone())
            },
            || {
                calls.borrow_mut().push("drain");
                Ok(vec![4])
            },
            |state| {
                calls.borrow_mut().push("reload");
                assert_eq!(state, bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(dirty, [4]);
        assert_eq!(*calls.borrow(), ["read", "drain", "reload"]);
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some((NestedFormat::Svm, SVM_NESTED_MAX_LEN)),
                |_| Ok(inactive_nested_state(NestedFormat::Vmx)),
                || panic!("wrong vendor must not drain dirty pages"),
                |_| panic!("wrong vendor must not reload state"),
            )
            .is_err()
        );
    }

    #[test]
    fn nested_dirty_reprotection_rejects_bad_state_before_drain_and_propagates_errors() {
        for offset in [0, 3, 4] {
            let mut bytes = inactive_nested_state(NestedFormat::Vmx);
            bytes[offset] ^= 1;
            assert!(
                drain_dirty_pages_with_nested_reprotection(
                    Some((NestedFormat::Vmx, VMX_NESTED_MAX_LEN)),
                    |_| Ok(bytes),
                    || panic!("invalid nested state cannot clear dirty pages"),
                    |_| panic!("invalid nested state cannot be reloaded"),
                )
                .is_err()
            );
        }
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some((NestedFormat::Vmx, VMX_NESTED_MAX_LEN)),
                |_| Err(BackendError::Internal("read failed")),
                || panic!("failed capture cannot clear dirty pages"),
                |_| panic!("failed capture cannot be reloaded"),
            )
            .is_err()
        );
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some((NestedFormat::Vmx, VMX_NESTED_MAX_LEN)),
                |_| Ok(inactive_nested_state(NestedFormat::Vmx)),
                || Err(BackendError::Internal("drain failed")),
                |_| panic!("failed drain cannot reload nested state"),
            )
            .is_err()
        );
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some((NestedFormat::Vmx, VMX_NESTED_MAX_LEN)),
                |_| Ok(inactive_nested_state(NestedFormat::Vmx)),
                || Ok(vec![1]),
                |_| Err(BackendError::Internal("reload failed")),
            )
            .is_err()
        );
    }
}
