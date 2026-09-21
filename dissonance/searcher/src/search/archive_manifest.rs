// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::campaign::{InputPolicy, Reporting};

pub use super::campaign::WorkloadPolicies;

pub const ARCHIVE_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveManifest {
    pub schema_version: u32,
    pub archive_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_sha256: Option<String>,
    pub workload_identity_sha256: String,
    pub workload_policies: WorkloadPolicies,
}

impl ArchiveManifest {
    #[must_use]
    pub fn build<G>(
        workload: &G,
        run: &G::Run,
        archive_bytes: &[u8],
        snapshot_bytes: Option<&[u8]>,
    ) -> Self
    where
        G: Reporting + InputPolicy,
    {
        Self {
            schema_version: ARCHIVE_MANIFEST_SCHEMA_VERSION,
            archive_sha256: sha256_hex(archive_bytes),
            snapshot_sha256: snapshot_bytes.map(sha256_hex),
            workload_identity_sha256: workload.workload_identity_sha256(),
            workload_policies: workload.policies(run),
        }
    }

    pub fn validate<G>(
        &self,
        workload: &G,
        run: &G::Run,
        archive_bytes: &[u8],
        snapshot_bytes: Option<&[u8]>,
    ) -> Result<(), Box<dyn Error>>
    where
        G: Reporting + InputPolicy,
    {
        if self.schema_version != ARCHIVE_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "archive manifest schema version {} is not supported",
                self.schema_version
            )
            .into());
        }
        validate_digest("archive", &self.archive_sha256, archive_bytes)?;
        match (&self.snapshot_sha256, snapshot_bytes) {
            (Some(expected), Some(bytes)) => validate_digest("snapshot", expected, bytes)?,
            (Some(_), None) => return Err("archive manifest requires its snapshot artifact".into()),
            (None, Some(_)) => {
                return Err("archive manifest does not authorize a snapshot artifact".into());
            }
            (None, None) => {}
        }
        let identity = workload.workload_identity_sha256();
        if self.workload_identity_sha256 != identity {
            return Err("archive manifest workload identity does not match this workload".into());
        }
        let expected_policies = workload.policies(run);
        if self.workload_policies != expected_policies {
            return Err("archive manifest workload policies do not match this workload".into());
        }
        workload
            .resolve_recorded(&self.workload_policies)
            .map_err(|error| {
                format!("archive manifest workload policies are not resolvable: {error}")
            })?;
        Ok(())
    }
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_digest(artifact: &str, expected: &str, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("archive manifest {artifact} SHA-256 is malformed").into());
    }
    let actual = sha256_hex(bytes);
    if expected != actual {
        return Err(format!("archive manifest {artifact} SHA-256 does not match").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{
        archive::ArchiveKey,
        campaign::{ArchiveReportState, CampaignJobResult, CampaignTypes, InputPolicy, Reporting},
        rand::RomuDuoJrRand,
    };
    use serde::{Deserialize, Serialize};
    use std::{cmp::Ordering, collections::BTreeMap, error::Error};

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct TestKey;

    impl ArchiveKey for TestKey {
        type Group = u8;
        type Lineage = ();

        fn groups() -> usize {
            1
        }

        fn group(self, _depth: usize) -> Self::Group {
            0
        }

        fn progress_cmp(_left: Self::Group, _right: Self::Group) -> Ordering {
            Ordering::Equal
        }

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    struct TestWorkload {
        identity: String,
        policies: WorkloadPolicies,
    }

    impl CampaignTypes for TestWorkload {
        type Target = ();
        type Action = ();
        type Key = TestKey;
        type Milestones = ();
        type Progress = ();
        type Snapshot = u8;
        type Observations = ();
        type Evidence = ();
        type ArchiveReport = ();
        type Run = ();
    }

    impl Reporting for TestWorkload {
        fn stream_format(&self) -> &'static str {
            "test-stream-v1"
        }

        fn checkpoint_format(&self) -> &'static str {
            "test-checkpoint-v1"
        }

        fn workload_identity_sha256(&self) -> String {
            self.identity.clone()
        }

        fn action_cost_unit(&self) -> &'static str {
            "test-actions"
        }

        fn execution_work_unit(&self) -> &'static str {
            "test-work"
        }

        fn result_sha256(
            &self,
            _result: &CampaignJobResult<Self>,
        ) -> Result<String, Box<dyn Error>> {
            Ok("result".to_owned())
        }

        fn archive_report(
            &self,
            _evidence: &Self::Evidence,
            _state: ArchiveReportState<Self>,
        ) -> Self::ArchiveReport {
        }
    }

    impl InputPolicy for TestWorkload {
        fn max_action_limit(&self) -> usize {
            1
        }

        fn max_action_cost(&self) -> u64 {
            1
        }

        fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
            self.policies.clone()
        }

        fn resolve_recorded(
            &self,
            policies: &WorkloadPolicies,
        ) -> Result<Self::Run, Box<dyn Error>> {
            (policies == &self.policies)
                .then_some(())
                .ok_or_else(|| "test policy mismatch".into())
        }

        fn sample_alphabet(
            &self,
            _run: &Self::Run,
            _rand: &mut RomuDuoJrRand,
        ) -> Result<Self::Action, Box<dyn Error>> {
            Ok(())
        }
    }

    fn workload() -> TestWorkload {
        TestWorkload {
            identity: "program-backend-origin".to_owned(),
            policies: BTreeMap::from([
                ("execution_backend".to_owned(), "core-a".to_owned()),
                ("terminal_policy".to_owned(), "halted".to_owned()),
            ]),
        }
    }

    #[test]
    fn manifest_round_trips_and_validates_artifacts_identity_and_policies() {
        let workload = workload();
        let manifest = ArchiveManifest::build(&workload, &(), b"archive", Some(b"snapshot"));
        let bytes = serde_json::to_vec(&manifest).expect("manifest JSON");
        let decoded: ArchiveManifest = serde_json::from_slice(&bytes).expect("manifest decode");
        assert_eq!(decoded, manifest);
        decoded
            .validate(&workload, &(), b"archive", Some(b"snapshot"))
            .expect("valid manifest");
    }

    #[test]
    fn manifest_rejects_wrong_identity_or_backend_policy() {
        let source = workload();
        let manifest = ArchiveManifest::build(&source, &(), b"archive", None);

        let mut wrong_identity = workload();
        wrong_identity.identity = "different-program".to_owned();
        assert!(
            manifest
                .validate(&wrong_identity, &(), b"archive", None)
                .is_err()
        );

        let mut wrong_backend = workload();
        wrong_backend
            .policies
            .insert("execution_backend".to_owned(), "core-b".to_owned());
        assert!(
            manifest
                .validate(&wrong_backend, &(), b"archive", None)
                .is_err()
        );
    }

    #[test]
    fn manifest_rejects_archive_and_snapshot_hash_mismatches_and_presence_errors() {
        let workload = workload();
        let with_snapshot = ArchiveManifest::build(&workload, &(), b"archive", Some(b"snapshot"));
        assert!(
            with_snapshot
                .validate(&workload, &(), b"changed", Some(b"snapshot"))
                .is_err()
        );
        assert!(
            with_snapshot
                .validate(&workload, &(), b"archive", Some(b"changed"))
                .is_err()
        );
        assert!(
            with_snapshot
                .validate(&workload, &(), b"archive", None)
                .is_err()
        );

        let without_snapshot = ArchiveManifest::build(&workload, &(), b"archive", None);
        assert!(
            without_snapshot
                .validate(&workload, &(), b"archive", Some(b"snapshot"))
                .is_err()
        );
    }

    #[test]
    fn manifest_rejects_unknown_schema_and_malformed_hashes() {
        let workload = workload();
        let mut manifest = ArchiveManifest::build(&workload, &(), b"archive", None);
        manifest.schema_version += 1;
        assert!(manifest.validate(&workload, &(), b"archive", None).is_err());

        let mut manifest = ArchiveManifest::build(&workload, &(), b"archive", None);
        manifest.archive_sha256 = "not-a-digest".to_owned();
        assert!(manifest.validate(&workload, &(), b"archive", None).is_err());
    }
}
