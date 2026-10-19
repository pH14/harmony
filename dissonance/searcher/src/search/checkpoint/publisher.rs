// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{
    CheckpointHeader, CheckpointPlan, CheckpointSchedule, CheckpointStoreWriter,
    image::{ImageBudget, ImageLimit, LimitedBytes},
};
use crate::search::snapshot_codec::CheckpointSnapshot;
use std::{error::Error, fmt, io::Write, mem::size_of, sync::Arc, thread::JoinHandle};

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestWriteHook(pub(crate) Arc<dyn Fn(u64) + Send + Sync>);
#[cfg(test)]
impl fmt::Debug for TestWriteHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TestWriteHook")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdmissionVersion {
    pub(crate) executions: u64,
    pub(crate) reserved: u64,
    pub(crate) admitted: usize,
    pub(crate) planned_clock: u64,
}

struct CheckpointImage<S> {
    header: CheckpointHeader,
    body: Vec<u8>,
    snapshots: Vec<(u64, S)>,
    budget: ImageBudget,
    ordinal: u64,
}

#[derive(Debug)]
pub(crate) struct PersistenceFailure {
    message: String,
}
impl fmt::Display for PersistenceFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "checkpoint persistence failed: {}", self.message)
    }
}
impl Error for PersistenceFailure {}

#[derive(Debug)]
pub(crate) struct CampaignPersistenceFailure {
    primary: Box<dyn Error>,
    persistence: PersistenceFailure,
}
impl fmt::Display for CampaignPersistenceFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}; checkpoint finalization also failed: {}",
            self.primary, self.persistence
        )
    }
}
impl Error for CampaignPersistenceFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

type WorkerResult = (CheckpointStoreWriter, Result<(), PersistenceFailure>);

pub(crate) struct CheckpointPublisher<S, P> {
    schedule: CheckpointSchedule<P>,
    template: CheckpointHeader,
    writer: Option<CheckpointStoreWriter>,
    worker: Option<JoinHandle<WorkerResult>>,
    keeper: Option<Arc<CheckpointImage<S>>>,
    ordinal: u64,
    #[cfg(test)]
    before_write: Option<Arc<dyn Fn(u64) + Send + Sync>>,
}

fn failure(error: impl fmt::Display) -> PersistenceFailure {
    PersistenceFailure {
        message: error.to_string(),
    }
}

fn header_charge(
    header: &CheckpointHeader,
    budget: &mut ImageBudget,
) -> Result<(), Box<dyn Error>> {
    for text in [
        &header.format,
        &header.reason,
        &header.workload_identity_sha256,
        &header.executable_identity,
    ] {
        budget.array::<u8>(text.capacity())?;
    }
    if let Some(owner) = &header.checkpoint_owner {
        budget.array::<u8>(owner.capacity())?;
    }
    for (key, value) in &header.policies {
        budget.map_entry::<String, String>()?;
        budget.array::<u8>(key.capacity())?;
        budget.array::<u8>(value.capacity())?;
    }
    Ok(())
}

impl<S: CheckpointSnapshot + Send + Sync + 'static, P: Copy + Ord> CheckpointPublisher<S, P> {
    pub(crate) fn create(
        plan: CheckpointPlan,
        template: CheckpointHeader,
        marks: usize,
        top: Option<P>,
    ) -> Result<Self, Box<dyn Error>> {
        if cfg!(target_arch = "wasm32") {
            return Err("file checkpoint publication is unavailable on wasm32".into());
        }
        let writer = CheckpointStoreWriter::create(plan.clone())?;
        #[cfg(test)]
        let before_write = plan.before_write.as_ref().map(|hook| hook.0.clone());
        Ok(Self {
            schedule: CheckpointSchedule::new(plan, marks, top),
            template,
            writer: Some(writer),
            worker: None,
            keeper: None,
            ordinal: 0,
            #[cfg(test)]
            before_write,
        })
    }
    pub(crate) fn due(
        &mut self,
        executions: u64,
        marks: usize,
        top: Option<P>,
    ) -> Option<&'static str> {
        self.schedule.due(executions, marks, top)
    }
    pub(crate) fn poll(&mut self) -> Result<(), Box<dyn Error>> {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.join()?;
        }
        Ok(())
    }
    fn join(&mut self) -> Result<(), PersistenceFailure> {
        if let Some(worker) = self.worker.take() {
            let (writer, result) = worker
                .join()
                .map_err(|_| failure("writer thread panicked"))?;
            self.writer = Some(writer);
            result?;
        }
        Ok(())
    }
    pub(crate) fn finish<T>(
        &mut self,
        primary: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        let result = self.join();
        self.keeper.take();
        match (primary, result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
            (Err(primary), Err(persistence)) => Err(Box::new(CampaignPersistenceFailure {
                primary,
                persistence,
            })),
        }
    }
    pub(crate) fn publish<'a>(
        &mut self,
        version: AdmissionVersion,
        reason: &str,
        snapshots: impl Iterator<Item = (u64, &'a S)> + Clone,
        body: impl Fn(&mut dyn Write) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>>
    where
        S: 'a,
    {
        self.join()?;
        self.keeper.take();
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or("checkpoint publication ordinal overflow")?;
        let mut budget = ImageBudget::new(self.schedule.plan.max_image_bytes);
        let snapshot_count = snapshots.clone().count();
        let capture = (|| -> Result<_, Box<dyn Error>> {
            budget.reserve(size_of::<CheckpointImage<S>>() + 2 * size_of::<usize>())?;
            header_charge(&self.template, &mut budget)?;
            budget.array::<u8>(reason.len())?;
            budget.array::<(u64, S)>(snapshot_count)?;
            for (_, snapshot) in snapshots.clone() {
                snapshot.checkpoint_image_charge(&mut budget)?;
            }
            let mut bytes = LimitedBytes::new(budget.remaining());
            let result = body(&mut bytes);
            if bytes.exceeded() {
                return Err(ImageLimit.into());
            }
            result?;
            let bytes = bytes.finish();
            budget.reserve(bytes.capacity())?;
            Ok(bytes)
        })();
        let mut header = self.template.clone();
        header.reason = reason.to_owned();
        header.executions = version.executions;
        header.reserved = version.reserved;
        header.admitted = version.admitted;
        header.planned_clock = version.planned_clock;
        let bytes = match capture {
            Ok(bytes) => bytes,
            Err(error) if is_image_limit(error.as_ref()) => {
                drop(budget);
                return self
                    .writer
                    .as_mut()
                    .ok_or("checkpoint writer is unavailable")?
                    .write(&header, snapshots, body);
            }
            Err(error) => return Err(error),
        };
        let mut frozen = Vec::new();
        frozen.try_reserve_exact(snapshot_count)?;
        budget.array::<(u64, S)>(frozen.capacity() - snapshot_count)?;
        for (id, snapshot) in snapshots {
            if frozen.len() == snapshot_count {
                return Err("checkpoint snapshot iterator grew during capture".into());
            }
            frozen.push((id, snapshot.freeze_checkpoint()?));
        }
        if frozen.len() != snapshot_count {
            return Err("checkpoint snapshot iterator shrank during capture".into());
        }
        let image = Arc::new(CheckpointImage {
            header,
            body: bytes,
            snapshots: frozen,
            budget,
            ordinal: self.ordinal,
        });
        self.keeper = Some(image.clone());
        let mut writer = self
            .writer
            .take()
            .ok_or("checkpoint writer is unavailable")?;
        #[cfg(test)]
        let before_write = self.before_write.clone();
        self.worker = Some(
            std::thread::Builder::new()
                .name("checkpoint-writer".into())
                .spawn(move || {
                    debug_assert!(image.budget.bytes() > 0);
                    debug_assert_ne!(image.ordinal, 0);
                    #[cfg(test)]
                    if let Some(before_write) = before_write {
                        before_write(image.ordinal);
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        writer
                            .write(
                                &image.header,
                                image.snapshots.iter().map(|(id, snapshot)| (*id, snapshot)),
                                |out| {
                                    out.write_all(&image.body)?;
                                    Ok(())
                                },
                            )
                            .map_err(failure)
                    }))
                    .unwrap_or_else(|_| Err(failure("writer panicked while publishing")));
                    (writer, result)
                })?,
        );
        Ok(())
    }
}

fn is_image_limit(mut error: &(dyn Error + 'static)) -> bool {
    loop {
        if error.is::<ImageLimit>() {
            return true;
        }
        if let Some(io) = error.downcast_ref::<std::io::Error>()
            && io.get_ref().is_some_and(|inner| inner.is::<ImageLimit>())
        {
            return true;
        }
        match error.source() {
            Some(source) => error = source,
            None => return false,
        }
    }
}

impl<S, P> Drop for CheckpointPublisher<S, P> {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.keeper.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::checkpoint::{CheckpointReader, SEARCH_CHECKPOINT_FORMAT};
    use std::{collections::BTreeMap, fs, path::PathBuf, sync::Barrier};

    fn directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "checkpoint-publisher-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        directory
    }
    fn plan(directory: PathBuf, max_image_bytes: usize) -> CheckpointPlan {
        CheckpointPlan {
            directory,
            every: None,
            on_marks: false,
            on_top_progress: false,
            checkpoint_owner: Some("publisher-fixture".into()),
            max_image_bytes,
            before_write: None,
        }
    }
    fn header() -> CheckpointHeader {
        CheckpointHeader {
            format: SEARCH_CHECKPOINT_FORMAT.into(),
            checkpoint_owner: None,
            reason: String::new(),
            workload_identity_sha256: "workload".into(),
            executable_identity: "executable".into(),
            campaign_seed: 7,
            window: 2,
            archive_entry_limit: 100,
            memory_budget_mib: None,
            policies: BTreeMap::new(),
            executions: 0,
            reserved: 0,
            admitted: 0,
            planned_clock: 0,
        }
    }
    fn version(executions: u64) -> AdmissionVersion {
        AdmissionVersion {
            executions,
            reserved: executions + 2,
            admitted: executions as usize,
            planned_clock: executions * 3,
        }
    }
    #[test]
    fn delayed_writer_uses_frozen_body_and_snapshots_without_archive_outer_arc_pins() {
        let asynchronous = directory("immutable");
        let synchronous = directory("immutable-reference");
        let mut publisher = CheckpointPublisher::<Vec<u8>, u8>::create(
            plan(asynchronous.clone(), 1 << 20),
            header(),
            0,
            None,
        )
        .unwrap();
        let entered = Arc::new(Barrier::new(2));
        let released = Arc::new(Barrier::new(2));
        let barriers = (entered.clone(), released.clone());
        publisher.before_write = Some(Arc::new(move |ordinal| {
            if ordinal == 1 {
                barriers.0.wait();
                barriers.1.wait();
            }
        }));
        let mut archived = Arc::new(vec![1, 2, 3]);
        let mut live = (
            vec![4_u64, 5],
            "old evidence".to_owned(),
            BTreeMap::from([(7_u64, 8_u64)]),
        );
        let original = live.clone();
        publisher
            .publish(
                version(1),
                "interval",
                std::iter::once((9, archived.as_ref())),
                |out| {
                    postcard::to_io(&live, out)?;
                    Ok(())
                },
            )
            .unwrap();
        entered.wait();
        assert_eq!(Arc::strong_count(&archived), 1);
        Arc::get_mut(&mut archived).unwrap()[0] = 99;
        live.0.push(6);
        live.1 = "new evidence".into();
        live.2.insert(7, 99);
        let held = Arc::downgrade(publisher.keeper.as_ref().unwrap());
        assert!(publisher.keeper.as_ref().unwrap().budget.bytes() >= 3 + 2 * size_of::<u64>());
        released.wait();
        publisher.join().unwrap();
        assert!(held.upgrade().is_some());
        let mut expected = header();
        let first = version(1);
        expected.reason = "interval".into();
        expected.executions = first.executions;
        expected.reserved = first.reserved;
        expected.admitted = first.admitted;
        expected.planned_clock = first.planned_clock;
        let mut reference =
            CheckpointStoreWriter::create(plan(synchronous.clone(), 1 << 20)).unwrap();
        reference
            .write(&expected, std::iter::once((9, &vec![1_u8, 2, 3])), |out| {
                postcard::to_io(&original, out)?;
                Ok(())
            })
            .unwrap();
        for file in ["000000000001-interval.ckpt", "snapshots.store"] {
            assert_eq!(
                fs::read(asynchronous.join(file)).unwrap(),
                fs::read(synchronous.join(file)).unwrap()
            );
        }
        let mut reader =
            CheckpointReader::open(&asynchronous.join("000000000001-interval.ckpt")).unwrap();
        assert_eq!(
            reader
                .next::<(Vec<u64>, String, BTreeMap<u64, u64>)>()
                .unwrap(),
            original
        );
        assert_eq!(reader.snapshots::<Vec<u8>>().unwrap(), [(9, vec![1, 2, 3])]);
        reader.finish().unwrap();
        publisher
            .publish(
                version(2),
                "final",
                std::iter::once((10, archived.as_ref())),
                |out| {
                    postcard::to_io(&live, out)?;
                    Ok(())
                },
            )
            .unwrap();
        assert!(held.upgrade().is_none());
        publisher.finish(Ok(())).unwrap();
        assert!(publisher.keeper.is_none());
        drop(publisher);
        drop(reference);
        fs::remove_dir_all(asynchronous).unwrap();
        fs::remove_dir_all(synchronous).unwrap();
    }
    #[test]
    fn the_exact_image_byte_limit_is_async_and_one_byte_less_uses_synchronous_pressure() {
        let snapshot = vec![1_u8, 2, 3];
        let body = postcard::to_allocvec(&[4_u8, 5, 6]).unwrap();
        let mut charge = ImageBudget::new(usize::MAX);
        charge
            .reserve(size_of::<CheckpointImage<Vec<u8>>>() + 2 * size_of::<usize>())
            .unwrap();
        header_charge(&header(), &mut charge).unwrap();
        charge.array::<u8>("final".len()).unwrap();
        charge.array::<(u64, Vec<u8>)>(1).unwrap();
        snapshot.checkpoint_image_charge(&mut charge).unwrap();
        charge.reserve(body.len()).unwrap();
        let exact = charge.bytes();
        for (label, limit, asynchronous) in
            [("exact", exact, true), ("below-exact", exact - 1, false)]
        {
            let path = directory(label);
            let mut publisher = CheckpointPublisher::<Vec<u8>, u8>::create(
                plan(path.clone(), limit),
                header(),
                0,
                None,
            )
            .unwrap();
            publisher
                .publish(
                    version(1),
                    "final",
                    std::iter::once((9, &snapshot)),
                    |out| {
                        out.write_all(&body)?;
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(publisher.worker.is_some(), asynchronous);
            assert_eq!(publisher.keeper.is_some(), asynchronous);
            if asynchronous {
                assert_eq!(publisher.keeper.as_ref().unwrap().budget.bytes(), exact);
            }
            publisher.finish(Ok(())).unwrap();
            let mut reader = CheckpointReader::open(&path.join("000000000001-final.ckpt")).unwrap();
            assert_eq!(reader.next::<[u8; 3]>().unwrap(), [4, 5, 6]);
            assert_eq!(
                reader.snapshots::<Vec<u8>>().unwrap(),
                [(9, snapshot.clone())]
            );
            reader.finish().unwrap();
            drop(publisher);
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn large_owned_assertion_values_trigger_pressure_before_freezing_and_preserve_bytes() {
        use crate::search::snapshot_codec::PostcardSnapshotCodec;
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct Snapshot {
            values: BTreeMap<String, (String, String)>,
            #[serde(skip)]
            freezes: Arc<AtomicUsize>,
        }
        impl CheckpointSnapshot for Snapshot {
            type Codec = PostcardSnapshotCodec<Self>;
            fn checkpoint_image_charge(
                &self,
                budget: &mut ImageBudget,
            ) -> Result<(), Box<dyn Error>> {
                budget.reserve(size_of::<Self>())?;
                for (name, (message, location)) in &self.values {
                    budget.map_entry::<String, (String, String)>()?;
                    budget.array::<u8>(name.capacity())?;
                    budget.array::<u8>(message.capacity())?;
                    budget.array::<u8>(location.capacity())?;
                }
                Ok(())
            }
            fn freeze_checkpoint(&self) -> Result<Self, Box<dyn Error>> {
                self.freezes.fetch_add(1, Ordering::SeqCst);
                Ok(self.clone())
            }
        }
        let mut snapshot = Snapshot {
            values: BTreeMap::new(),
            freezes: Arc::default(),
        };
        let mut owned_values = 0;
        for index in 0..8 {
            let mut message = String::with_capacity(65536);
            message.push_str(&"message".repeat(4096));
            let mut location = String::with_capacity(32768);
            location.push_str(&"location".repeat(1024));
            owned_values += message.capacity() + location.capacity();
            snapshot
                .values
                .insert(format!("assertion-{index}"), (message, location));
        }
        let body = postcard::to_allocvec(&42_u64).unwrap();
        let mut budget = ImageBudget::new(usize::MAX);
        budget
            .reserve(size_of::<CheckpointImage<Snapshot>>() + 2 * size_of::<usize>())
            .unwrap();
        header_charge(&header(), &mut budget).unwrap();
        budget.array::<u8>("final".len()).unwrap();
        budget.array::<(u64, Snapshot)>(1).unwrap();
        snapshot.checkpoint_image_charge(&mut budget).unwrap();
        budget.reserve(body.len()).unwrap();
        let exact = budget.bytes();
        let mut reference = None;
        for (label, limit, asynchronous) in [
            ("assertion-exact", exact, true),
            ("assertion-below", exact - 1, false),
            (
                "assertion-values-pressure",
                exact - owned_values + 1024,
                false,
            ),
        ] {
            snapshot.freezes.store(0, Ordering::SeqCst);
            let path = directory(label);
            let mut publisher = CheckpointPublisher::<Snapshot, u8>::create(
                plan(path.clone(), limit),
                header(),
                0,
                None,
            )
            .unwrap();
            if !asynchronous {
                publisher.before_write = Some(Arc::new(|_| {
                    panic!("oversized assertion values reached the worker")
                }));
            }
            publisher
                .publish(
                    version(1),
                    "final",
                    std::iter::once((3, &snapshot)),
                    |out| {
                        out.write_all(&body)?;
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(publisher.worker.is_some(), asynchronous);
            assert_eq!(publisher.keeper.is_some(), asynchronous);
            assert_eq!(
                snapshot.freezes.load(Ordering::SeqCst),
                usize::from(asynchronous)
            );
            publisher.finish(Ok(())).unwrap();
            let file = path.join("000000000001-final.ckpt");
            let bytes = (
                fs::read(&file).unwrap(),
                fs::read(path.join("snapshots.store")).unwrap(),
            );
            if let Some(reference) = &reference {
                assert_eq!(&bytes, reference);
            } else {
                reference = Some(bytes);
            }
            let mut reader = CheckpointReader::open(&file).unwrap();
            assert_eq!(reader.next::<u64>().unwrap(), 42);
            let restored = reader.snapshots::<Snapshot>().unwrap();
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].0, 3);
            assert_eq!(restored[0].1.values, snapshot.values);
            reader.finish().unwrap();
            drop(publisher);
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn an_oversized_snapshot_uses_the_same_codec_synchronously_before_freezing() {
        let path = directory("snapshot-pressure");
        let mut publisher =
            CheckpointPublisher::<Vec<u8>, u8>::create(plan(path.clone(), 4096), header(), 0, None)
                .unwrap();
        publisher.before_write = Some(Arc::new(|_| panic!("oversized image reached the worker")));
        let snapshot = vec![7_u8; 32768];
        publisher
            .publish(
                version(1),
                "final",
                std::iter::once((3, &snapshot)),
                |out| {
                    postcard::to_io(&42_u64, out)?;
                    Ok(())
                },
            )
            .unwrap();
        assert!(publisher.worker.is_none());
        assert!(publisher.keeper.is_none());
        let mut reader = CheckpointReader::open(&path.join("000000000001-final.ckpt")).unwrap();
        assert_eq!(reader.next::<u64>().unwrap(), 42);
        assert_eq!(reader.snapshots::<Vec<u8>>().unwrap(), [(3, snapshot)]);
        reader.finish().unwrap();
        publisher.finish(Ok(())).unwrap();
        drop(publisher);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn oversized_body_pressure_precedes_an_oversized_copy_and_preserves_body_bytes() {
        let path = directory("body-pressure");
        let mut publisher =
            CheckpointPublisher::<u8, u8>::create(plan(path.clone(), 4096), header(), 0, None)
                .unwrap();
        publisher.before_write = Some(Arc::new(|_| panic!("oversized body reached the worker")));
        let body = vec![8_u8; 32768];
        publisher
            .publish(version(1), "final", std::iter::once((3, &7)), |out| {
                postcard::to_io(&body, out)?;
                Ok(())
            })
            .unwrap();
        assert!(publisher.worker.is_none());
        assert!(publisher.keeper.is_none());
        let mut reader = CheckpointReader::open(&path.join("000000000001-final.ckpt")).unwrap();
        assert_eq!(reader.next::<Vec<u8>>().unwrap(), body);
        reader.finish().unwrap();
        publisher.finish(Ok(())).unwrap();
        drop(publisher);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn final_join_refuses_success_and_preserves_a_typed_primary_with_writer_failure() {
        #[derive(Debug)]
        struct Primary;
        impl fmt::Display for Primary {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("primary execution failure")
            }
        }
        impl Error for Primary {}
        for primary in [false, true] {
            let path = directory(if primary {
                "combined-failure"
            } else {
                "final-failure"
            });
            let mut publisher = CheckpointPublisher::<u8, u8>::create(
                plan(path.clone(), 1 << 20),
                header(),
                0,
                None,
            )
            .unwrap();
            let entered = Arc::new(Barrier::new(2));
            let released = Arc::new(Barrier::new(2));
            let barriers = (entered.clone(), released.clone());
            publisher.before_write = Some(Arc::new(move |_| {
                barriers.0.wait();
                barriers.1.wait();
            }));
            publisher
                .publish(version(1), "final", std::iter::once((3, &7)), |out| {
                    postcard::to_io(&1_u64, out)?;
                    Ok(())
                })
                .unwrap();
            entered.wait();
            fs::remove_dir_all(&path).unwrap();
            released.wait();
            let result: Result<(), Box<dyn Error>> =
                if primary { Err(Primary.into()) } else { Ok(()) };
            let error = publisher.finish(result).unwrap_err();
            if primary {
                let combined = error.downcast_ref::<CampaignPersistenceFailure>().unwrap();
                assert!(combined.primary.is::<Primary>());
                assert!(combined.persistence.message.contains("No such file"));
            } else {
                assert!(error.is::<PersistenceFailure>());
            }
            assert!(publisher.worker.is_none());
            assert!(publisher.keeper.is_none());
            assert!(!path.join("000000000001-final.ckpt").exists());
        }
    }
}
