use std::collections::{HashMap, HashSet};

use crate::core::api::{ApiClient, StartUploadResponse};
use crate::core::session::{self, UploadState};
use crate::core::ChunkUploadResult;

pub enum StartDecision {
    Completed { file_key: String },
    Resume { file_key: String },
    StartNew,
}

pub enum StartResult {
    AlreadyCompleted(String),
    Resumed(String),
    Started(String),
}

impl StartResult {
    pub fn file_key(&self) -> &str {
        match self {
            StartResult::AlreadyCompleted(k) | StartResult::Resumed(k) | StartResult::Started(k) => k,
        }
    }

    pub fn should_upload(&self) -> bool {
        !matches!(self, StartResult::AlreadyCompleted(_))
    }
}

pub struct PreparedUpload {
    pub transfer_id: String,
    pub upload_id: String,
    /// Plaintext bytes per chunk (= server part_size - ciphertext_overhead).
    /// Engines use this for `start_pos` and `chunk_size` calculations.
    pub part_size: u64,
    /// Data parts only (no footer); used for progress display.
    pub total_parts: u32,
    pub completed_etags: Vec<(u32, String)>,
    pub done_parts: HashSet<u32>,
    /// Data + optional footer part; drives `remaining_parts`.
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

pub async fn start_and_register<A: ApiClient>(
    file_hash: String,
    transfer_id: &str,
    file_path: String,
    file_name: String,
    file_size: u64,
    user_params: HashMap<String, String>,
    api: &A,
) -> Result<StartResult, String> {
    match start_decision(&file_hash) {
        StartDecision::Completed { file_key } => Ok(StartResult::AlreadyCompleted(file_key)),
        StartDecision::Resume { file_key } => Ok(StartResult::Resumed(file_key)),
        StartDecision::StartNew => {
            let start_resp = api
                .start_upload(&file_name, &file_hash, file_size, &user_params)
                .await
                .map_err(|e| e.to_string())?;
            let file_key = register_started_upload(
                file_hash, transfer_id, file_path, file_name, file_size, user_params, start_resp,
            );
            session::persist_session();
            Ok(StartResult::Started(file_key))
        }
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

pub fn prepare_upload(file_key: &str, total_bytes: u64, ciphertext_overhead: u64) -> Result<PreparedUpload, String> {
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
    let server_part_size = entry
        .part_size
        .ok_or_else(|| format!("Missing part_size for {}", file_key))?;

    // When AEAD is active, each plaintext chunk is smaller by the tag size so
    // the uploaded ciphertext fits exactly in the presigned-URL slot.
    let effective_part_size = server_part_size.saturating_sub(ciphertext_overhead);
    let total_data_parts = total_parts(total_bytes, effective_part_size);

    let completed_etags = entry.completed_chunk_etags;
    let done_parts: HashSet<u32> = completed_etags.iter().map(|(p, _)| *p).collect();

    // Include footer part (total_data_parts + 1) in remaining_parts when
    // the encryptor adds overhead. The adapter's read_chunk detects this
    // special part number and returns compute_footer() bytes instead of
    // reading from the file.
    let last_part = if ciphertext_overhead > 0 { total_data_parts + 1 } else { total_data_parts };
    let remaining_parts = (1..=last_part)
        .filter(|p| !done_parts.contains(p))
        .collect();

    // Committed bytes: only data parts, not the footer.
    let committed_bytes = done_parts
        .iter()
        .filter(|&&p| p <= total_data_parts)
        .map(|&p| part_size_for(p, effective_part_size, total_bytes))
        .sum();

    Ok(PreparedUpload {
        transfer_id: entry.transfer_id,
        upload_id,
        part_size: effective_part_size,
        total_parts: total_data_parts,
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
