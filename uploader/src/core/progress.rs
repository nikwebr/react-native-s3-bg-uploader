use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::core::session::{AggregateProgress, FileProgress, GlobalUploaderState, UploadState};

/// Percentage reported while an upload is still running.
///
/// `uploaded_bytes` counts in-flight bytes, i.e. the part body that is on the wire right
/// now, on top of the parts the backend already acknowledged. That is the network
/// progress the user watches while a part is sent, instead of a bar that only jumps at
/// part boundaries. Those bytes are not acknowledged yet: a connection that dies after
/// the last byte left the device still fails that part, which has to be sent again. A
/// running upload is therefore kept just below 100 and the real 100 is reported only
/// together with `Completed`, so a reported 100% always means "every part is stored".
pub const RUNNING_PERCENTAGE_CAP: f64 = 99.9;

/// Percentage cap for a session or transfer aggregate in the given state.
pub fn percentage_cap(state: &GlobalUploaderState) -> f64 {
    if *state == GlobalUploaderState::Completed {
        100.0
    } else {
        RUNNING_PERCENTAGE_CAP
    }
}

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
    /// Guards against double-counting when stale workers from a previous run
    /// call complete_chunk after a pause+resume starts a new run.
    completed_part_set: HashSet<u32>,
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
            completed_part_set: HashSet::new(),
        }
    }

    pub fn uploaded_bytes(&self) -> u64 {
        let in_flight_sum: u64 = self.in_flight_progress.values().sum();
        let reported = self.completed_bytes.saturating_add(in_flight_sum);
        if self.total_bytes == 0 {
            reported
        } else {
            // A late in-flight report for a part that another worker already committed
            // must not lift the file above its own size.
            reported.min(self.total_bytes)
        }
    }

    pub fn percentage(&self) -> f64 {
        if self.status == UploadState::Completed {
            return 100.0;
        }
        if self.total_bytes == 0 {
            return 0.0;
        }
        ((self.uploaded_bytes() as f64 / self.total_bytes as f64) * 100.0)
            .min(RUNNING_PERCENTAGE_CAP)
    }

    /// Record the bytes one attempt of `part_number` has handed to the transport.
    ///
    /// One attempt always starts at zero and the transport reports cumulative bytes for
    /// that attempt only. A part that fails is re-sent from the beginning, so the bytes
    /// of the failed attempt are gone — the caller resets the part with
    /// `update_in_flight(part_number, 0)` before every attempt (see the platform
    /// `upload_chunk_with_retry`). That is what keeps the bar honest: it climbs with the
    /// bytes actually being transferred, drops back to the acknowledged level when an
    /// attempt is lost, and climbs again while the part is re-uploaded.
    pub fn update_in_flight(&mut self, part_number: u32, bytes_uploaded: u64) {
        if self.completed_part_set.contains(&part_number) {
            // A late report for a part that already delivered its ETag. Adding it on top
            // of the committed size would count the same bytes twice.
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
    progress: Mutex<HashMap<String, UploadProgress>>,
}

impl<N: ProgressNotifier> ProgressManager<N> {
    pub fn new(notifier: N) -> Self {
        Self {
            notifier,
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
                        .min(percentage_cap(&session_agg.state));
            }

            if p.transfer_id == current_transfer_id {
                transfer_agg.uploaded_size = transfer_agg.uploaded_size.saturating_add(delta);
                if transfer_agg.total_size > 0 {
                    transfer_agg.percentage = ((transfer_agg.uploaded_size as f64
                        / transfer_agg.total_size as f64)
                        * 100.0)
                        .min(percentage_cap(&transfer_agg.state));
                }
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
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
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
        let fp = p.to_file_progress();
        self.progress.lock().unwrap().insert(file_key, p);
        self.notifier.notify(&fp, &session_agg, &transfer_agg);
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
        let mut lock = self.progress.lock().unwrap();
        if let Some(p) = lock.get_mut(file_key) {
            if p.status != UploadState::Running {
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
        let mut lock = self.progress.lock().unwrap();
        if let Some(p) = lock.get_mut(file_key) {
            p.status = status.clone();
            if matches!(
                status,
                UploadState::Paused | UploadState::Failed | UploadState::Completed
            ) {
                // In-flight bytes belong to an upload that stopped: they were never
                // acknowledged. Keeping them reported progress (up to the whole file)
                // for an upload that was no longer running, and the resumed run then
                // rebuilt the entry from the committed parts and dropped back — the
                // visible progress loss users saw after a connection error.
                p.in_flight_progress.clear();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(total_bytes: u64, total_parts: u32) -> UploadProgress {
        UploadProgress::new(
            "key".into(),
            "file.bin".into(),
            "hash".into(),
            "transfer".into(),
            total_bytes,
            total_parts,
        )
    }

    #[test]
    fn in_flight_bytes_are_reported_but_never_as_100() {
        let mut p = progress(1000, 2);
        p.complete_chunk(1, 500);
        assert_eq!(p.percentage(), 50.0);

        // Network progress: part 2 is half way through its body.
        p.update_in_flight(2, 250);
        assert_eq!(p.uploaded_bytes(), 750);
        assert_eq!(p.percentage(), 75.0);

        // Every byte of part 2 left the device, but its ETag has not arrived: the part
        // can still fail and be sent again, so the upload must not claim to be finished.
        p.update_in_flight(2, 500);
        assert_eq!(p.uploaded_bytes(), 1000);
        assert_eq!(p.percentage(), RUNNING_PERCENTAGE_CAP);

        p.complete_chunk(2, 500);
        p.status = UploadState::Completed;
        assert_eq!(p.percentage(), 100.0);
    }

    #[test]
    fn a_failed_attempt_drops_its_bytes_so_the_retry_is_visible() {
        let mut p = progress(1000, 2);
        p.complete_chunk(1, 500);
        p.update_in_flight(2, 500);
        assert_eq!(p.uploaded_bytes(), 1000);

        // The response was lost, so part 2 has to be sent again. Its bytes were never
        // stored: the retry starts at zero and only the bytes actually re-sent count.
        p.update_in_flight(2, 0);
        assert_eq!(p.uploaded_bytes(), 500);

        p.update_in_flight(2, 200);
        assert_eq!(p.uploaded_bytes(), 700);

        // Once the retry is acknowledged the part counts once, not twice.
        p.complete_chunk(2, 500);
        assert_eq!(p.uploaded_bytes(), 1000);
        assert_eq!(p.completed_parts, 2);
    }

    #[test]
    fn a_stopped_upload_reports_committed_bytes_only() {
        let mut p = progress(1000, 2);
        p.complete_chunk(1, 500);
        p.update_in_flight(2, 400);
        // Paused/Failed/Completed clear in-flight bytes: nothing is on the wire any more
        // and none of it was acknowledged, so the resumed run continues from part 1.
        p.in_flight_progress.clear();
        p.status = UploadState::Failed;
        assert_eq!(p.uploaded_bytes(), 500);
        assert_eq!(p.percentage(), 50.0);
    }

    #[test]
    fn committed_part_is_counted_once() {
        let mut p = progress(1000, 2);
        p.update_in_flight(1, 1000);
        p.complete_chunk(1, 400);
        p.complete_chunk(1, 400);
        // A late in-flight report for an acknowledged part cannot double count it.
        p.update_in_flight(1, 400);
        assert_eq!(p.uploaded_bytes(), 400);
        assert_eq!(p.completed_parts, 1);
        assert!(p.percentage() <= RUNNING_PERCENTAGE_CAP);
    }
}
