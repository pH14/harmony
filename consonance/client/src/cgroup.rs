// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

pub fn own_path(cgroup: &str) -> Option<&str> {
    cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| path.trim_start_matches('/'))
}

pub fn tightest(root: &Path, path: &str, value: impl Fn(&Path) -> Option<u64>) -> Option<u64> {
    let mut tightest: Option<u64> = None;
    let mut dir = root.join(path);
    loop {
        if let Some(here) = value(&dir) {
            tightest = Some(tightest.map_or(here, |tightest| tightest.min(here)));
        }
        if dir == root || !dir.pop() || !dir.starts_with(root) {
            break;
        }
    }
    tightest
}

pub fn memory_headroom(dir: &Path, read: &impl Fn(&Path) -> Option<String>) -> Option<u64> {
    let number = |file: &str| read(&dir.join(file))?.trim().parse::<u64>().ok();
    let limit = [number("memory.max"), number("memory.high")]
        .into_iter()
        .flatten()
        .min()?;
    let reclaimable = read(&dir.join("memory.stat")).map_or(0, |stat| reclaimable_file(&stat));
    let used = number("memory.current")?.saturating_sub(reclaimable);
    Some(limit.saturating_sub(used))
}

pub fn reclaimable_file(stat: &str) -> u64 {
    let field = |name: &str| {
        stat.lines()
            .find_map(|line| {
                let (key, value) = line.split_once(' ')?;
                (key == name).then(|| value.trim().parse::<u64>().ok())?
            })
            .unwrap_or(0)
    };
    field("file").saturating_sub(
        field("shmem")
            .saturating_add(field("file_dirty"))
            .saturating_add(field("file_writeback")),
    )
}

pub fn cpu_cores(dir: &Path, read: &impl Fn(&Path) -> Option<String>) -> Option<u64> {
    let text = read(&dir.join("cpu.max"))?;
    let mut fields = text.split_ascii_whitespace();
    let quota: u64 = fields.next()?.parse().ok()?;
    let period: u64 = fields.next()?.parse().ok()?;
    (period > 0).then(|| (quota / period).max(1))
}

#[cfg(target_os = "linux")]
pub fn here(value: impl Fn(&Path) -> Option<u64>) -> Option<u64> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    tightest(Path::new("/sys/fs/cgroup"), own_path(&cgroup)?, value)
}

#[cfg(target_os = "linux")]
pub fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, path::PathBuf};

    fn files(entries: &[(&str, &str)]) -> impl Fn(&Path) -> Option<String> + use<> {
        let files: BTreeMap<PathBuf, String> = entries
            .iter()
            .map(|(path, text)| (PathBuf::from(path), (*text).to_owned()))
            .collect();
        move |path| files.get(path).cloned()
    }

    #[test]
    fn the_unified_hierarchy_line_names_the_cgroup() {
        assert_eq!(
            own_path("0::/system.slice/run-1.scope\n"),
            Some("system.slice/run-1.scope")
        );
        assert_eq!(own_path("1:cpu:/x\n"), None);
    }

    #[test]
    fn the_tightest_memory_limit_on_the_path_bounds_headroom() {
        let read = files(&[
            ("/cg/a/memory.max", "1000\n"),
            ("/cg/a/memory.current", "400\n"),
            ("/cg/a/b/memory.max", "max\n"),
            ("/cg/a/b/memory.current", "300\n"),
            ("/cg/a/b/c/memory.max", "900\n"),
            ("/cg/a/b/c/memory.current", "100\n"),
        ]);
        let root = Path::new("/cg");
        let headroom = |dir: &Path| memory_headroom(dir, &read);
        assert_eq!(tightest(root, "a/b/c", headroom), Some(600));
        assert_eq!(tightest(root, "z", headroom), None);
    }

    #[test]
    fn a_memory_high_below_the_maximum_is_the_limit() {
        let read = files(&[
            ("/cg/a/memory.max", "1000\n"),
            ("/cg/a/memory.high", "700\n"),
            ("/cg/a/memory.current", "200\n"),
            ("/cg/b/memory.max", "max\n"),
            ("/cg/b/memory.high", "max\n"),
            ("/cg/b/memory.current", "200\n"),
        ]);
        assert_eq!(memory_headroom(Path::new("/cg/a"), &read), Some(500));
        assert_eq!(memory_headroom(Path::new("/cg/b"), &read), None);
    }

    #[test]
    fn clean_page_cache_counts_as_free() {
        let stat = "anon 4000\nfile 3000\nkernel 100\nshmem 1000\nfile_mapped 50\n\
                    file_dirty 300\nfile_writeback 200\ninactive_file 900\n";
        assert_eq!(reclaimable_file(stat), 1500);
        assert_eq!(reclaimable_file("anon 5\n"), 0);
        let read = files(&[
            ("/cg/a/memory.max", "10000\n"),
            ("/cg/a/memory.current", "8000\n"),
            ("/cg/a/memory.stat", stat),
        ]);
        assert_eq!(memory_headroom(Path::new("/cg/a"), &read), Some(3500));
    }

    #[test]
    fn a_cpu_quota_allows_whole_cores_and_at_least_one() {
        let read = files(&[
            ("/cg/a/cpu.max", "max 100000\n"),
            ("/cg/a/b/cpu.max", "450000 100000\n"),
            ("/cg/a/b/c/cpu.max", "50000 100000\n"),
        ]);
        let root = Path::new("/cg");
        let cores = |dir: &Path| cpu_cores(dir, &read);
        assert_eq!(tightest(root, "a", cores), None);
        assert_eq!(tightest(root, "a/b", cores), Some(4));
        assert_eq!(tightest(root, "a/b/c", cores), Some(1));
    }
}
