// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(not(miri))]
pub fn require_single_arena() {
    match tikv_jemalloc_ctl::opt::narenas::read() {
        Ok(1) => {}
        Ok(count) => {
            eprintln!(
                "error: jemalloc is set to {count} arenas and the search binaries need 1; build with JEMALLOC_SYS_WITH_MALLOC_CONF=narenas:1, which .cargo/config.toml sets"
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("error: reading the jemalloc arena count failed: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(miri)]
pub fn require_single_arena() {}
