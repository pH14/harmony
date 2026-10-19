// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(any(test, all(target_os = "linux", target_env = "gnu", not(miri))))]
fn arena_limit(parallelism: usize, override_value: Option<&str>) -> Result<i32, String> {
    match override_value {
        Some(value) => value
            .parse::<i32>()
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or_else(|| "HARMONY_MALLOC_ARENAS must be a positive 32-bit integer".to_owned()),
        None => Ok(i32::try_from(parallelism.clamp(1, 8)).unwrap()),
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu", not(miri)))]
pub fn configure_malloc_arenas() {
    let parallelism = std::thread::available_parallelism().map_or(1, usize::from);
    let override_value = std::env::var("HARMONY_MALLOC_ARENAS");
    let limit = match override_value {
        Ok(value) => arena_limit(parallelism, Some(&value)),
        Err(std::env::VarError::NotPresent) => arena_limit(parallelism, None),
        Err(error) => Err(format!("HARMONY_MALLOC_ARENAS is invalid: {error}")),
    }
    .unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    });
    // SAFETY: mallopt only changes glibc's allocator tuning and takes no pointers. It runs first in main, before the search starts any thread, and limit is a positive c_int.
    let set = unsafe { libc::mallopt(libc::M_ARENA_MAX, limit) };
    if set != 1 {
        eprintln!("error: mallopt(M_ARENA_MAX, {limit}) failed");
        std::process::exit(1);
    }
    eprintln!("malloc arena limit: {limit}");
}

#[cfg(not(all(target_os = "linux", target_env = "gnu", not(miri))))]
pub fn configure_malloc_arenas() {}

#[cfg(test)]
mod tests {
    use super::arena_limit;

    #[test]
    fn automatic_arenas_are_bounded_and_overrides_are_checked() {
        for (cores, expected) in [(0, 1), (1, 1), (4, 4), (8, 8), (128, 8), (usize::MAX, 8)] {
            assert_eq!(arena_limit(cores, None), Ok(expected));
        }
        assert_eq!(arena_limit(128, Some("1")), Ok(1));
        assert_eq!(arena_limit(1, Some("16")), Ok(16));
        for invalid in ["0", "-1", "", "not-a-number", "2147483648"] {
            assert!(arena_limit(128, Some(invalid)).is_err());
        }
    }
}
