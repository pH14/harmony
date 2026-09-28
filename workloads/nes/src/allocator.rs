// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "linux", target_env = "gnu", not(miri)))]
pub fn use_one_malloc_arena() {
    // SAFETY: mallopt only changes glibc's allocator tuning and takes no pointers. It runs first in main, before the search starts any thread, so no other thread is inside malloc.
    let set = unsafe { libc::mallopt(libc::M_ARENA_MAX, 1) };
    if set != 1 {
        eprintln!(
            "error: mallopt(M_ARENA_MAX, 1) failed; the search binaries need glibc limited to one malloc arena"
        );
        std::process::exit(1);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu", not(miri))))]
pub fn use_one_malloc_arena() {}
