// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::{OnceLock, RwLock};

use super::{MachineError, identity};

struct LoaderState {
    approved: Option<identity::LoaderGeneration>,
    poisoned: bool,
    active: usize,
}

static LOADER_STATE: OnceLock<RwLock<LoaderState>> = OnceLock::new();

fn state() -> &'static RwLock<LoaderState> {
    LOADER_STATE.get_or_init(|| {
        RwLock::new(LoaderState {
            approved: None,
            poisoned: false,
            active: 0,
        })
    })
}

fn changed_error() -> MachineError {
    MachineError::Backend(
        "QuickNES native loader changed outside the approved core lifecycle".to_owned(),
    )
}

fn poison() {
    state()
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .poisoned = true;
}

fn lifecycle_matches(
    before: identity::LoaderGeneration,
    after: identity::LoaderGeneration,
    cleanup: bool,
    succeeded: bool,
    active: usize,
) -> bool {
    let Some(adds) = after.adds.checked_sub(before.adds) else {
        return false;
    };
    let Some(removes) = after.removes.checked_sub(before.removes) else {
        return false;
    };
    if !succeeded {
        return adds == 0 && removes == 0 && after.images == before.images;
    }
    if cleanup {
        active > 0
            && adds == 0
            && removes >= 1
            && before
                .images
                .checked_sub(after.images)
                .and_then(|value| u64::try_from(value).ok())
                == Some(removes)
            && (active == 1 || removes == 1)
    } else {
        active < usize::MAX
            && adds >= 1
            && removes == 0
            && after
                .images
                .checked_sub(before.images)
                .and_then(|value| u64::try_from(value).ok())
                == Some(adds)
            && (active == 0 || adds == 1)
    }
}

fn mutation<T>(
    cleanup: bool,
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    let mut guard = state().write().unwrap_or_else(|error| error.into_inner());
    if guard.poisoned {
        return if cleanup {
            operation()
        } else {
            Err(changed_error())
        };
    }
    let before = match identity::loader_generation() {
        Ok(value) => value,
        Err(error) => {
            guard.poisoned = true;
            if cleanup {
                let _ = operation();
            }
            return Err(error);
        }
    };
    if guard.approved.is_some_and(|approved| approved != before) {
        guard.poisoned = true;
        if cleanup {
            let _ = operation();
        }
        return Err(changed_error());
    }
    let result = operation();
    let after = match identity::loader_generation() {
        Ok(value) => value,
        Err(error) => {
            guard.poisoned = true;
            drop(guard);
            return match result {
                Err(original) => Err(MachineError::Backend(format!("{original}; {error}"))),
                Ok(_) => Err(error),
            };
        }
    };
    if !lifecycle_matches(before, after, cleanup, result.is_ok(), guard.active)
        || (cleanup && result.is_err())
    {
        guard.poisoned = true;
        drop(guard);
        return match result {
            Err(error) if cleanup => Err(error),
            Err(error) => Err(MachineError::Backend(format!(
                "{error}; {}",
                changed_error()
            ))),
            _ => Err(changed_error()),
        };
    }
    guard.approved = Some(after);
    if result.is_ok() {
        if cleanup {
            guard.active -= 1;
        } else {
            guard.active += 1;
        }
    }
    drop(guard);
    result
}

pub(super) fn open<T>(
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    mutation(false, operation)
}

pub(super) fn close<T>(
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    mutation(true, operation)
}

pub(super) fn call<T>(
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    call_checked(|_| operation())
}

pub(super) fn call_checked<T>(
    operation: impl FnOnce(&dyn Fn() -> Result<(), MachineError>) -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    let guard = state().read().unwrap_or_else(|error| error.into_inner());
    if guard.poisoned {
        return Err(changed_error());
    }
    let Some(approved) = guard.approved else {
        return Err(MachineError::Backend(
            "QuickNES native core was not registered with the loader guard".to_owned(),
        ));
    };
    let expected = (approved.adds, approved.removes);
    let check = || {
        let current = identity::loader_epoch()?;
        if current == expected {
            Ok(())
        } else {
            Err(changed_error())
        }
    };
    let before = check();
    if let Err(error) = before {
        drop(guard);
        poison();
        return Err(error);
    }
    let result = operation(&check);
    let after = check();
    if let Err(error) = after {
        drop(guard);
        poison();
        return Err(error);
    }
    result
}

pub(super) fn cleanup<T>(
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    let guard = state().read().unwrap_or_else(|error| error.into_inner());
    let approved = guard.approved.map(|value| (value.adds, value.removes));
    let before = identity::loader_epoch();
    let result = operation();
    let after = identity::loader_epoch();
    let changed = guard.poisoned
        || before.as_ref().ok() != approved.as_ref()
        || after.as_ref().ok() != approved.as_ref();
    drop(guard);
    if changed {
        poison();
        return Err(changed_error());
    }
    result
}
