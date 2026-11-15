use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::core::session::{AggregateProgress, FileProgress, UploadState};

// ---------------------------------------------------------------------------
// Per-file upload progress (in-flight tracking during chunk uploads)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UploadProgress {
    /// S3 key — the public identifier returned by uploadFile().
    pub file_key: String,
    pub file_name: String,
    pub file_hash: String,
    pub transfer_id: String,
    pub total_bytes: u64,
    pub total_parts: u32,
    /// Bytes in completed chunks only.
    pub completed_bytes: u64,
    /// part_number → bytes uploaded so far (in-flight, cleared on chunk completion).
    pub in_flight_progress: HashMap<u32, u64>,
    pub completed_parts: u32,
    pub status: UploadState,
    run_version: u64,
    /// Guards against double-counting when stale workers from a previous run
    /// call complete_chunk after a pause+resume starts a new run.
    completed_part_set: HashSet<u32>,
    preserve_paused_progress: bool,
    deferred_failed_parts: HashSet<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::Session;

    #[test]
    fn confirmed_bytes_above_99_percent_remain_exact_until_the_final_acknowledgement() {
        use std::sync::Arc;

        struct RecordingNotifier(
            Arc<Mutex<Vec<(FileProgress, AggregateProgress, AggregateProgress)>>>,
        );
        impl ProgressNotifier for RecordingNotifier {
            fn notify(
                &self,
                file: &FileProgress,
                session: &AggregateProgress,
                transfer: &AggregateProgress,
            ) {
                self.0
                    .lock()
                    .unwrap()
                    .push((file.clone(), session.clone(), transfer.clone()));
            }
        }

        const FIRST_PART: u64 = 32 * 1024 * 1024;
        const FINAL_PART: u64 = 64 * 1024;
        const TOTAL: u64 = FIRST_PART + FINAL_PART;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let manager = ProgressManager::new(RecordingNotifier(seen.clone()));
        let mut session = Session::new();
        session.register_file(
            "key".into(),
            "hash".into(),
            "transfer".into(),
            "file".into(),
            "file".into(),
            TOTAL,
            HashMap::new(),
        );
        let aggregates = |session: &Session| {
            (
                session.get_aggregate_progress(None),
                session.get_aggregate_progress(Some("transfer")),
            )
        };
        let assert_snapshot = |session: &Session, bytes: u64, parts: u32| {
            let (s, t) = aggregates(session);
            let file = manager.get_live_progress(None, None).remove(0);
            let (s, t) = manager.get_live_aggregate(s, t, "transfer");
            let callback = seen.lock().unwrap().last().unwrap().clone();
            let percentage = bytes as f64 / TOTAL as f64 * 100.0;
            for snapshot in [&file, &callback.0] {
                assert_eq!(snapshot.uploaded_bytes, bytes);
                assert_eq!(snapshot.completed_parts, parts);
                assert_eq!(snapshot.percentage, percentage);
            }
            for snapshot in [&s, &t, &callback.1, &callback.2] {
                assert_eq!(snapshot.uploaded_size, bytes);
                assert_eq!(snapshot.percentage, percentage);
            }
        };

        let (s, t) = aggregates(&session);
        manager.init(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            TOTAL,
            2,
            0,
            0,
            HashSet::new(),
            7,
            s.clone(),
            t.clone(),
        );
        manager.update_in_flight_for_run("key", 1, FIRST_PART, Some(7), s.clone(), t.clone());
        manager.update_in_flight_for_run("key", 2, FINAL_PART, Some(7), s, t);
        assert_snapshot(&session, TOTAL - TOTAL.div_ceil(100), 0);

        session.complete_chunk("key", 1, "etag1".into(), FIRST_PART);
        let (s, t) = aggregates(&session);
        manager.complete_chunk("key", 1, FIRST_PART, s.clone(), t.clone());
        assert_snapshot(&session, FIRST_PART, 1);
        for sent in [0, FINAL_PART / 2, FINAL_PART] {
            manager.update_in_flight_for_run("key", 2, sent, Some(7), s.clone(), t.clone());
            assert_snapshot(&session, FIRST_PART, 1);
        }
        manager.reset_failed_part("key", 2, 7, false, s, t);
        assert_snapshot(&session, FIRST_PART, 1);

        session.complete_chunk("key", 2, "etag2".into(), FINAL_PART);
        let (s, t) = aggregates(&session);
        manager.complete_chunk("key", 2, FINAL_PART, s, t);
        assert_snapshot(&session, TOTAL, 2);
    }

    #[test]
    fn retries_discard_unconfirmed_bytes_and_never_report_100_before_acknowledgement() {
        let mut p = UploadProgress::new(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            1000,
            2,
        );
        p.complete_chunk(1, 500);
        for _ in 0..5 {
            p.update_in_flight(2, 500);
            assert_eq!(p.uploaded_bytes(), 990);
            p.update_in_flight(2, 0);
            assert_eq!(p.percentage(), 50.0);
        }
        p.complete_chunk(2, 500);
        assert_eq!(p.percentage(), 100.0);
        p.update_in_flight(2, 1);
        p.complete_chunk(2, 1);
        assert_eq!(p.uploaded_bytes(), 1000);
        assert_eq!(p.completed_parts, 2);
    }

    #[test]
    fn pause_failure_and_resume_use_only_confirmed_parts() {
        let manager = ProgressManager::new(NoOpNotifier);
        let mut session = Session::new();
        session.register_file(
            "key".into(),
            "hash".into(),
            "transfer".into(),
            "".into(),
            "file".into(),
            1000,
            HashMap::new(),
        );
        session.complete_chunk("key", 1, "etag".into(), 500);
        let s = session.get_aggregate_progress(None);
        let t = session.get_aggregate_progress(Some("transfer"));
        manager.init(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            1000,
            2,
            500,
            1,
            HashSet::from([1]),
            0,
            s.clone(),
            t.clone(),
        );
        manager.update_in_flight("key", 2, 400, s.clone(), t.clone());
        for state in [UploadState::Failed, UploadState::Paused] {
            manager.set_status("key", state, s.clone(), t.clone());
            assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 500);
            manager.set_status("key", UploadState::Initialized, s.clone(), t.clone());
            manager.update_in_flight("key", 2, 500, s.clone(), t.clone());
            manager.init(
                "key".into(),
                "file".into(),
                "hash".into(),
                "transfer".into(),
                1000,
                2,
                500,
                1,
                HashSet::from([1]),
                0,
                s.clone(),
                t.clone(),
            );
            manager.update_in_flight("key", 2, 0, s.clone(), t.clone());
            // A late completion for a resumed, already-confirmed part is ignored.
            manager.complete_chunk("key", 1, 500, s.clone(), t.clone());
            let live = manager.get_live_progress(None, None).remove(0);
            assert_eq!(live.uploaded_bytes, 500);
            assert_eq!(live.completed_parts, 1);
            let (sa, ta) = manager.get_live_aggregate(s.clone(), t.clone(), "transfer");
            assert_eq!(sa.uploaded_size, 500);
            assert_eq!(ta.percentage, 50.0);
        }
        session.complete_chunk("key", 2, "etag2".into(), 500);
        manager.complete_chunk(
            "key",
            2,
            500,
            session.get_aggregate_progress(None),
            session.get_aggregate_progress(Some("transfer")),
        );
        assert_eq!(manager.get_live_progress(None, None)[0].percentage, 100.0);
    }

    #[test]
    fn an_old_request_cannot_repopulate_a_resumed_runs_in_flight_bytes() {
        let manager = ProgressManager::new(NoOpNotifier);
        let aggregate = Session::new().get_aggregate_progress(None);
        manager.init(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            1000,
            2,
            500,
            1,
            HashSet::from([1]),
            2,
            aggregate.clone(),
            aggregate.clone(),
        );
        manager.update_in_flight_for_run(
            "key",
            2,
            400,
            Some(1),
            aggregate.clone(),
            aggregate.clone(),
        );
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 500);
        manager.update_in_flight_for_run("key", 2, 400, Some(2), aggregate.clone(), aggregate);
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 900);
    }

    #[test]
    fn native_pause_and_viable_resume_preserve_progress_and_aggregates() {
        let manager = ProgressManager::new(NoOpNotifier);
        let mut session = Session::new();
        session.register_file(
            "key".into(),
            "hash".into(),
            "transfer".into(),
            "file".into(),
            "file".into(),
            1000,
            HashMap::new(),
        );
        session.complete_chunk("key", 1, "etag".into(), 500);
        let s = session.get_aggregate_progress(None);
        let t = session.get_aggregate_progress(Some("transfer"));
        manager.init(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            1000,
            2,
            500,
            1,
            HashSet::from([1]),
            7,
            s.clone(),
            t.clone(),
        );
        manager.update_in_flight_for_run("key", 2, 400, Some(7), s.clone(), t.clone());
        manager.set_status_with_pause_progress(
            "key",
            UploadState::Paused,
            true,
            s.clone(),
            t.clone(),
        );
        manager.update_in_flight_for_run("key", 2, 500, Some(7), s.clone(), t.clone());
        // A second pause notification must not discard the native snapshot.
        manager.set_status("key", UploadState::Paused, s.clone(), t.clone());
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 900);
        let (paused_s, paused_t) = manager.get_live_aggregate(s.clone(), t.clone(), "transfer");
        assert_eq!(paused_s.uploaded_size, 900);
        assert_eq!(paused_t.percentage, 90.0);
        manager.set_status("key", UploadState::Running, s.clone(), t.clone());
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 900);
        // Once a resumed attempt really fails, only that attempt is rolled back.
        manager.reset_failed_part("key", 2, 7, false, s, t);
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 500);
    }

    #[test]
    fn native_resume_rolls_back_only_failed_parts_not_viable_held_parts() {
        let manager = ProgressManager::new(NoOpNotifier);
        let mut session = Session::new();
        session.register_file(
            "key".into(),
            "hash".into(),
            "transfer".into(),
            "file".into(),
            "file".into(),
            1500,
            HashMap::new(),
        );
        session.complete_chunk("key", 1, "etag".into(), 500);
        let s = session.get_aggregate_progress(None);
        let t = session.get_aggregate_progress(Some("transfer"));
        manager.init(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            1500,
            3,
            500,
            1,
            HashSet::from([1]),
            7,
            s.clone(),
            t.clone(),
        );
        manager.update_in_flight_for_run("key", 2, 400, Some(7), s.clone(), t.clone());
        manager.update_in_flight_for_run("key", 3, 200, Some(7), s.clone(), t.clone());
        manager.set_status_with_pause_progress(
            "key",
            UploadState::Paused,
            true,
            s.clone(),
            t.clone(),
        );
        manager.reset_failed_part("key", 2, 6, true, s.clone(), t.clone()); // Stale error: ignored.
        manager.reset_failed_part("key", 2, 7, true, s.clone(), t.clone());
        assert_eq!(
            manager.get_live_progress(None, None)[0].uploaded_bytes,
            1100
        );
        manager.set_status("key", UploadState::Running, s.clone(), t.clone());
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 700);
        assert_eq!(
            manager
                .get_live_aggregate(s.clone(), t.clone(), "transfer")
                .0
                .uploaded_size,
            700
        );
        // A completely restarted run has no resumable in-flight bytes.
        manager.set_status_with_pause_progress(
            "key",
            UploadState::Paused,
            true,
            s.clone(),
            t.clone(),
        );
        manager.set_status("key", UploadState::Initialized, s, t);
        assert_eq!(manager.get_live_progress(None, None)[0].uploaded_bytes, 500);
    }

    #[test]
    fn concurrent_workers_deliver_snapshots_in_progress_order() {
        use std::sync::{mpsc, Arc, Condvar};
        use std::thread;
        use std::time::Duration;

        struct GatedNotifier {
            entered: mpsc::Sender<()>,
            gate: Arc<(Mutex<bool>, Condvar)>,
            seen: Arc<Mutex<Vec<u64>>>,
        }
        impl ProgressNotifier for GatedNotifier {
            fn notify(&self, fp: &FileProgress, _: &AggregateProgress, _: &AggregateProgress) {
                self.seen.lock().unwrap().push(fp.uploaded_bytes);
                if fp.uploaded_bytes == 100 {
                    self.entered.send(()).unwrap();
                    let (lock, signal) = &*self.gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = signal.wait(released).unwrap();
                    }
                }
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let manager = Arc::new(ProgressManager::new(GatedNotifier {
            entered: entered_tx,
            gate: gate.clone(),
            seen: seen.clone(),
        }));
        let aggregate = Session::new().get_aggregate_progress(None);
        manager.init(
            "key".into(),
            "file".into(),
            "hash".into(),
            "transfer".into(),
            1000,
            2,
            0,
            0,
            HashSet::new(),
            0,
            aggregate.clone(),
            aggregate.clone(),
        );
        let first = {
            let manager = manager.clone();
            let aggregate = aggregate.clone();
            thread::spawn(move || {
                manager.update_in_flight("key", 1, 100, aggregate.clone(), aggregate)
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let second = {
            let manager = manager.clone();
            thread::spawn(move || {
                started_tx.send(()).unwrap();
                manager.update_in_flight("key", 2, 100, aggregate.clone(), aggregate);
                finished_tx.send(()).unwrap();
            })
        };
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let returned_early = finished_rx.recv_timeout(Duration::from_millis(200)).is_ok();
        // Polling must remain available while delivery is blocked.
        let polled = manager.get_live_progress(None, None)[0].uploaded_bytes;
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        first.join().unwrap();
        second.join().unwrap();
        assert!(
            !returned_early,
            "A newer snapshot overtook the blocked callback"
        );
        assert_eq!(polled, 100);
        assert_eq!(*seen.lock().unwrap(), vec![0, 100, 200]);
    }
}

impl UploadProgress {
    pub fn new(
        file_key: String,
        file_name: String,
        file_hash: String,
        transfer_id: String,
        total_bytes: u64,
        total_parts: u32,
    ) -> Self {
        Self {
            file_key,
            file_name,
            file_hash,
            transfer_id,
            total_bytes,
            total_parts,
            completed_bytes: 0,
            in_flight_progress: HashMap::new(),
            completed_parts: 0,
            status: UploadState::Running,
            run_version: 0,
            completed_part_set: HashSet::new(),
            preserve_paused_progress: false,
            deferred_failed_parts: HashSet::new(),
        }
    }

    pub fn uploaded_bytes(&self) -> u64 {
        if self.status != UploadState::Running
            && !(self.status == UploadState::Paused && self.preserve_paused_progress)
        {
            return self.completed_bytes.min(self.total_bytes);
        }
        let in_flight_sum: u64 = self.in_flight_progress.values().sum();
        self.completed_bytes
            .saturating_add(in_flight_sum)
            .min(self.progress_ceiling())
    }

    fn progress_ceiling(&self) -> u64 {
        if self.completed_bytes >= self.total_bytes {
            self.total_bytes
        } else {
            self.total_bytes
                .saturating_sub(self.total_bytes.div_ceil(100))
                .max(self.completed_bytes)
        }
    }

    pub fn percentage(&self) -> f64 {
        if self.status == UploadState::Completed {
            return 100.0;
        }
        if self.total_bytes == 0 {
            return 0.0;
        }
        ((self.uploaded_bytes() as f64 / self.total_bytes as f64) * 100.0).min(100.0)
    }

    pub fn update_in_flight(&mut self, part_number: u32, bytes_uploaded: u64) {
        if self.completed_part_set.contains(&part_number) {
            return;
        }
        self.in_flight_progress.insert(part_number, bytes_uploaded);
    }

    pub fn complete_chunk(&mut self, part_number: u32, chunk_size: u64) {
        if !self.completed_part_set.insert(part_number) {
            // Already counted by a concurrent worker from the same or a previous run.
            self.in_flight_progress.remove(&part_number);
            return;
        }
        self.in_flight_progress.remove(&part_number);
        self.completed_bytes += chunk_size;
        self.completed_parts += 1;
    }

    pub fn to_file_progress(&self) -> FileProgress {
        FileProgress {
            file_key: Some(self.file_key.clone()),
            file_name: self.file_name.clone(),
            file_hash: self.file_hash.clone(),
            transfer_id: self.transfer_id.clone(),
            total_bytes: self.total_bytes,
            uploaded_bytes: self.uploaded_bytes(),
            completed_parts: self.completed_parts,
            total_parts: self.total_parts,
            percentage: self.percentage(),
            state: self.status.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Notifier trait and no-op implementation
// ---------------------------------------------------------------------------

/// Platform-specific progress callback abstraction.
pub trait ProgressNotifier: Send + Sync {
    fn notify(
        &self,
        file_progress: &FileProgress,
        session_aggregate: &AggregateProgress,
        transfer_aggregate: &AggregateProgress,
    );
}

pub struct NoOpNotifier;

impl ProgressNotifier for NoOpNotifier {
    fn notify(&self, _: &FileProgress, _: &AggregateProgress, _: &AggregateProgress) {}
}

// ---------------------------------------------------------------------------
// ProgressManager — tracks all active file uploads
// ---------------------------------------------------------------------------

pub struct ProgressManager<N: ProgressNotifier> {
    notifier: N,
    /// Serialize snapshot construction and delivery across native part workers.
    /// The progress lock is still released before invoking a notifier, so a
    /// callback can safely query progress without seeing out-of-order snapshots.
    notification: Mutex<()>,
    progress: Mutex<HashMap<String, UploadProgress>>,
}

impl<N: ProgressNotifier> ProgressManager<N> {
    pub fn new(notifier: N) -> Self {
        Self {
            notifier,
            notification: Mutex::new(()),
            progress: Mutex::new(HashMap::new()),
        }
    }

    /// Build session- and transfer-level aggregates that include in-flight bytes
    /// from all currently tracked files, merged on top of the session baseline.
    fn build_aggregates(
        lock: &HashMap<String, UploadProgress>,
        mut session_agg: AggregateProgress,
        mut transfer_agg: AggregateProgress,
        current_transfer_id: &str,
    ) -> (AggregateProgress, AggregateProgress) {
        let committed_session_bytes = session_agg.uploaded_size;
        let committed_transfer_bytes = transfer_agg.uploaded_size;
        // For each tracked file, replace the session's `uploaded_bytes` baseline
        // with the live value (completed + in-flight) from ProgressManager.
        for p in lock.values() {
            let live = p.uploaded_bytes();
            // The session already counted `p.completed_bytes` worth of uploads.
            // Add only the delta (in-flight portion) on top.
            let delta = live.saturating_sub(p.completed_bytes);

            session_agg.uploaded_size = session_agg.uploaded_size.saturating_add(delta);
            if session_agg.total_size > 0 {
                session_agg.percentage =
                    ((session_agg.uploaded_size as f64 / session_agg.total_size as f64) * 100.0)
                        .min(100.0);
            }

            if p.transfer_id == current_transfer_id {
                transfer_agg.uploaded_size = transfer_agg.uploaded_size.saturating_add(delta);
                if transfer_agg.total_size > 0 {
                    transfer_agg.percentage = ((transfer_agg.uploaded_size as f64
                        / transfer_agg.total_size as f64)
                        * 100.0)
                        .min(100.0);
                }
            }
        }
        // Cap unacknowledged bytes without lowering the confirmed baseline,
        // which may already exceed 99% when only a small final part remains.
        if lock.values().any(|p| {
            (p.status == UploadState::Running || p.preserve_paused_progress)
                && p.completed_bytes < p.total_bytes
        }) {
            session_agg.uploaded_size = session_agg.uploaded_size.min(
                session_agg
                    .total_size
                    .saturating_sub(session_agg.total_size.div_ceil(100))
                    .max(committed_session_bytes),
            );
            if session_agg.total_size > 0 {
                session_agg.percentage =
                    session_agg.uploaded_size as f64 / session_agg.total_size as f64 * 100.0;
            }
        }
        if lock.values().any(|p| {
            (p.status == UploadState::Running || p.preserve_paused_progress)
                && p.transfer_id == current_transfer_id
                && p.completed_bytes < p.total_bytes
        }) {
            transfer_agg.uploaded_size = transfer_agg.uploaded_size.min(
                transfer_agg
                    .total_size
                    .saturating_sub(transfer_agg.total_size.div_ceil(100))
                    .max(committed_transfer_bytes),
            );
            if transfer_agg.total_size > 0 {
                transfer_agg.percentage =
                    transfer_agg.uploaded_size as f64 / transfer_agg.total_size as f64 * 100.0;
            }
        }
        (session_agg, transfer_agg)
    }

    pub fn init(
        &self,
        file_key: String,
        file_name: String,
        file_hash: String,
        transfer_id: String,
        total_bytes: u64,
        total_parts: u32,
        already_completed_bytes: u64,
        already_completed_parts: u32,
        completed_part_numbers: HashSet<u32>,
        run_version: u64,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        let mut p = UploadProgress::new(
            file_key.clone(),
            file_name,
            file_hash,
            transfer_id,
            total_bytes,
            total_parts,
        );
        p.completed_bytes = already_completed_bytes;
        p.completed_parts = already_completed_parts;
        p.completed_part_set = completed_part_numbers;
        p.run_version = run_version;
        let mut lock = self.progress.lock().unwrap();
        let fp = p.to_file_progress();
        let tid = p.transfer_id.clone();
        lock.insert(file_key, p);
        let (s, t) = Self::build_aggregates(&lock, session_agg, transfer_agg, &tid);
        drop(lock);
        self.notifier.notify(&fp, &s, &t);
    }

    pub fn tracked_file_keys(&self) -> Vec<String> {
        self.progress.lock().unwrap().keys().cloned().collect()
    }

    pub fn clear(&self) {
        self.progress.lock().unwrap().clear();
    }

    pub fn update_in_flight(
        &self,
        file_key: &str,
        part_number: u32,
        bytes_uploaded: u64,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        self.update_in_flight_for_run(
            file_key,
            part_number,
            bytes_uploaded,
            None,
            session_agg,
            transfer_agg,
        );
    }

    pub fn update_in_flight_for_run(
        &self,
        file_key: &str,
        part_number: u32,
        bytes_uploaded: u64,
        run_version: Option<u64>,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        let mut lock = self.progress.lock().unwrap();
        if let Some(p) = lock.get_mut(file_key) {
            if p.status != UploadState::Running
                || run_version.is_some_and(|version| p.run_version != version)
            {
                // Don't re-populate in_flight after pause; stale bytes would inflate
                // progress reported by polls and aggregate calculations.
                return;
            }
            p.update_in_flight(part_number, bytes_uploaded);
            let fp = p.to_file_progress();
            let tid = p.transfer_id.clone();
            let (s, t) = Self::build_aggregates(&lock, session_agg, transfer_agg, &tid);
            drop(lock);
            self.notifier.notify(&fp, &s, &t);
        }
    }

    pub fn complete_chunk(
        &self,
        file_key: &str,
        part_number: u32,
        chunk_size: u64,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        let mut lock = self.progress.lock().unwrap();
        if let Some(p) = lock.get_mut(file_key) {
            p.complete_chunk(part_number, chunk_size);
            let fp = p.to_file_progress();
            let tid = p.transfer_id.clone();
            let (s, t) = Self::build_aggregates(&lock, session_agg, transfer_agg, &tid);
            drop(lock);
            self.notifier.notify(&fp, &s, &t);
        }
    }

    pub fn set_status(
        &self,
        file_key: &str,
        status: UploadState,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        self.set_status_with_pause_progress(file_key, status, false, session_agg, transfer_agg);
    }

    pub fn set_status_with_pause_progress(
        &self,
        file_key: &str,
        status: UploadState,
        preserve: bool,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        let mut lock = self.progress.lock().unwrap();
        if let Some(p) = lock.get_mut(file_key) {
            let keep = status == UploadState::Paused
                && (preserve || (p.status == UploadState::Paused && p.preserve_paused_progress));
            if status == UploadState::Running {
                for part in p.deferred_failed_parts.drain() {
                    p.in_flight_progress.remove(&part);
                }
            }
            p.status = status.clone();
            p.preserve_paused_progress = keep;
            if status != UploadState::Running && !keep {
                p.in_flight_progress.clear();
                p.deferred_failed_parts.clear();
            }
            let fp = p.to_file_progress();
            let tid = p.transfer_id.clone();
            let (s, t) = Self::build_aggregates(&lock, session_agg, transfer_agg, &tid);
            drop(lock);
            self.notifier.notify(&fp, &s, &t);
        }
    }

    /// Keep a native paused snapshot stable. Reset only failed attempts once the
    /// user resumes; healthy held attempts keep their absolute byte counters.
    pub fn reset_failed_part(
        &self,
        file_key: &str,
        part: u32,
        version: u64,
        defer_until_resume: bool,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        let mut lock = self.progress.lock().unwrap();
        if let Some(p) = lock.get_mut(file_key) {
            if p.run_version != version || p.completed_part_set.contains(&part) {
                return;
            }
            if defer_until_resume || (p.status == UploadState::Paused && p.preserve_paused_progress)
            {
                p.deferred_failed_parts.insert(part);
                return;
            }
            if p.status != UploadState::Running {
                return;
            }
            p.in_flight_progress.remove(&part);
            let fp = p.to_file_progress();
            let tid = p.transfer_id.clone();
            let (s, t) = Self::build_aggregates(&lock, session_agg, transfer_agg, &tid);
            drop(lock);
            self.notifier.notify(&fp, &s, &t);
        }
    }

    pub fn remove(&self, file_key: &str) {
        self.progress.lock().unwrap().remove(file_key);
    }

    /// Register a file's initial state in the progress map and fire a notification.
    /// Called right after `initialize_and_enqueue` / `process_pending_files` so clients
    /// see the correct state (INITIALIZED for new/resumed files, COMPLETED for already-done
    /// files) before the upload worker starts — or instead of it for completed files.
    /// If the worker thread has already advanced the entry, we notify with the live state.
    pub fn register_not_started(
        &self,
        file_key: String,
        file_name: String,
        file_hash: String,
        transfer_id: String,
        total_bytes: u64,
        committed_bytes: u64,
        committed_parts: u32,
        state: UploadState,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        let mut lock = self.progress.lock().unwrap();
        if let Some(existing) = lock.get(&file_key) {
            // Worker already started — notify with the live state, don't overwrite.
            let fp = existing.to_file_progress();
            drop(lock);
            self.notifier.notify(&fp, &session_agg, &transfer_agg);
            return;
        }
        let mut p = UploadProgress::new(
            file_key.clone(),
            file_name,
            file_hash,
            transfer_id,
            total_bytes,
            0,
        );
        p.status = state;
        p.completed_bytes = committed_bytes;
        p.completed_parts = committed_parts;
        let fp = p.to_file_progress();
        lock.insert(file_key, p);
        drop(lock);
        self.notifier.notify(&fp, &session_agg, &transfer_agg);
    }

    /// Fire a one-shot notification without inserting into the progress map.
    /// Used to deliver the initial state of a newly registered (NotStarted) file.
    pub fn notify_external(
        &self,
        fp: &crate::core::session::FileProgress,
        session_agg: &AggregateProgress,
        transfer_agg: &AggregateProgress,
    ) {
        let _notification = self.notification.lock().unwrap();
        self.notifier.notify(fp, session_agg, transfer_agg);
    }

    /// Return live FileProgress snapshots (including in-flight bytes) for all tracked files.
    /// Optionally filtered by transfer_id or file_hash.
    pub fn get_live_progress(
        &self,
        transfer_id: Option<&str>,
        file_hash: Option<&str>,
    ) -> Vec<FileProgress> {
        let lock = self.progress.lock().unwrap();
        lock.values()
            .filter(|p| {
                transfer_id.map_or(true, |t| p.transfer_id == t)
                    && file_hash.map_or(true, |h| p.file_hash == h)
            })
            .map(|p| p.to_file_progress())
            .collect()
    }

    /// Return live aggregate progress (including in-flight bytes) merged with session baseline.
    pub fn get_live_aggregate(
        &self,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
        transfer_id: &str,
    ) -> (AggregateProgress, AggregateProgress) {
        let lock = self.progress.lock().unwrap();
        Self::build_aggregates(&lock, session_agg, transfer_agg, transfer_id)
    }
}
