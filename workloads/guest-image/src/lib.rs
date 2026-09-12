// SPDX-License-Identifier: AGPL-3.0-or-later

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

impl Owner {
    pub const ROOT: Owner = Owner { uid: 0, gid: 0 };
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

    fn header(&mut self, name: &str, mode: u32, owner: Owner, filesize: usize) {
        let ino = self.ino;
        self.ino += 1;
        let namesize = name.len() + 1;
        let Owner { uid, gid } = owner;
        let _ = write!(
            self.out,
            "070701{ino:08x}{mode:08x}{uid:08x}{gid:08x}{:08x}{:08x}{filesize:08x}{:08x}{:08x}{:08x}{:08x}{namesize:08x}{:08x}",
            1, 0, 0, 0, 0, 0, 0
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
        self.dir_owned(name, mode, Owner::ROOT);
    }

    pub fn file(&mut self, name: &str, mode: u32, data: &[u8]) {
        self.file_owned(name, mode, Owner::ROOT, data);
    }

    pub fn symlink(&mut self, name: &str, target: &[u8]) {
        self.symlink_owned(name, Owner::ROOT, target);
    }

    pub fn dir_owned(&mut self, name: &str, mode: u32, owner: Owner) {
        self.header(name, 0o040000 | (mode & 0o7777), owner, 0);
    }

    pub fn file_owned(&mut self, name: &str, mode: u32, owner: Owner, data: &[u8]) {
        self.header(name, 0o100000 | (mode & 0o7777), owner, data.len());
        self.out.extend_from_slice(data);
        self.pad4();
    }

    pub fn symlink_owned(&mut self, name: &str, owner: Owner, target: &[u8]) {
        self.header(name, 0o120000 | 0o777, owner, target.len());
        self.out.extend_from_slice(target);
        self.pad4();
    }

    pub fn tree(&mut self, dir: &Path, prefix: &str) -> Result<(), CpioError> {
        self.tree_owned(dir, prefix, &|_| Owner::ROOT)
    }

    pub fn tree_owned(
        &mut self,
        dir: &Path,
        prefix: &str,
        owner_of: &dyn Fn(&Path) -> Owner,
    ) -> Result<(), CpioError> {
        self.walk(dir, Path::new(""), prefix, owner_of)
    }

    fn walk(
        &mut self,
        dir: &Path,
        relative: &Path,
        prefix: &str,
        owner_of: &dyn Fn(&Path) -> Owner,
    ) -> Result<(), CpioError> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let relative = relative.join(&name);
            let name = name.to_string_lossy();
            let archive_name = format!("{prefix}/{name}");
            let owner = owner_of(&relative);
            let meta = std::fs::symlink_metadata(&path)?;
            let ftype = meta.file_type();
            if ftype.is_symlink() {
                let target = std::fs::read_link(&path)?;
                self.symlink_owned(&archive_name, owner, target.as_os_str().as_encoded_bytes());
            } else if ftype.is_dir() {
                self.dir_owned(&archive_name, meta.permissions().mode(), owner);
                self.walk(&path, &relative, &archive_name, owner_of)?;
            } else if ftype.is_file() {
                let data = std::fs::read(&path)?;
                self.file_owned(&archive_name, meta.permissions().mode(), owner, &data);
            } else if ftype.is_fifo() || ftype.is_socket() || meta.rdev() != 0 {
                continue;
            } else {
                return Err(CpioError::Unsupported(path));
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.header("TRAILER!!!", 0, Owner::ROOT, 0);
        self.out
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Owner, Writer};

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

    fn parse_entry(bytes: &[u8], at: usize) -> (String, u32, u32, usize, Vec<u8>, usize) {
        let field = |i: usize| {
            let s = std::str::from_utf8(&bytes[at + 6 + 8 * i..at + 6 + 8 * (i + 1)]).unwrap();
            u32::from_str_radix(s, 16).unwrap()
        };
        assert_eq!(&bytes[at..at + 6], b"070701");
        let (ino, mode, filesize, namesize) = (field(0), field(1), field(6), field(11));
        let name_at = at + 110;
        let name = std::str::from_utf8(&bytes[name_at..name_at + namesize as usize - 1])
            .unwrap()
            .to_string();
        assert_eq!(bytes[name_at + namesize as usize - 1], 0);
        let data_at = (name_at + namesize as usize).next_multiple_of(4);
        let data = bytes[data_at..data_at + filesize as usize].to_vec();
        let next = (data_at + filesize as usize).next_multiple_of(4);
        (name, ino, mode, filesize as usize, data, next)
    }

    #[test]
    fn header_fields_parse_back_exactly() {
        let mut w = Writer::new();
        w.dir("d", 0o040755);
        w.file("d/f", 0o100640, b"12345");
        w.symlink("d/l", b"f");
        let bytes = w.finish();

        let (name, ino, mode, filesize, _, next) = parse_entry(&bytes, 0);
        assert_eq!((name.as_str(), ino, mode, filesize), ("d", 1, 0o040755, 0));
        let (name, ino, mode, filesize, data, next) = parse_entry(&bytes, next);
        assert_eq!(
            (name.as_str(), ino, mode, filesize),
            ("d/f", 2, 0o100640, 5)
        );
        assert_eq!(data, b"12345");
        let (name, ino, mode, filesize, data, next) = parse_entry(&bytes, next);
        assert_eq!(
            (name.as_str(), ino, mode, filesize),
            ("d/l", 3, 0o120777, 1)
        );
        assert_eq!(data, b"f");
        let (name, _, _, _, _, next) = parse_entry(&bytes, next);
        assert_eq!(name, "TRAILER!!!");
        assert_eq!(next, bytes.len());
    }

    fn parse_owner(bytes: &[u8], at: usize) -> Owner {
        let field = |i: usize| {
            let s = std::str::from_utf8(&bytes[at + 6 + 8 * i..at + 6 + 8 * (i + 1)]).unwrap();
            u32::from_str_radix(s, 16).unwrap()
        };
        Owner {
            uid: field(2),
            gid: field(3),
        }
    }

    #[test]
    fn entries_record_the_owner_they_were_given() {
        let postgres = Owner { uid: 70, gid: 70 };
        let mut w = Writer::new();
        w.dir_owned("data", 0o700, postgres);
        w.file_owned("data/conf", 0o600, postgres, b"cfg");
        w.symlink_owned("data/link", postgres, b"conf");
        w.file("plain", 0o644, b"x");
        let bytes = w.finish();

        let mut at = 0;
        let mut owners = Vec::new();
        loop {
            let (name, _, _, _, _, next) = parse_entry(&bytes, at);
            if name == "TRAILER!!!" {
                break;
            }
            owners.push((name, parse_owner(&bytes, at)));
            at = next;
        }
        assert_eq!(
            owners,
            [
                ("data".to_string(), postgres),
                ("data/conf".to_string(), postgres),
                ("data/link".to_string(), postgres),
                ("plain".to_string(), Owner::ROOT),
            ]
        );
    }

    #[test]
    fn tree_owned_looks_up_each_entry_by_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("var/lib/data")).unwrap();
        std::fs::write(dir.path().join("var/lib/data/conf"), b"cfg").unwrap();
        std::fs::write(dir.path().join("etc"), b"root-owned").unwrap();
        let postgres = Owner { uid: 70, gid: 70 };
        let owner_of = |path: &std::path::Path| {
            if path.starts_with("var/lib/data") {
                postgres
            } else {
                Owner::ROOT
            }
        };
        let mut w = Writer::new();
        w.tree_owned(dir.path(), "root", &owner_of).unwrap();
        let bytes = w.finish();

        let mut at = 0;
        let mut owners = Vec::new();
        loop {
            let (name, _, _, _, _, next) = parse_entry(&bytes, at);
            if name == "TRAILER!!!" {
                break;
            }
            owners.push((name, parse_owner(&bytes, at)));
            at = next;
        }
        assert_eq!(
            owners,
            [
                ("root/etc".to_string(), Owner::ROOT),
                ("root/var".to_string(), Owner::ROOT),
                ("root/var/lib".to_string(), Owner::ROOT),
                ("root/var/lib/data".to_string(), postgres),
                ("root/var/lib/data/conf".to_string(), postgres),
            ]
        );

        let mut plain = Writer::new();
        plain.tree(dir.path(), "root").unwrap();
        let plain = plain.finish();
        assert_ne!(plain, bytes, "the owner is part of the bytes");
        assert_eq!(parse_owner(&plain, 0), Owner::ROOT);
    }

    #[test]
    fn tree_archives_sorted_recursive_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), b"bee").unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/inner"), b"in").unwrap();
        std::os::unix::fs::symlink("b.txt", dir.path().join("c")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(dir.path().join("fifo"))
                .status()
                .unwrap()
                .success()
        );
        let _listener = match std::os::unix::net::UnixListener::bind(dir.path().join("sock")) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => None,
            Err(error) => panic!("bind socket fixture: {error}"),
        };
        let mut w = Writer::new();
        w.tree(dir.path(), "root").unwrap();
        let bytes = w.finish();

        let mut names = Vec::new();
        let mut at = 0;
        loop {
            let (name, _, _, _, _, next) = parse_entry(&bytes, at);
            if name == "TRAILER!!!" {
                break;
            }
            names.push(name);
            at = next;
        }
        assert_eq!(names, ["root/a", "root/a/inner", "root/b.txt", "root/c"]);
    }
}
