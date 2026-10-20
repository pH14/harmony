// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::admission::ExecutionMode;
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
    prepare_staged(&staged, base, None, ExecutionMode::Vm)
}

pub fn prepare_oci_with_bundle(
    image: &str,
    base: &[u8],
    bundle: Option<&str>,
    mode: ExecutionMode,
) -> Result<Prepared, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    prepare_staged(&staged, base, bundle, mode)
}

fn prepare_staged(
    staged: &StagedImage,
    base: &[u8],
    supplied: Option<&str>,
    mode: ExecutionMode,
) -> Result<Prepared, Box<dyn Error>> {
    prepare_staged_with(staged, base, supplied, mode, || {})
}

fn prepare_staged_with(
    staged: &StagedImage,
    base: &[u8],
    supplied: Option<&str>,
    mode: ExecutionMode,
    after_archive: impl FnOnce(),
) -> Result<Prepared, Box<dyn Error>> {
    if base.is_empty() {
        return Err("fault search requires a guest base image".into());
    }
    let request = if let Some(bundle) = supplied {
        let path = "/etc/harmony/cli.bundle";
        LaunchRequest::new(Vec::new())
            .with_bundle(path)
            .with_external_inputs(vec![oci_support::bundle::ExternalInput::new(
                path,
                bundle.as_bytes().to_vec(),
            )])
    } else {
        LaunchRequest::new(Vec::new()).with_bundle(SUPERVISOR_BUNDLE)
    };
    let execution = oci_support::bundle::prepare(staged, &request)?;
    after_archive();
    let (bundle, vocabulary) = admit_prepared(&execution, supplied, mode)?;
    Ok(Prepared {
        vocabulary,
        bundle,
        initramfs: execution.initramfs(base),
    })
}

pub fn admit_prepared(
    execution: &oci_support::PreparedExecution,
    supplied: Option<&str>,
    mode: ExecutionMode,
) -> Result<(String, FaultVocabulary), Box<dyn Error>> {
    if execution.exposes_kvm() {
        return Err("fault execution cannot expose host KVM".into());
    }
    let inputs = execution.external_inputs();
    let expected = if supplied.is_some() {
        "/etc/harmony/cli.bundle"
    } else {
        SUPERVISOR_BUNDLE
    };
    if execution.execution().bundle.as_deref() != Some(expected)
        || (supplied.is_none() && !inputs.is_empty())
        || supplied.is_some_and(|text| {
            inputs.len() != 1
                || inputs[0].destination != std::path::Path::new(expected)
                || inputs[0].data != text.as_bytes()
        })
    {
        return Err("prepared supervisor bundle differs from its request".into());
    }
    let archive = execution.archived_rootfs()?;
    let image = crate::admission::ArchivedImage::new(&archive);
    let admission = image.inspect()?;
    admission.require_for(mode)?;
    let bundle = match supplied {
        Some(text) => text.to_owned(),
        None => String::from_utf8(
            image
                .file(BUNDLE_PATH)
                .ok_or("the bundle must reside inside the staged image")?
                .to_vec(),
        )?,
    };
    let vocabulary = FaultVocabulary::parse(&bundle)?
        .with_instrumented_events(admission.instrumented_events())
        .with_clock_faults(mode == ExecutionMode::Vm);
    Ok((bundle, vocabulary))
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
    fn admitted_prepared_input_checks_the_actual_supervisor_bundle() {
        let root = tempfile::tempdir().unwrap();
        let staged = image(root.path());
        let launch = LaunchRequest::default().with_bundle(SUPERVISOR_BUNDLE);
        let prepared = oci_support::bundle::prepare(&staged, &launch).unwrap();
        let (bundle, vocabulary) = admit_prepared(&prepared, None, ExecutionMode::Vm).unwrap();
        assert_eq!(bundle, BUNDLE);
        assert_eq!(vocabulary.nodes(), 1);
        assert!(
            admit_prepared(
                &prepared,
                Some("node changed /bin/other\n"),
                ExecutionMode::Vm
            )
            .is_err()
        );
        let changed = oci_support::bundle::prepare(&staged, &launch.with_kvm()).unwrap();
        assert!(
            admit_prepared(&changed, None, ExecutionMode::Vm)
                .unwrap_err()
                .to_string()
                .contains("host KVM")
        );
    }

    #[test]
    fn preparation_uses_the_canonical_supervisor_and_keeps_the_bundle_alphabet() {
        let root = tempfile::tempdir().unwrap();
        let first = prepare_staged(&image(root.path()), b"base", None, ExecutionMode::Vm).unwrap();
        let repeated =
            prepare_staged(&image(root.path()), b"base", None, ExecutionMode::Vm).unwrap();
        assert_eq!(first.vocabulary.nodes(), 1);
        assert_eq!(first.vocabulary.hooks(), [1, 2]);
        assert_eq!(first.bundle, BUNDLE);
        assert_eq!(first.initramfs, repeated.initramfs);
        assert!(first.initramfs.starts_with(b"base"));
    }

    #[test]
    fn preparation_marks_the_existing_bundle_for_structured_supervision() {
        let root = tempfile::tempdir().unwrap();
        let prepared =
            prepare_staged(&image(root.path()), b"base", None, ExecutionMode::Vm).unwrap();
        let request = LaunchRequest::new(Vec::new()).with_bundle(SUPERVISOR_BUNDLE);
        let execution = oci_support::bundle::prepare(&image(root.path()), &request).unwrap();
        assert_eq!(
            execution.execution().bundle.as_deref(),
            Some(SUPERVISOR_BUNDLE)
        );
        assert_eq!(
            prepared.vocabulary.identifier(),
            "faultlab_bundle_v10;nodes=1;hooks=1,2;disks=;events=none;clock=time_namespace"
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
        let prepared = prepare_staged(&staged, b"base", None, ExecutionMode::Vm).unwrap();
        assert!(prepared.vocabulary.instrumented_events());

        std::fs::remove_file(root.path().join("symbols/service.sym.tsv")).unwrap();
        assert!(
            !prepare_staged(&staged, b"base", None, ExecutionMode::Vm)
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
            prepare_staged(&staged, b"base", None, ExecutionMode::Vm)
                .err()
                .unwrap()
                .to_string()
                .contains("digest mismatch")
        );
    }

    #[test]
    fn an_empty_base_image_is_rejected_before_control_assembly() {
        let root = tempfile::tempdir().unwrap();
        let error = prepare_staged(&image(root.path()), &[], None, ExecutionMode::Vm)
            .err()
            .unwrap();
        assert!(error.to_string().contains("guest base image"));
    }

    #[cfg(unix)]
    #[test]
    fn preparation_reads_unreadable_staged_files_and_restores_their_modes() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let staged = image(root.path());
        std::fs::write(root.path().join("secret"), b"hidden").unwrap();
        for path in ["secret", BUNDLE_PATH, "etc/harmony"] {
            std::fs::set_permissions(root.path().join(path), std::fs::Permissions::from_mode(0o0))
                .unwrap();
        }
        let prepared = prepare_staged(&staged, b"base", None, ExecutionMode::Vm).unwrap();
        assert_eq!(prepared.bundle, BUNDLE);
        for path in ["secret", "etc/harmony"] {
            let mode = std::fs::symlink_metadata(root.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0, "{path}");
        }
        std::fs::set_permissions(
            root.path().join("etc/harmony"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mode = std::fs::symlink_metadata(root.path().join(BUNDLE_PATH))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_symlink_outside_the_image_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        std::os::unix::fs::symlink(external.path(), root.path().join(BUNDLE_PATH)).unwrap();
        let error = prepare_staged(
            &staged(root.path().to_path_buf()),
            b"base",
            None,
            ExecutionMode::Vm,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("inside the staged image"));
    }

    #[cfg(unix)]
    #[test]
    fn an_in_root_bundle_symlink_uses_the_packed_target() {
        let root = tempfile::tempdir().unwrap();
        let staged = image(root.path());
        std::fs::rename(
            root.path().join(BUNDLE_PATH),
            root.path().join("etc/harmony/document"),
        )
        .unwrap();
        std::os::unix::fs::symlink("document", root.path().join(BUNDLE_PATH)).unwrap();
        let prepared = prepare_staged(&staged, b"base", None, ExecutionMode::Vm).unwrap();
        assert_eq!(prepared.bundle, BUNDLE);
    }

    #[test]
    fn admission_and_bundle_describe_the_exact_archived_rootfs() {
        let root = tempfile::tempdir().unwrap();
        let staged = image(root.path());
        let request = LaunchRequest::new(Vec::new()).with_bundle(SUPERVISOR_BUNDLE);
        let expected = oci_support::bundle::prepare(&staged, &request)
            .unwrap()
            .initramfs(b"base");
        let changed = BUNDLE.replace("node service", "node changed");
        let prepared = prepare_staged_with(&staged, b"base", None, ExecutionMode::Vm, || {
            std::fs::write(root.path().join(BUNDLE_PATH), changed).unwrap();
        })
        .unwrap();
        assert_eq!(prepared.bundle, BUNDLE);
        assert_eq!(prepared.initramfs, expected);
    }
}
