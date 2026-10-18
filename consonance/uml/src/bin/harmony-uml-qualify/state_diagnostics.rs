// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use uml::{Capture, Checkpoint, Checkpoints, Session, SessionError};

pub(super) struct StateDiagnostics {
    root: PathBuf,
}

impl StateDiagnostics {
    pub(super) fn from_environment(fixture: &str) -> io::Result<Option<Self>> {
        std::env::var_os("UML_STATE_DIAGNOSTICS_DIR")
            .map(|root| Self::create(Path::new(&root), fixture))
            .transpose()
    }

    fn create(root: &Path, fixture: &str) -> io::Result<Self> {
        let root = root.join(fixture);
        fs::create_dir(&root)?;
        Ok(Self { root })
    }

    pub(super) fn export(
        &self,
        label: &str,
        checkpoints: &Checkpoints,
        checkpoint: &Checkpoint,
        capture_ms: Option<f64>,
    ) -> Result<(), SessionError> {
        let root = self.root.join(label);
        fs::create_dir(&root)?;
        let started = now();
        let sidecar = checkpoint.sidecar()?;
        fs::write(root.join("sidecar.bin"), &sidecar)?;
        let mut frames = BufWriter::new(File::create_new(root.join("frames.bin"))?);
        let count = checkpoints.full(checkpoint, |pages| -> io::Result<usize> {
            for (gfn, hash, bytes) in pages {
                frames.write_all(&gfn.to_le_bytes())?;
                frames.write_all(&hash[..])?;
                frames.write_all(&bytes[..])?;
            }
            frames.flush()?;
            Ok(pages.len())
        })??;
        let metadata = json!({
            "format": "uml-raw-state-diagnostics-v1",
            "claim": "raw captured state for localization; not canonical whole-state identity",
            "label": label,
            "frame_record_bytes": 8 + 32 + 4096,
            "image_reserved_pages": (256 << 20) / 4096,
            "image_bytes": checkpoint.bytes()?,
            "raw_checkpoint_digest": checkpoint.digest()?,
            "frames": count,
            "events": checkpoint.events().len(),
            "moment": checkpoint.moment(),
            "capture_ms": capture_ms,
            "export_ms": started.elapsed().as_secs_f64() * 1000.0,
        });
        fs::write(
            root.join("metadata.json"),
            serde_json::to_vec(&metadata).map_err(io::Error::other)?,
        )?;
        Ok(())
    }

    pub(super) fn repeats(
        &self,
        label: &str,
        checkpoints: &Checkpoints,
        session: &mut Session,
    ) -> Result<(), SessionError> {
        let events = session.events().to_vec();
        let moment = session.moment();
        for repeat in 1..=3 {
            let started = now();
            let captured = session.capture(Capture::default())?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            self.export(
                &format!("{label}-{repeat}"),
                checkpoints,
                &captured,
                Some(elapsed),
            )?;
            if session.events() != events || session.moment() != moment {
                return Err(SessionError::Protocol(
                    "diagnostic capture changed bridge state".into(),
                ));
            }
        }
        Ok(())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "diagnostic export costs are host measurements outside guest execution"
)]
fn now() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_fixture_directory_is_never_reused() {
        let temporary = tempfile::tempdir().unwrap();
        let first = StateDiagnostics::create(temporary.path(), "values").unwrap();
        fs::write(first.root.join("witness"), b"preserved").unwrap();
        assert!(StateDiagnostics::create(temporary.path(), "values").is_err());
        assert_eq!(fs::read(first.root.join("witness")).unwrap(), b"preserved");
    }
}
