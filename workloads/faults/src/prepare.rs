// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, path::Path};

use crate::bundle::FaultVocabulary;
use oci_support::bundle::LaunchRequest;
use oci_support::image::StagedImage;
use sha2::{Digest, Sha256};

pub const BUNDLE_PATH: &str = "etc/harmony/bundle";
pub const SUPERVISOR_BUNDLE: &str = "/etc/harmony/bundle";

pub struct Prepared {
    pub vocabulary: FaultVocabulary,
    pub bundle: String,
    pub initramfs: Vec<u8>,
}

pub fn prepare_oci(image: &str, base: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    prepare_staged(&staged, base)
}

fn prepare_staged(staged: &StagedImage, base: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    if base.is_empty() {
        return Err("fault search requires a guest base image".into());
    }
    let root = staged.rootfs.canonicalize()?;
    let document = root.join(BUNDLE_PATH).canonicalize()?;
    if !document.starts_with(&root) {
        return Err("the bundle must reside inside the staged image".into());
    }
    let bundle = String::from_utf8(std::fs::read(document)?)?;
    let vocabulary =
        FaultVocabulary::parse(&bundle)?.with_instrumented_events(has_instrumented_events(&root));
    let request = LaunchRequest::new(Vec::new()).with_bundle(SUPERVISOR_BUNDLE);
    let execution = oci_support::bundle::prepare(staged, &request)?;
    Ok(Prepared {
        vocabulary,
        bundle,
        initramfs: execution.initramfs(base),
    })
}

fn rooted_file(root: &Path, relative: &str) -> Option<std::path::PathBuf> {
    let path = root.join(relative).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn has_instrumented_events(root: &Path) -> bool {
    if rooted_file(root, "usr/lib/libvoidstar.so").is_none()
        || !valid_instrumented_event_attestation(root)
    {
        return false;
    }
    let Ok(symbols) = root.join("symbols").canonicalize() else {
        return false;
    };
    if !symbols.starts_with(root) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(symbols) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".sym.tsv"))
            && entry
                .path()
                .canonicalize()
                .is_ok_and(|path| path.starts_with(root))
            && entry
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    })
}

fn valid_instrumented_event_attestation(root: &Path) -> bool {
    let Some(attestation) = rooted_file(root, "symbols/harmony-instrumented-events") else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(attestation) else {
        return false;
    };
    text.lines().any(|line| {
        let Some((expected, image_path)) = line.split_once(char::is_whitespace) else {
            return false;
        };
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        let image_path = image_path.trim().trim_start_matches('/');
        let Ok(executable) = root.join(image_path).canonicalize() else {
            return false;
        };
        if !executable.starts_with(root) {
            return false;
        }
        std::fs::read(executable).is_ok_and(|bytes| {
            format!("{:x}", Sha256::digest(bytes)) == expected.to_ascii_lowercase()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_support::image::{Ownership, RuntimeConfig};
    use std::path::{Path, PathBuf};

    const BUNDLE: &str = "\
setup /bin/sh -c \"mkdir -p /tmp/service\"
node service /usr/bin/service --data-dir /tmp/service
hook 1 /hooks/compact
hook 2 /hooks/defrag
ready /usr/bin/servicectl endpoint health
";

    fn staged(root: PathBuf) -> StagedImage {
        StagedImage {
            rootfs: root,
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        }
    }

    fn image(root: &Path) -> StagedImage {
        std::fs::create_dir_all(root.join("etc/harmony")).unwrap();
        std::fs::write(root.join(BUNDLE_PATH), BUNDLE).unwrap();
        staged(root.to_path_buf())
    }

    #[test]
    fn preparation_uses_the_canonical_supervisor_and_keeps_the_bundle_alphabet() {
        let root = tempfile::tempdir().unwrap();
        let first = prepare_staged(&image(root.path()), b"base").unwrap();
        let repeated = prepare_staged(&image(root.path()), b"base").unwrap();
        assert_eq!(first.vocabulary.nodes(), 1);
        assert_eq!(first.vocabulary.hooks(), [1, 2]);
        assert_eq!(first.bundle, BUNDLE);
        assert_eq!(first.initramfs, repeated.initramfs);
        assert!(first.initramfs.starts_with(b"base"));
    }

    #[test]
    fn preparation_marks_the_existing_bundle_for_structured_supervision() {
        let root = tempfile::tempdir().unwrap();
        let prepared = prepare_staged(&image(root.path()), b"base").unwrap();
        let request = LaunchRequest::new(Vec::new()).with_bundle(SUPERVISOR_BUNDLE);
        let execution = oci_support::bundle::prepare(&image(root.path()), &request).unwrap();
        assert_eq!(
            execution.execution.bundle.as_deref(),
            Some(SUPERVISOR_BUNDLE)
        );
        let interrupts = if prepared.vocabulary.interrupt_injection() {
            "enabled"
        } else {
            "none"
        };
        assert_eq!(
            prepared.vocabulary.identifier(),
            format!("faultlab_bundle_v4;nodes=1;hooks=1,2;events=none;interrupts={interrupts}")
        );
    }

    #[test]
    fn preparation_derives_instrumented_events_from_runtime_and_symbols() {
        let root = tempfile::tempdir().unwrap();
        let staged = image(root.path());
        std::fs::create_dir_all(root.path().join("usr/lib")).unwrap();
        std::fs::create_dir_all(root.path().join("symbols")).unwrap();
        std::fs::create_dir_all(root.path().join("opt/service")).unwrap();
        std::fs::write(root.path().join("usr/lib/libvoidstar.so"), b"runtime").unwrap();
        std::fs::write(
            root.path().join("symbols/service.sym.tsv"),
            b"1\tservice.go:1\n",
        )
        .unwrap();
        std::fs::write(root.path().join("opt/service/node"), b"instrumented node").unwrap();
        std::fs::write(
            root.path().join("symbols/harmony-instrumented-events"),
            b"f05c4a6fcf5bba49af1a80cf4015c096f55a4869c7df89cb85fa8737e993c995  /opt/service/node\n",
        )
        .unwrap();
        let prepared = prepare_staged(&staged, b"base").unwrap();
        assert!(prepared.vocabulary.instrumented_events());

        std::fs::remove_file(root.path().join("symbols/service.sym.tsv")).unwrap();
        assert!(
            !prepare_staged(&staged, b"base")
                .unwrap()
                .vocabulary
                .instrumented_events()
        );

        std::fs::write(
            root.path().join("symbols/service.sym.tsv"),
            b"1\tservice.go:1\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("symbols/harmony-instrumented-events"),
            b"0000000000000000000000000000000000000000000000000000000000000000  /opt/service/node\n",
        )
        .unwrap();
        assert!(
            !prepare_staged(&staged, b"base")
                .unwrap()
                .vocabulary
                .instrumented_events()
        );
    }

    #[test]
    fn an_empty_base_image_is_rejected_before_control_assembly() {
        let root = tempfile::tempdir().unwrap();
        let error = prepare_staged(&image(root.path()), &[]).err().unwrap();
        assert!(error.to_string().contains("guest base image"));
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_symlink_outside_the_image_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        std::os::unix::fs::symlink(external.path(), root.path().join(BUNDLE_PATH)).unwrap();
        let error = prepare_staged(&staged(root.path().to_path_buf()), b"base")
            .err()
            .unwrap();
        assert!(error.to_string().contains("inside the staged image"));
    }
}
