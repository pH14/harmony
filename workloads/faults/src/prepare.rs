// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::bundle::FaultVocabulary;
use oci_support::bundle::LaunchRequest;
use oci_support::image::StagedImage;

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
    let vocabulary = FaultVocabulary::parse(&bundle)?;
    let request = LaunchRequest::new(Vec::new()).with_bundle(SUPERVISOR_BUNDLE);
    let execution = oci_support::bundle::prepare(staged, &request)?;
    Ok(Prepared {
        vocabulary,
        bundle,
        initramfs: execution.initramfs(base),
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
        assert_eq!(
            prepared.vocabulary.identifier(),
            "faultlab_bundle_v1;nodes=1;hooks=1,2"
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
