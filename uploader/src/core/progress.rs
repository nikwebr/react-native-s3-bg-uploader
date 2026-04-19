use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::session::{AggregateProgress, FileProgress, UploadState};

// ---------------------------------------------------------------------------
// Per-file upload progress (in-flight tracking during chunk uploads)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UploadProgress {
    /// S3 key — the public identifier returned by uploadFile().
    pub file_key: String,
    pub transfer_id: String,
    pub total_bytes: u64,
    pub total_parts: u32,
    /// Bytes in completed chunks only.
    pub completed_bytes: u64,
    /// part_number → bytes uploaded so far (in-flight, cleared on chunk completion).
    pub in_flight_progress: HashMap<u32, u64>,
    pub completed_parts: u32,
    pub status: UploadState,
}

impl UploadProgress {
    pub fn new(file_key: String, transfer_id: String, total_bytes: u64, total_parts: u32) -> Self {
        Self {
            file_key,
            transfer_id,
            total_bytes,
            total_parts,
            completed_bytes: 0,
            in_flight_progress: HashMap::new(),
            completed_parts: 0,
            status: UploadState::Running,
        }
    }

    pub fn uploaded_bytes(&self) -> u64 {
        let in_flight_sum: u64 = self.in_flight_progress.values().sum();
        self.completed_bytes + in_flight_sum
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
        self.in_flight_progress.insert(part_number, bytes_uploaded);
    }

    pub fn complete_chunk(&mut self, part_number: u32, chunk_size: u64) {
        self.in_flight_progress.remove(&part_number);
        self.completed_bytes += chunk_size;
        self.completed_parts += 1;
    }

    pub fn to_file_progress(&self) -> FileProgress {
        FileProgress {
            file_key: self.file_key.clone(),
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
        (session_agg, transfer_agg)
    }

    pub fn init(
        &self,
        file_key: String,
        transfer_id: String,
        total_bytes: u64,
        total_parts: u32,
        already_completed_bytes: u64,
        already_completed_parts: u32,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
    ) {
        let mut p = UploadProgress::new(file_key.clone(), transfer_id, total_bytes, total_parts);
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
            p.update_in_flight(part_number, bytes_uploaded);
            if p.status != UploadState::Running {
                return;
            }
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
            p.status = status;
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

    /// Return live FileProgress snapshots (including in-flight bytes) for all tracked files.
    /// Optionally filtered by transfer_id or file_key.
    pub fn get_live_progress(
        &self,
        transfer_id: Option<&str>,
        file_key: Option<&str>,
    ) -> Vec<FileProgress> {
        let lock = self.progress.lock().unwrap();
        lock.values()
            .filter(|p| {
                transfer_id.map_or(true, |t| p.transfer_id == t)
                    && file_key.map_or(true, |k| p.file_key == k)
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
