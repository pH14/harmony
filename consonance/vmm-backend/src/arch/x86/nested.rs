// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::error::{BackendError, Result};

pub const NESTED_HEADER_LEN: usize = 128;
pub const VMX_NESTED_MAX_LEN: usize = NESTED_HEADER_LEN + 2 * 4096;

fn validate_vmx_nested_shape(bytes: &[u8], maximum: usize) -> Result<()> {
    if bytes.len() < NESTED_HEADER_LEN || bytes.len() > maximum {
        return Err(BackendError::Internal(
            "nested VMX state length outside host capability",
        ));
    }
    let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let format = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    if size != bytes.len() || format != 0 {
        return Err(BackendError::Internal(
            "invalid nested VMX state format or size",
        ));
    }
    Ok(())
}

pub fn validate_vmx_nested_state(bytes: &[u8], maximum: usize) -> Result<()> {
    validate_vmx_nested_shape(bytes, maximum)?;
    if u16::from_le_bytes(bytes[..2].try_into().unwrap()) != 0 {
        return Err(BackendError::Internal(
            "nested VMX snapshot requires L1 outside guest mode",
        ));
    }
    Ok(())
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn nested_probe(maximum: usize) -> Result<Vec<u8>> {
    if !(NESTED_HEADER_LEN..=VMX_NESTED_MAX_LEN).contains(&maximum) {
        return Err(BackendError::Internal(
            "unsupported nested VMX capability size",
        ));
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
    validate_vmx_nested_shape(&bytes, maximum)?;
    Ok(bytes)
}

pub fn inactive_vmx_nested_state() -> Vec<u8> {
    let mut bytes = vec![0; NESTED_HEADER_LEN];
    bytes[4..8].copy_from_slice(&(NESTED_HEADER_LEN as u32).to_le_bytes());
    bytes[8..24].fill(0xff);
    bytes
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn drain_dirty_pages_with_nested_reprotection(
    maximum: Option<usize>,
    read: impl FnOnce(usize) -> Result<Vec<u8>>,
    drain: impl FnOnce() -> Result<Vec<u64>>,
    reload: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<Vec<u64>> {
    let nested = match maximum {
        Some(maximum) => {
            let bytes = read(maximum)?;
            validate_vmx_nested_state(&bytes, maximum)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let inactive = inactive_vmx_nested_state();
        validate_vmx_nested_state(&inactive, VMX_NESTED_MAX_LEN).unwrap();
        let mut live = inactive.clone();
        live[8..16].copy_from_slice(&0x3000_u64.to_le_bytes());
        assert_ne!(live, inactive);
        validate_vmx_nested_state(&live, VMX_NESTED_MAX_LEN).unwrap();
        for (offset, value) in [(0, 1), (0, 2), (0, 4), (0, 8), (2, 1)] {
            let mut invalid = live.clone();
            invalid[offset] = value;
            assert!(validate_vmx_nested_state(&invalid, VMX_NESTED_MAX_LEN).is_err());
        }
    }

    #[test]
    fn nested_dirty_reprotection_reloads_the_complete_payload_after_the_drain() {
        use std::cell::RefCell;

        let mut bytes = inactive_vmx_nested_state();
        bytes.resize(NESTED_HEADER_LEN + 4096, 0x5a);
        let length = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes[8..16].copy_from_slice(&0x3000_u64.to_le_bytes());
        let calls = RefCell::new(Vec::new());
        let dirty = drain_dirty_pages_with_nested_reprotection(
            Some(VMX_NESTED_MAX_LEN),
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
    fn nested_dirty_reprotection_rejects_bad_state_before_drain_and_propagates_errors() {
        for offset in [0, 2, 4] {
            let mut bytes = inactive_vmx_nested_state();
            bytes[offset] ^= 1;
            assert!(
                drain_dirty_pages_with_nested_reprotection(
                    Some(VMX_NESTED_MAX_LEN),
                    |_| Ok(bytes),
                    || panic!("invalid nested state cannot clear dirty pages"),
                    |_| panic!("invalid nested state cannot be reloaded"),
                )
                .is_err()
            );
        }
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some(VMX_NESTED_MAX_LEN),
                |_| Err(BackendError::Internal("read failed")),
                || panic!("failed capture cannot clear dirty pages"),
                |_| panic!("failed capture cannot be reloaded"),
            )
            .is_err()
        );
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some(VMX_NESTED_MAX_LEN),
                |_| Ok(inactive_vmx_nested_state()),
                || Err(BackendError::Internal("drain failed")),
                |_| panic!("failed drain cannot reload nested state"),
            )
            .is_err()
        );
        assert!(
            drain_dirty_pages_with_nested_reprotection(
                Some(VMX_NESTED_MAX_LEN),
                |_| Ok(inactive_vmx_nested_state()),
                || Ok(vec![1]),
                |_| Err(BackendError::Internal("reload failed")),
            )
            .is_err()
        );
    }
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn canonicalize_vmx_exit_info(bytes: &mut [u8]) -> Result<()> {
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
