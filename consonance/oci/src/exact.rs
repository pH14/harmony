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
                cursor = found;
            }
            Component::Prefix(_) => return Ok(false),
        }
    }
    Ok(true)
}
