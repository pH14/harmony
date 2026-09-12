// SPDX-License-Identifier: AGPL-3.0-or-later

use oci_support::{PreparedExecution, bundle, image::StagedImage};

pub const ROM_PATH: &str = "/game.nes";
pub const AGENT_PATH: &str = "/opt/harmony/play-agent";

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("NES ROM is empty")]
    EmptyRom,
    #[error(transparent)]
    Bundle(#[from] oci_support::bundle::BundleError),
    #[error(transparent)]
    Image(#[from] oci_support::image::ImageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn prepare_oci(image: &StagedImage, rom: &[u8]) -> Result<PreparedExecution, PrepareError> {
    if rom.is_empty() {
        return Err(PrepareError::EmptyRom);
    }
    let command = vec![
        AGENT_PATH.to_owned(),
        "--nes-payload".to_owned(),
        "--rom".to_owned(),
        ROM_PATH.to_owned(),
    ];
    let request = bundle::LaunchRequest::new(command)
        .with_external_inputs(vec![bundle::ExternalInput::new(ROM_PATH, rom.to_vec())]);
    Ok(bundle::prepare(image, &request)?)
}

pub fn stage_and_prepare(image: &str, rom: &[u8]) -> Result<PreparedExecution, PrepareError> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    prepare_oci(&staged, rom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_support::image::{Ownership, RuntimeConfig};

    fn image() -> StagedImage {
        let rootfs = tempfile::tempdir().expect("rootfs");
        std::fs::create_dir_all(rootfs.path().join("opt/harmony")).expect("agent directory");
        std::fs::write(rootfs.path().join("opt/harmony/play-agent"), b"agent").expect("agent");
        let path = rootfs.keep();
        StagedImage {
            rootfs: path,
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        }
    }

    #[test]
    fn prepared_execution_uses_the_generic_supervisor_and_external_rom() {
        let prepared = prepare_oci(&image(), b"rom-bytes").expect("prepare");
        assert_eq!(
            prepared.execution.argv,
            [AGENT_PATH, "--nes-payload", "--rom", ROM_PATH]
        );
        assert_eq!(prepared.execution.uid, 0);
        assert_eq!(prepared.execution.gid, 0);
        assert_eq!(prepared.execution.cwd, "/");
        let mut compressed = tempfile::NamedTempFile::new().expect("compressed segment");
        std::io::Write::write_all(&mut compressed, &prepared.control_segment)
            .expect("write segment");
        let archive = std::process::Command::new("gzip")
            .args(["-dc"])
            .arg(compressed.path())
            .output()
            .expect("gzip");
        assert!(archive.status.success());
        assert!(
            archive
                .stdout
                .windows(b"game.nes".len())
                .any(|window| window == b"game.nes")
        );
        assert!(
            !archive
                .stdout
                .windows(b"/init".len())
                .any(|window| window == b"/init")
        );
    }

    #[test]
    fn rom_bytes_are_part_of_the_prepared_identity() {
        let first = prepare_oci(&image(), b"rom-one").expect("prepare");
        let second = prepare_oci(&image(), b"rom-two").expect("prepare");
        assert_ne!(first.identity, second.identity);
        assert_ne!(first.control_segment, second.control_segment);
    }

    #[test]
    fn repeated_preparation_is_byte_deterministic() {
        let first = prepare_oci(&image(), b"rom").expect("prepare");
        let second = prepare_oci(&image(), b"rom").expect("prepare");
        assert_eq!(first, second);
        assert_eq!(first.initramfs(b"platform"), second.initramfs(b"platform"));
        assert!(std::path::Path::new(ROM_PATH).is_absolute());
    }

    #[test]
    fn empty_rom_is_rejected_before_image_processing() {
        assert!(matches!(
            prepare_oci(&image(), &[]),
            Err(PrepareError::EmptyRom)
        ));
    }
}
