use std::collections::{HashMap, HashSet};

use crate::core::api::StartUploadResponse;
use crate::core::session::{self, UploadState};
use crate::core::ChunkUploadResult;

pub enum StartDecision {
    Completed { file_key: String },
    Resume { file_key: String },
    StartNew,
}

pub struct PreparedUpload {
    pub transfer_id: String,
    pub upload_id: String,
    pub part_size: u64,
    pub total_parts: u32,
    pub completed_etags: Vec<(u32, String)>,
    pub done_parts: HashSet<u32>,
    pub remaining_parts: Vec<u32>,
    pub committed_bytes: u64,
}

pub fn start_decision(file_hash: &str) -> StartDecision {
    let sess = session::session();
    match sess.find_by_hash(file_hash) {
        Some(entry) if entry.state == UploadState::Completed => StartDecision::Completed {
            file_key: entry.file_key.clone(),
        },
        Some(entry) if entry.is_resumable() => StartDecision::Resume {
            file_key: entry.file_key.clone(),
        },
        _ => StartDecision::StartNew,
    }
}

pub fn register_started_upload(
    file_hash: String,
    transfer_id: &str,
    file_path: String,
    file_name: String,
    file_size: u64,
    user_params: HashMap<String, String>,
    start_resp: StartUploadResponse,
) -> String {
    let file_key = start_resp.key.clone();
    let total_parts = total_parts(file_size, start_resp.part_size);

    session::session().register_file(
        file_key.clone(),
        file_hash,
        transfer_id.to_string(),
        file_path,
        file_name,
        file_size,
        user_params,
    );
    session::session().set_upload_info(
        &file_key,
        start_resp.upload_id,
        start_resp.part_size,
        total_parts,
    );

    file_key
}

pub fn prepare_upload(file_key: &str, total_bytes: u64) -> Result<PreparedUpload, String> {
    let entry = {
        let sess = session::session();
        sess.files
            .get(file_key)
            .cloned()
            .ok_or_else(|| format!("File entry not found for {}", file_key))?
    };

    let upload_id = entry
        .upload_id
        .ok_or_else(|| format!("Missing upload_id for {}", file_key))?;
    let part_size = entry
        .part_size
        .ok_or_else(|| format!("Missing part_size for {}", file_key))?;
    let total_parts = total_parts(total_bytes, part_size);
    let completed_etags = entry.completed_chunk_etags;
    let done_parts: HashSet<u32> = completed_etags.iter().map(|(p, _)| *p).collect();
    let remaining_parts = (1..=total_parts)
        .filter(|p| !done_parts.contains(p))
        .collect();
    let committed_bytes = completed_bytes(&done_parts, part_size, total_bytes);

    Ok(PreparedUpload {
        transfer_id: entry.transfer_id,
        upload_id,
        part_size,
        total_parts,
        completed_etags,
        done_parts,
        remaining_parts,
        committed_bytes,
    })
}

pub fn total_parts(total_bytes: u64, part_size: u64) -> u32 {
    total_bytes.div_ceil(part_size) as u32
}

pub fn part_size_for(part_number: u32, part_size: u64, total_bytes: u64) -> u64 {
    let start = (part_number as u64 - 1) * part_size;
    total_bytes.saturating_sub(start).min(part_size)
}

pub fn completed_bytes(done_parts: &HashSet<u32>, part_size: u64, total_bytes: u64) -> u64 {
    done_parts
        .iter()
        .map(|&part_number| part_size_for(part_number, part_size, total_bytes))
        .sum()
}

pub fn combine_upload_results(
    completed_etags: Vec<(u32, String)>,
    new_results: Vec<ChunkUploadResult>,
) -> Vec<ChunkUploadResult> {
    let mut all_results: Vec<ChunkUploadResult> = completed_etags
        .into_iter()
        .map(|(part_number, etag)| ChunkUploadResult { part_number, etag })
        .collect();
    all_results.extend(new_results);
    all_results
}
