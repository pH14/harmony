// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_ACCOUNT_FILE_BYTES: u64 = 1 << 20;
const PASSWD: &str = "etc/passwd";
const GROUP: &str = "etc/group";

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("image User value is invalid: {0}")]
    InvalidUser(String),
    #[error("rootfs is not a directory: {0}")]
    InvalidRootfs(PathBuf),
    #[error("cannot access staged rootfs {path}: {source}")]
    Rootfs { path: PathBuf, source: io::Error },
    #[error("account path escapes the staged rootfs: {0}")]
    UnsafePath(PathBuf),
    #[error("cannot read account file {path}: {source}")]
    AccountFile { path: PathBuf, source: io::Error },
    #[error("required account file is missing: {0}")]
    MissingAccountFile(PathBuf),
    #[error("account path is not a regular file: {0}")]
    AccountFileType(PathBuf),
    #[error("account file exceeds {limit} bytes: {path}")]
    AccountFileTooLarge { path: PathBuf, limit: u64 },
    #[error("account file is not valid UTF-8: {0}")]
    AccountFileUtf8(PathBuf),
    #[error("malformed account entry in {path} at line {line}")]
    MalformedAccount { path: PathBuf, line: usize },
    #[error("user {0:?} was not found in the staged /etc/passwd")]
    UserNotFound(String),
    #[error("group {0:?} was not found in the staged /etc/group")]
    GroupNotFound(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ProcessCredentials {
    pub uid: u32,
    pub gid: u32,
    #[serde(default, rename = "additionalGids")]
    pub additional_gids: Vec<u32>,
}

impl ProcessCredentials {
    pub const ROOT: Self = Self {
        uid: 0,
        gid: 0,
        additional_gids: Vec::new(),
    };

    #[must_use]
    pub fn supplementary_gids(&self) -> &[u32] {
        &self.additional_gids
    }
}

#[derive(Clone, Debug)]
struct PasswdEntry {
    name: String,
    uid: u32,
    gid: u32,
}

#[derive(Clone, Debug)]
struct GroupEntry {
    name: String,
    gid: u32,
    members: Vec<String>,
}

enum UserPart<'a> {
    Id(u32),
    Name(&'a str),
}

fn parse_user_part(value: &str) -> Result<UserPart<'_>, CredentialsError> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        let id = value
            .parse()
            .map_err(|_| CredentialsError::InvalidUser(value.to_string()))?;
        Ok(UserPart::Id(id))
    } else {
        Ok(UserPart::Name(value))
    }
}

pub(crate) fn validate_user_spec(user: Option<&str>) -> Result<(), CredentialsError> {
    let Some(user) = user else { return Ok(()) };
    if user.is_empty() {
        return Ok(());
    }
    if user
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(CredentialsError::InvalidUser(user.to_string()));
    }
    let mut parts = user.split(':');
    let user_part = parts.next().unwrap_or_default();
    let group_part = parts.next();
    if parts.next().is_some() || user_part.is_empty() || group_part == Some("") {
        return Err(CredentialsError::InvalidUser(user.to_string()));
    }
    Ok(())
}

pub fn resolve(user: Option<&str>, rootfs: &Path) -> Result<ProcessCredentials, CredentialsError> {
    validate_user_spec(user)?;
    let rootfs = canonical_rootfs(rootfs)?;
    let Some(user) = user.filter(|value| !value.is_empty()) else {
        return Ok(ProcessCredentials::ROOT);
    };

    let mut parts = user.split(':');
    let user_part = parse_user_part(parts.next().unwrap_or_default())?;
    let group_part = parts.next().map(parse_user_part).transpose()?;

    let passwd = match (&user_part, &group_part) {
        (UserPart::Name(_), _) | (UserPart::Id(_), None) => read_passwd(&rootfs, false)?,
        (UserPart::Id(_), Some(_)) => None,
    };
    let (uid, primary_gid, passwd_name) = match user_part {
        UserPart::Id(uid) => {
            let entry = passwd
                .as_ref()
                .and_then(|entries| entries.iter().find(|entry| entry.uid == uid));
            match entry {
                Some(entry) => (uid, entry.gid, Some(entry.name.as_str())),
                None => (uid, 0, None),
            }
        }
        UserPart::Name(name) => {
            let entry = passwd
                .as_ref()
                .and_then(|entries| entries.iter().find(|entry| entry.name == name))
                .ok_or_else(|| CredentialsError::UserNotFound(name.to_string()))?;
            (entry.uid, entry.gid, Some(entry.name.as_str()))
        }
    };

    let (gid, additional_gids) = match group_part {
        Some(UserPart::Id(gid)) => (gid, Vec::new()),
        Some(UserPart::Name(name)) => {
            let groups = read_group(&rootfs, true)?.unwrap_or_default();
            let gid = groups
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.gid)
                .ok_or_else(|| CredentialsError::GroupNotFound(name.to_string()))?;
            (gid, Vec::new())
        }
        None => {
            let additional_gids = match passwd_name {
                Some(name) => supplementary_gids(
                    read_group(&rootfs, false)?.as_deref().unwrap_or_default(),
                    name,
                    primary_gid,
                ),
                None => Vec::new(),
            };
            (primary_gid, additional_gids)
        }
    };

    Ok(ProcessCredentials {
        uid,
        gid,
        additional_gids,
    })
}

fn canonical_rootfs(rootfs: &Path) -> Result<PathBuf, CredentialsError> {
    let rootfs = rootfs
        .canonicalize()
        .map_err(|source| CredentialsError::Rootfs {
            path: rootfs.to_path_buf(),
            source,
        })?;
    if !rootfs.is_dir() {
        return Err(CredentialsError::InvalidRootfs(rootfs));
    }
    Ok(rootfs)
}

fn resolve_deepest(rootfs: &Path, relative: &Path) -> Result<PathBuf, CredentialsError> {
    let mut missing = Vec::new();
    let mut cursor = rootfs.join(relative);
    loop {
        match cursor.canonicalize() {
            Ok(mut resolved) => {
                for part in missing.iter().rev() {
                    resolved.push(part);
                }
                if !resolved.starts_with(rootfs) {
                    return Err(CredentialsError::UnsafePath(relative.to_path_buf()));
                }
                return Ok(resolved);
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return Err(CredentialsError::AccountFile {
                        path: cursor,
                        source,
                    });
                };
                missing.push(name);
                if !cursor.pop() {
                    return Err(CredentialsError::AccountFile {
                        path: cursor,
                        source,
                    });
                }
            }
            Err(source) => {
                return Err(CredentialsError::AccountFile {
                    path: cursor,
                    source,
                });
            }
        }
    }
}

fn read_account_file(
    rootfs: &Path,
    relative: &str,
    required: bool,
) -> Result<Option<(PathBuf, Vec<u8>)>, CredentialsError> {
    let path = resolve_deepest(rootfs, Path::new(relative))?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) => {
            if source.kind() == io::ErrorKind::NotFound {
                if required {
                    return Err(CredentialsError::MissingAccountFile(path));
                }
                return Ok(None);
            }
            return Err(CredentialsError::AccountFile { path, source });
        }
    };
    if !metadata.is_file() {
        return Err(CredentialsError::AccountFileType(path));
    }
    let file = File::open(&path).map_err(|source| CredentialsError::AccountFile {
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_ACCOUNT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| CredentialsError::AccountFile {
            path: path.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_ACCOUNT_FILE_BYTES {
        return Err(CredentialsError::AccountFileTooLarge {
            path,
            limit: MAX_ACCOUNT_FILE_BYTES,
        });
    }
    Ok(Some((path, bytes)))
}

fn read_passwd(
    rootfs: &Path,
    required: bool,
) -> Result<Option<Vec<PasswdEntry>>, CredentialsError> {
    let Some((path, bytes)) = read_account_file(rootfs, PASSWD, required)? else {
        return Ok(None);
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| CredentialsError::AccountFileUtf8(path.clone()))?;
    let mut entries = Vec::new();
    for (line, record) in text.lines().enumerate() {
        let line = line + 1;
        let record = record.strip_suffix('\r').unwrap_or(record);
        if record.is_empty() || record.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = record.split(':').collect();
        if !(4..=7).contains(&fields.len())
            || !valid_name(fields[0])
            || fields.iter().any(|field| field.contains('\0'))
        {
            return Err(CredentialsError::MalformedAccount {
                path: path.clone(),
                line,
            });
        }
        let uid = parse_id(fields[2]).ok_or_else(|| CredentialsError::MalformedAccount {
            path: path.clone(),
            line,
        })?;
        let gid = parse_id(fields[3]).ok_or_else(|| CredentialsError::MalformedAccount {
            path: path.clone(),
            line,
        })?;
        entries.push(PasswdEntry {
            name: fields[0].to_string(),
            uid,
            gid,
        });
    }
    Ok(Some(entries))
}

fn read_group(rootfs: &Path, required: bool) -> Result<Option<Vec<GroupEntry>>, CredentialsError> {
    let Some((path, bytes)) = read_account_file(rootfs, GROUP, required)? else {
        return Ok(None);
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| CredentialsError::AccountFileUtf8(path.clone()))?;
    let mut entries = Vec::new();
    for (line, record) in text.lines().enumerate() {
        let line = line + 1;
        let record = record.strip_suffix('\r').unwrap_or(record);
        if record.is_empty() || record.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = record.split(':').collect();
        if fields.len() != 4
            || !valid_name(fields[0])
            || fields.iter().any(|field| field.contains('\0'))
        {
            return Err(CredentialsError::MalformedAccount {
                path: path.clone(),
                line,
            });
        }
        let gid = parse_id(fields[2]).ok_or_else(|| CredentialsError::MalformedAccount {
            path: path.clone(),
            line,
        })?;
        let members = fields[3]
            .split(',')
            .filter(|member| !member.is_empty())
            .map(str::to_string)
            .collect();
        entries.push(GroupEntry {
            name: fields[0].to_string(),
            gid,
            members,
        });
    }
    Ok(Some(entries))
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte != 0 && byte != b'\n' && byte != b'\r' && byte != b':')
}

fn parse_id(value: &str) -> Option<u32> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn supplementary_gids(groups: &[GroupEntry], user: &str, primary_gid: u32) -> Vec<u32> {
    let mut gids = BTreeSet::new();
    for group in groups {
        if group.gid != primary_gid && group.members.iter().any(|member| member == user) {
            gids.insert(group.gid);
        }
    }
    gids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rootfs() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join(PASSWD),
            b"root:x:0:0:root:/root:/bin/sh\nworker:x:42:84:Worker:/home/worker:/bin/sh\nother:x:43:85:Other:/home/other:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(GROUP),
            b"root:x:0:root\nprimary:x:84:worker\nshared:x:99:worker\nother:x:100:other\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn named_and_numeric_forms_resolve_against_the_rootfs() {
        let dir = rootfs();
        for (spec, expected) in [
            (
                "worker",
                ProcessCredentials {
                    uid: 42,
                    gid: 84,
                    additional_gids: vec![99],
                },
            ),
            (
                "42",
                ProcessCredentials {
                    uid: 42,
                    gid: 84,
                    additional_gids: vec![99],
                },
            ),
            (
                "worker:shared",
                ProcessCredentials {
                    uid: 42,
                    gid: 99,
                    additional_gids: vec![],
                },
            ),
            (
                "42:99",
                ProcessCredentials {
                    uid: 42,
                    gid: 99,
                    additional_gids: vec![],
                },
            ),
            (
                "42:other",
                ProcessCredentials {
                    uid: 42,
                    gid: 100,
                    additional_gids: vec![],
                },
            ),
            (
                "worker:100",
                ProcessCredentials {
                    uid: 42,
                    gid: 100,
                    additional_gids: vec![],
                },
            ),
        ] {
            assert_eq!(resolve(Some(spec), dir.path()).unwrap(), expected, "{spec}");
        }
    }

    #[test]
    fn explicit_group_suppresses_supplementary_groups() {
        let dir = rootfs();
        let got = resolve(Some("worker:shared"), dir.path()).unwrap();
        assert!(got.supplementary_gids().is_empty());
    }

    #[test]
    fn supplementary_gids_accessor_exposes_resolved_memberships() {
        let dir = rootfs();
        let got = resolve(Some("worker"), dir.path()).unwrap();
        assert_eq!(got.supplementary_gids(), &[99]);
    }

    #[test]
    fn unknown_numeric_user_keeps_the_requested_uid() {
        let dir = tempfile::tempdir().unwrap();
        let got = resolve(Some("777"), dir.path()).unwrap();
        assert_eq!(
            got,
            ProcessCredentials {
                uid: 777,
                gid: 0,
                additional_gids: vec![],
            }
        );
    }

    #[test]
    fn missing_named_accounts_are_errors() {
        let dir = rootfs();
        assert!(matches!(
            resolve(Some("missing"), dir.path()),
            Err(CredentialsError::UserNotFound(_))
        ));
        assert!(matches!(
            resolve(Some("worker:missing"), dir.path()),
            Err(CredentialsError::GroupNotFound(_))
        ));
    }

    #[test]
    fn malformed_user_forms_are_errors() {
        let dir = rootfs();
        for value in [
            ":group",
            "user:",
            "user:group:extra",
            "user\0",
            "user\n",
            "user\r",
            "worker:group\n",
        ] {
            assert!(matches!(
                validate_user_spec(Some(value)),
                Err(CredentialsError::InvalidUser(_))
            ));
            assert!(matches!(
                resolve(Some(value), dir.path()),
                Err(CredentialsError::InvalidUser(_))
            ));
        }
        assert!(matches!(
            resolve(Some("4294967296"), dir.path()),
            Err(CredentialsError::InvalidUser(_))
        ));
    }

    #[test]
    fn omitted_user_does_not_require_account_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve(None, dir.path()).unwrap(), ProcessCredentials::ROOT);
        assert_eq!(
            resolve(Some(""), dir.path()).unwrap(),
            ProcessCredentials::ROOT
        );
    }

    #[test]
    fn missing_group_file_means_no_supplementary_groups() {
        let dir = rootfs();
        std::fs::remove_file(dir.path().join(GROUP)).unwrap();
        let got = resolve(Some("worker"), dir.path()).unwrap();
        assert_eq!(got.uid, 42);
        assert_eq!(got.gid, 84);
        assert!(got.supplementary_gids().is_empty());
    }

    #[test]
    fn required_group_file_missing_is_distinguished_from_optional_group_file() {
        let dir = rootfs();
        std::fs::remove_file(dir.path().join(GROUP)).unwrap();
        assert!(matches!(
            resolve(Some("worker:shared"), dir.path()),
            Err(CredentialsError::MissingAccountFile(_))
        ));
    }

    #[test]
    fn optional_account_path_errors_are_not_treated_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("etc"), b"not a directory").unwrap();
        assert!(matches!(
            resolve(Some("777"), dir.path()),
            Err(CredentialsError::AccountFile { .. })
        ));
    }

    #[test]
    fn account_path_symlink_loops_are_reported_as_account_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::os::unix::fs::symlink("passwd", dir.path().join(PASSWD)).unwrap();
        assert!(matches!(
            resolve(Some("777"), dir.path()),
            Err(CredentialsError::AccountFile { .. })
        ));
    }

    fn passwd_bytes(size: usize) -> Vec<u8> {
        let mut bytes = b"worker:x:42:84:Worker:/home/worker:/bin/sh\n".to_vec();
        bytes.resize(size, b'\n');
        bytes
    }

    #[test]
    fn account_file_at_the_byte_limit_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join(PASSWD),
            passwd_bytes(MAX_ACCOUNT_FILE_BYTES as usize),
        )
        .unwrap();
        assert_eq!(
            resolve(Some("worker"), dir.path()).unwrap(),
            ProcessCredentials {
                uid: 42,
                gid: 84,
                additional_gids: vec![],
            }
        );
    }

    #[test]
    fn account_file_over_the_byte_limit_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join(PASSWD),
            passwd_bytes(MAX_ACCOUNT_FILE_BYTES as usize + 1),
        )
        .unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::AccountFileTooLarge {
                limit: MAX_ACCOUNT_FILE_BYTES,
                ..
            })
        ));
    }

    #[test]
    fn account_paths_cannot_escape_the_rootfs() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("passwd"),
            b"worker:x:42:84:Worker:/home/worker:/bin/sh\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("passwd"), dir.path().join(PASSWD)).unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::UnsafePath(_))
        ));
    }

    #[test]
    fn malformed_account_records_are_errors() {
        let dir = rootfs();
        std::fs::write(dir.path().join(PASSWD), b"worker:x:not-id:84\n").unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::MalformedAccount { .. })
        ));
        std::fs::write(dir.path().join(PASSWD), b"worker:x:42:84:x:x:x\n").unwrap();
        std::fs::write(dir.path().join(GROUP), b"shared:x:not-id:worker\n").unwrap();
        assert!(matches!(
            resolve(Some("worker:shared"), dir.path()),
            Err(CredentialsError::MalformedAccount { .. })
        ));
    }

    #[test]
    fn passwd_blank_and_comment_records_are_ignored() {
        let dir = rootfs();
        std::fs::write(
            dir.path().join(PASSWD),
            b"# generated account file\n\nworker:x:42:84:Worker:/home/worker:/bin/sh\n",
        )
        .unwrap();
        assert_eq!(resolve(Some("worker"), dir.path()).unwrap().uid, 42);
    }

    #[test]
    fn group_blank_and_comment_records_are_ignored() {
        let dir = rootfs();
        std::fs::write(
            dir.path().join(GROUP),
            b"# generated group file\n\nshared:x:99:worker\n",
        )
        .unwrap();
        assert_eq!(
            resolve(Some("worker"), dir.path())
                .unwrap()
                .supplementary_gids(),
            &[99]
        );
    }

    #[test]
    fn malformed_passwd_reports_the_one_based_line_number() {
        let dir = rootfs();
        std::fs::write(
            dir.path().join(PASSWD),
            b"worker:x:42:84:Worker:/home/worker:/bin/sh\nbroken\n",
        )
        .unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::MalformedAccount { line: 2, .. })
        ));
    }

    #[test]
    fn malformed_group_reports_the_one_based_line_number() {
        let dir = rootfs();
        std::fs::write(dir.path().join(GROUP), b"primary:x:84:worker\nbroken\n").unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::MalformedAccount { line: 2, .. })
        ));
    }

    #[test]
    fn empty_account_names_are_malformed() {
        let dir = rootfs();
        std::fs::write(dir.path().join(PASSWD), b":x:42:84\n").unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::MalformedAccount { line: 1, .. })
        ));
        std::fs::write(dir.path().join(PASSWD), b"bad\rname:x:42:84\n").unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::MalformedAccount { line: 1, .. })
        ));
        std::fs::write(
            dir.path().join(PASSWD),
            b"worker:x:42:84:Worker:/home/worker:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(GROUP), b":x:99:worker\n").unwrap();
        assert!(matches!(
            resolve(Some("worker"), dir.path()),
            Err(CredentialsError::MalformedAccount { line: 1, .. })
        ));
    }
}
