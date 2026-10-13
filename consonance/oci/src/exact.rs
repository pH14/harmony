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
    path_inner(path, &mut 0)
}

fn path_inner(path: &Path, symlink_hops: &mut usize) -> io::Result<bool> {
    let mut cursor = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        std::env::current_dir()?.canonicalize()?
    };
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
                    let target = if target.is_absolute() {
                        target
                    } else {
                        parent.join(target)
                    };
                    if !path_inner(&target, symlink_hops)? {
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

    #[test]
    fn local_path_rejects_case_alias_in_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("layout")).unwrap();
        let alias = dir.path().join("image");
        std::os::unix::fs::symlink("LAYOUT", &alias).unwrap();
        assert!(!path(&alias).unwrap());
    }
}
