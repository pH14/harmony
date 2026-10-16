// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeSet,
    ffi::{CStr, c_void},
    sync::{Mutex, OnceLock},
};

use crate::MachineError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LoaderGeneration {
    pub(super) adds: u64,
    pub(super) removes: u64,
    pub(super) images: usize,
}

static CORE_BASES: OnceLock<Mutex<BTreeSet<usize>>> = OnceLock::new();

pub(super) fn core_bases() -> &'static Mutex<BTreeSet<usize>> {
    CORE_BASES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn unavailable() -> MachineError {
    MachineError::Backend("native QuickNES requires the glibc loader ABI".to_owned())
}

pub(super) fn validate_core_owner(
    _symbol: *mut c_void,
    _loader_path: &CStr,
) -> Result<usize, MachineError> {
    Err(unavailable())
}

pub(super) fn fingerprint(
    _core_sha256: &str,
    _core_base: usize,
    _bindings: &[(&str, usize)],
) -> Result<String, MachineError> {
    Err(unavailable())
}

pub(super) fn loader_generation() -> Result<LoaderGeneration, MachineError> {
    Err(unavailable())
}

pub(super) fn loader_epoch() -> Result<(u64, u64), MachineError> {
    Err(unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_loader_is_unavailable_on_musl() {
        assert!(loader_generation().is_err());
        assert!(loader_epoch().is_err());
        let attempted = crate::quicknes::loader_guard::open::<()>(|| {
            panic!("a musl host must refuse before native library loading")
        });
        assert!(attempted.is_err());
    }
}
