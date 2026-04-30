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
    manager.init(
        file_key.to_string(),
        transfer_id,
        total_bytes,
        total_parts,
        committed_bytes,
        completed_parts,
        session_agg,
        transfer_agg,
    );
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
