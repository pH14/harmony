// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(crate) fn entry(parent: &Path, name: &OsStr) -> io::Result<Option<PathBuf>> {
    for found in std::fs::read_dir(parent)? {
        let found = found?;
        if found.file_name() == name {
            return Ok(Some(found.path()));
        }
    }
    Ok(None)
}

pub(crate) fn path(path: &Path) -> io::Result<bool> {
    let cursor = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        std::env::current_dir()?.canonicalize()?
    };
    path_inner(path, cursor, &mut 0)
}

fn path_inner(path: &Path, mut cursor: PathBuf, symlink_hops: &mut usize) -> io::Result<bool> {
    for component in path.components() {
        match component {
            Component::RootDir => cursor = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                cursor = cursor.canonicalize()?;
                cursor.pop();
            }
            Component::Normal(name) => {
                let parent = cursor.canonicalize()?;
                let Some(found) = entry(&parent, name)? else {
                    return Ok(false);
                };
                if std::fs::symlink_metadata(&found)?.file_type().is_symlink() {
                    *symlink_hops += 1;
                    if *symlink_hops > 40 {
                        return Ok(false);
                    }
                    let target = std::fs::read_link(&found)?;
                    if !path_inner(&target, parent, symlink_hops)? {
                        return Ok(false);
                    }
                }
                cursor = found;
            }
            Component::Prefix(_) => return Ok(false),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::path;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn local_path_rejects_case_alias_in_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("layout")).unwrap();
        let alias = dir.path().join("image");
        std::os::unix::fs::symlink("LAYOUT", &alias).unwrap();
        assert!(!path(&alias).unwrap());
    }

    #[test]
    fn relative_symlink_does_not_require_readable_ancestors() {
        if std::env::var_os("HARMONY_EXACT_PATH_CHILD").is_some() {
            assert!(path(Path::new("layout")).unwrap());
            assert!(path(Path::new("image")).unwrap());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let work = outer.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir(work.join("layout")).unwrap();
        std::os::unix::fs::symlink("layout", work.join("image")).unwrap();
        std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o111)).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exact::tests::relative_symlink_does_not_require_readable_ancestors",
            ])
            .env("HARMONY_EXACT_PATH_CHILD", "1")
            .current_dir(&work)
            .status()
            .unwrap();
        std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(status.success());
    }
}
