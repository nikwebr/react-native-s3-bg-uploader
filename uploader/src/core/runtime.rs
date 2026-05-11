use crate::core::progress::{ProgressManager, ProgressNotifier};
use crate::core::session::{self, AggregateProgress, UploadState};

pub fn transfer_id_for_file(file_key: &str) -> String {
    session::session()
        .files
        .get(file_key)
        .map(|e| e.transfer_id.clone())
        .unwrap_or_default()
}

pub fn aggregate_snapshot(transfer_id: &str) -> (AggregateProgress, AggregateProgress) {
    let sess = session::session();
    (
        sess.get_aggregate_progress(None),
        sess.get_aggregate_progress(Some(transfer_id)),
    )
}

pub fn aggregate_snapshot_for_file(
    file_key: &str,
) -> (String, AggregateProgress, AggregateProgress) {
    let transfer_id = transfer_id_for_file(file_key);
    let (session_agg, transfer_agg) = aggregate_snapshot(&transfer_id);
    (transfer_id, session_agg, transfer_agg)
}

pub fn pause_all<N: ProgressNotifier>(manager: &ProgressManager<N>) {
    session::pause_all();
    let paused_keys: Vec<String> = {
        let sess = session::session();
        manager
            .tracked_file_keys()
            .into_iter()
            .filter(|k| sess.files.get(k).map_or(false, |e| e.state == UploadState::Paused))
            .collect()
    };
    for file_key in &paused_keys {
        set_status(manager, file_key, UploadState::Paused);
    }
}

/// Call `session::resume_all()` and fire progress callbacks so the UI reflects the
/// resumed state immediately, without waiting for the next upload event.
/// Paused files show Running (the upload may still be active on fast pause+resume).
/// Failed files show Initialized (they will restart from scratch).
/// Does NOT touch platform pause flags — the caller is responsible for clearing those.
/// Returns an error if any session file still needs a fresh local file reference.
pub fn resume_all<N: ProgressNotifier>(manager: &ProgressManager<N>) -> Result<(), String> {
    let resumed_info: Vec<(String, UploadState)> = {
        let sess = session::session();
        let missing = sess.files_needing_provision.len();
        if missing > 0 {
            return Err(format!(
                "Cannot resume: {missing} file(s) not yet re-provided. Call uploadFile() for each missing file first."
            ));
        }
        sess.files
            .values()
            .filter(|e| e.state == UploadState::Paused || e.state == UploadState::Failed)
            .map(|e| (e.file_key.clone(), e.state.clone()))
            .collect()
    };
    session::resume_all();
    for (key, original_state) in &resumed_info {
        let new_status = if *original_state == UploadState::Paused {
            UploadState::Running
        } else {
            UploadState::Initialized
        };
        set_status(manager, &key, new_status);
    }
    Ok(())
}

pub fn mark_state_persist(file_key: &str, state: UploadState) {
    session::session().mark_file_state(file_key, state);
    session::persist_session();
}

pub fn init_progress<N: ProgressNotifier>(
    manager: &ProgressManager<N>,
    file_key: &str,
    total_bytes: u64,
    total_parts: u32,
    committed_bytes: u64,
    completed_parts: u32,
) {
    let (transfer_id, session_agg, transfer_agg) = aggregate_snapshot_for_file(file_key);
    let (file_name, file_hash, already_paused) = {
        let sess = session::session();
        let e = sess.files.get(file_key);
        (
            e.map(|e| e.file_name.clone()).unwrap_or_default(),
            e.map(|e| e.file_hash.clone()).unwrap_or_default(),
            e.map(|e| e.state == UploadState::Paused).unwrap_or(false),
        )
    };
    manager.init(
        file_key.to_string(),
        file_name,
        file_hash,
        transfer_id,
        total_bytes,
        total_parts,
        committed_bytes,
        completed_parts,
        session_agg,
        transfer_agg,
    );
    // pause_all() may have set the session to Paused between mark_state_persist(Running)
    // and here. Snap the ProgressManager entry immediately so no in-flight bytes leak
    // through and the UI sees Paused right away instead of briefly Running.
    if already_paused {
        set_status(manager, file_key, UploadState::Paused);
    }
}

pub fn set_status<N: ProgressNotifier>(
    manager: &ProgressManager<N>,
    file_key: &str,
    status: UploadState,
) {
    let (_, session_agg, transfer_agg) = aggregate_snapshot_for_file(file_key);
    manager.set_status(file_key, status, session_agg, transfer_agg);
}

pub fn update_in_flight<N: ProgressNotifier>(
    manager: &ProgressManager<N>,
    file_key: &str,
    part_number: u32,
    bytes_uploaded: u64,
) {
    let (_, session_agg, transfer_agg) = aggregate_snapshot_for_file(file_key);
    manager.update_in_flight(
        file_key,
        part_number,
        bytes_uploaded,
        session_agg,
        transfer_agg,
    );
}

pub fn complete_chunk<N: ProgressNotifier>(
    manager: &ProgressManager<N>,
    file_key: &str,
    part_number: u32,
    etag: String,
    chunk_size: u64,
) {
    session::session().complete_chunk(file_key, part_number, etag, chunk_size);
    session::persist_session();
    let (_, session_agg, transfer_agg) = aggregate_snapshot_for_file(file_key);
    manager.complete_chunk(file_key, part_number, chunk_size, session_agg, transfer_agg);
}

/// Fire a progress callback announcing that a file was cancelled.
/// Call AFTER removing the file from session and ProgressManager so the aggregates
/// already reflect the reduced totals.
pub fn notify_cancelled_file<N: ProgressNotifier>(
    manager: &ProgressManager<N>,
    file_key: &str,
    file_name: &str,
    file_hash: &str,
    transfer_id: &str,
    total_bytes: u64,
    uploaded_bytes: u64,
) {
    let (session_agg, transfer_agg) = aggregate_snapshot(transfer_id);
    let percentage = if total_bytes > 0 {
        (uploaded_bytes as f64 / total_bytes as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let fp = session::FileProgress {
        file_key: Some(file_key.to_string()),
        file_name: file_name.to_string(),
        file_hash: file_hash.to_string(),
        transfer_id: transfer_id.to_string(),
        total_bytes,
        uploaded_bytes,
        completed_parts: 0,
        total_parts: 0,
        percentage,
        state: session::UploadState::Cancelled,
    };
    manager.notify_external(&fp, &session_agg, &transfer_agg);
}

/// Register a file in the ProgressManager and fire a notification using the actual
/// session state. Files with existing progress are shown as PAUSED; others as INITIALIZED.
pub fn notify_file_registered<N: ProgressNotifier>(manager: &ProgressManager<N>, file_key: &str) {
    let (transfer_id, session_agg, transfer_agg) = aggregate_snapshot_for_file(file_key);
    let sess = session::session();
    let entry = sess.files.get(file_key);
    let total_bytes = entry.map(|e| e.total_bytes).unwrap_or(0);
    let file_name = entry.map(|e| e.file_name.clone()).unwrap_or_default();
    let file_hash = entry.map(|e| e.file_hash.clone()).unwrap_or_default();
    let committed_bytes = entry.map(|e| e.uploaded_bytes).unwrap_or(0);
    let committed_parts = entry.map(|e| e.completed_parts).unwrap_or(0);
    let state = entry
        .map(|e| match e.state.clone() {
            UploadState::Completed => UploadState::Completed,
            // Any non-completed file: show progress if it has some, else INITIALIZED.
            _ => if committed_bytes > 0 { UploadState::Paused } else { UploadState::Initialized },
        })
        .unwrap_or(UploadState::Initialized);
    drop(sess);
    manager.register_not_started(
        file_key.to_string(),
        file_name,
        file_hash,
        transfer_id,
        total_bytes,
        committed_bytes,
        committed_parts,
        state,
        session_agg,
        transfer_agg,
    );
}
