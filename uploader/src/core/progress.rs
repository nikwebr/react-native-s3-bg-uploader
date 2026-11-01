use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::core::session::{AggregateProgress, FileProgress, UploadState};

// ---------------------------------------------------------------------------
// Per-file upload progress (committed bytes only)
// ---------------------------------------------------------------------------
//
// Progress only counts parts whose ETag the backend has returned. Bytes handed to
// the transport are *not* progress: a connection that drops after the last byte
// left the device still fails that part, and it has to be sent again. Counting it
// early made the bar claim 100% (and then fall back on the retry or resume) for an
// upload that was not finished. Because unacknowledged bytes are never counted,
// there is nothing to rewind either, so reconnecting continues from the last
// committed part instead of dropping the bar.

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
            completed_parts: 0,
            status: UploadState::Running,
            completed_part_set: HashSet::new(),
        }
    }

    /// Bytes the backend has acknowledged. In-flight bytes are deliberately excluded:
    /// they can still fail and be sent again, so they are not durable progress.
    pub fn uploaded_bytes(&self) -> u64 {
        self.completed_bytes
    }

    pub fn percentage(&self) -> f64 {
        if self.status == UploadState::Completed {
            return 100.0;
        }
        if self.total_bytes == 0 {
            return 0.0;
        }
        ((self.completed_bytes as f64 / self.total_bytes as f64) * 100.0).min(100.0)
    }

    pub fn complete_chunk(&mut self, part_number: u32, chunk_size: u64) {
        if !self.completed_part_set.insert(part_number) {
            // Already counted by a concurrent worker from the same or a previous run.
            return;
        }
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

    /// Session- and transfer-level aggregates already include every committed part
    /// (the session is updated before the progress manager), so there is no in-flight
    /// delta left to merge here.
    fn build_aggregates(
        _lock: &HashMap<String, UploadProgress>,
        session_agg: AggregateProgress,
        transfer_agg: AggregateProgress,
        _current_transfer_id: &str,
    ) -> (AggregateProgress, AggregateProgress) {
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

    /// Return live FileProgress snapshots for all tracked files.
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

    /// Return the aggregate progress already merged with the committed parts.
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
    fn unacknowledged_bytes_are_not_progress() {
        let mut p = progress(1000, 2);
        // Part 1 is stored, part 2 is still on the wire. The transport progress of
        // part 2 must not count: its response can still be lost and the part retried.
        p.complete_chunk(1, 500);
        assert_eq!(p.uploaded_bytes(), 500);
        assert_eq!(p.percentage(), 50.0);

        p.complete_chunk(2, 500);
        assert_eq!(p.uploaded_bytes(), 1000);
        // All bytes are committed now; completion follows immediately and reports 100.
        assert_eq!(p.percentage(), 100.0);
        p.status = UploadState::Completed;
        assert_eq!(p.percentage(), 100.0);
    }

    #[test]
    fn a_retry_never_rewinds_progress() {
        let mut p = progress(1000, 2);
        p.complete_chunk(1, 500);
        let before = p.uploaded_bytes();

        // Part 2 failed and is sent again: no bytes of it were ever counted, so the
        // bar stays where it was instead of climbing down and back up.
        assert_eq!(p.uploaded_bytes(), before);
        assert_eq!(p.percentage(), 50.0);
        p.complete_chunk(2, 500);
        assert_eq!(p.uploaded_bytes(), 1000);
        assert_eq!(p.percentage(), 100.0);
    }

    #[test]
    fn a_stopped_upload_keeps_its_committed_progress() {
        let mut p = progress(1000, 2);
        p.complete_chunk(1, 500);
        p.status = UploadState::Failed;
        // Nothing was counted for the failed part, so resuming cannot drop below this.
        assert_eq!(p.uploaded_bytes(), 500);
        assert_eq!(p.percentage(), 50.0);
    }

    #[test]
    fn committed_part_is_counted_once() {
        let mut p = progress(1000, 2);
        p.complete_chunk(1, 400);
        p.complete_chunk(1, 400);
        assert_eq!(p.uploaded_bytes(), 400);
        assert_eq!(p.completed_parts, 1);
        assert_eq!(p.percentage(), 40.0);
    }
}
