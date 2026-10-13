// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn entry(parent: &Path, name: &OsStr) -> io::Result<Option<PathBuf>> {
    for found in std::fs::read_dir(parent)? {
        let found = found?;
        if found.file_name() == name {
            return Ok(Some(found.path()));
        }
    }
    Ok(None)
}
