// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal deterministic cpio "newc" writer.
//!
//! The staged container bundle is injected into the guest by appending a
//! second initramfs segment to the stock guest initramfs: the kernel accepts
//! concatenated cpio archives (and concatenated gzip members decompress to
//! their concatenation), and later entries override earlier ones. Entries are
//! written in sorted order with zeroed mtimes so the same bundle always
//! produces the same bytes — the segment participates in the run digest.

use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CpioError {
    #[error("unsupported file type in rootfs: {0} (sockets/devices are not staged)")]
    Unsupported(std::path::PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct Writer {
    out: Vec<u8>,
    ino: u32,
}

impl Writer {
    pub fn new() -> Self {
        Writer {
            out: Vec::new(),
            ino: 1,
        }
    }

    fn header(&mut self, name: &str, mode: u32, filesize: usize) {
        let ino = self.ino;
        self.ino += 1;
        // 070701 magic + 13 fields of 8 hex digits: ino, mode, uid, gid,
        // nlink, mtime, filesize, devmajor, devminor, rdevmajor, rdevminor,
        // namesize, check. uid/gid/mtime pinned to 0 for byte stability.
        let namesize = name.len() + 1;
        let _ = write!(
            self.out,
            "070701{ino:08x}{mode:08x}{:08x}{:08x}{:08x}{:08x}{filesize:08x}{:08x}{:08x}{:08x}{:08x}{namesize:08x}{:08x}",
            0, 0, 1, 0, 0, 0, 0, 0, 0
        );
        self.out.extend_from_slice(name.as_bytes());
        self.out.push(0);
        self.pad4();
    }

    fn pad4(&mut self) {
        while !self.out.len().is_multiple_of(4) {
            self.out.push(0);
        }
    }

    pub fn dir(&mut self, name: &str, mode: u32) {
        self.header(name, 0o040000 | (mode & 0o7777), 0);
    }

    pub fn file(&mut self, name: &str, mode: u32, data: &[u8]) {
        self.header(name, 0o100000 | (mode & 0o7777), data.len());
        self.out.extend_from_slice(data);
        self.pad4();
    }

    pub fn symlink(&mut self, name: &str, target: &[u8]) {
        self.header(name, 0o120000 | 0o777, target.len());
        self.out.extend_from_slice(target);
        self.pad4();
    }

    /// Recursively add `dir`'s contents under archive path `prefix`, in
    /// sorted order.
    pub fn tree(&mut self, dir: &Path, prefix: &str) -> Result<(), CpioError> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let archive_name = format!("{prefix}/{name}");
            let meta = std::fs::symlink_metadata(&path)?;
            let ftype = meta.file_type();
            if ftype.is_symlink() {
                let target = std::fs::read_link(&path)?;
                self.symlink(&archive_name, target.as_os_str().as_encoded_bytes());
            } else if ftype.is_dir() {
                self.dir(&archive_name, meta.permissions().mode());
                self.tree(&path, &archive_name)?;
            } else if ftype.is_file() {
                let data = std::fs::read(&path)?;
                self.file(&archive_name, meta.permissions().mode(), &data);
            } else if ftype.is_fifo() || ftype.is_socket() || meta.rdev() != 0 {
                // Images occasionally carry stray sockets/devices; the guest
                // gets fresh /dev and /run mounts, so skipping is safe.
                continue;
            } else {
                return Err(CpioError::Unsupported(path));
            }
        }
        Ok(())
    }

    /// Close the archive and return its bytes (uncompressed cpio).
    pub fn finish(mut self) -> Vec<u8> {
        self.header("TRAILER!!!", 0, 0);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::Writer;

    /// Same logical contents, same bytes: the segment participates in the
    /// run digest, so the writer must be a pure function of the tree.
    #[test]
    fn identical_input_identical_bytes() {
        let build = || {
            let mut w = Writer::new();
            w.dir("d", 0o755);
            w.file("d/a", 0o644, b"alpha");
            w.symlink("d/l", b"a");
            w.finish()
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn newc_shape() {
        let mut w = Writer::new();
        w.file("x", 0o644, b"1234");
        let bytes = w.finish();
        assert!(bytes.starts_with(b"070701"));
        assert!(bytes.len().is_multiple_of(4));
        assert!(bytes.windows(10).any(|win| win == b"TRAILER!!!"));
    }
}
