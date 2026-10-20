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
            StartResult::AlreadyCompleted(k)
            | StartResult::Resumed(k)
            | StartResult::Started(k) => k,
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
    /// Data parts only — a descriptor, when the scheme has one, rides inside the last
/// data part. Drives `remaining_parts`.
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
                file_hash,
                transfer_id,
                file_path,
                file_name,
                file_size,
                user_params,
                start_resp,
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

pub fn prepare_upload(
    file_key: &str,
    total_bytes: u64,
    ciphertext_overhead: u64,
    has_footer: bool,
    footer_len: u64,
) -> Result<PreparedUpload, String> {
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

    // When AEAD is active, each plaintext chunk is smaller by the tag size so the
    // uploaded ciphertext fits the presigned-URL slot. The descriptor is reserved
    // in every part as well, since it rides inside the last one.
    let reserved = ciphertext_overhead + if has_footer { footer_len } else { 0 };
    let effective_part_size = effective_part_size(server_part_size, ciphertext_overhead, has_footer, footer_len)
        .ok_or_else(|| {
            format!(
                "part_size {} is too small for the active encryptor (overhead={}, \
                 has_footer={}, footer_len={} -> reserved={} bytes)",
                server_part_size, ciphertext_overhead, has_footer, footer_len, reserved
            )
        })?;
    let mut total_data_parts = total_parts(total_bytes, effective_part_size);
    // An empty file still needs one part to carry the descriptor.
    if has_footer && total_data_parts == 0 {
        total_data_parts = 1;
    }

    let completed_etags = entry.completed_chunk_etags;
    let done_parts: HashSet<u32> = completed_etags.iter().map(|(p, _)| *p).collect();

    // Every uploaded part is a data part — the descriptor is appended to the last
    // one by `append_descriptor`, so no extra S3 part exists (see its docs).
    let remaining_parts = (1..=total_data_parts)
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

/// Plaintext bytes that fit one presigned-URL slot once ciphertext expansion and
/// the descriptor reservation are accounted for.
pub fn effective_part_size(
    server_part_size: u64,
    ciphertext_overhead: u64,
    has_footer: bool,
    footer_len: u64,
) -> Option<u64> {
    let reserved = ciphertext_overhead.checked_add(if has_footer { footer_len } else { 0 })?;
    server_part_size.checked_sub(reserved).filter(|v| *v > 0)
}

/// Append the descriptor to the **final data part**.
///
/// The descriptor is deliberately not an S3 part of its own: S3 requires every part
/// except the last to be at least 5 MiB, so a separate 28-byte trailing part would
/// demote the real last part to an intermediate one and CompleteMultipartUpload
/// would fail with `EntityTooSmall` for every file whose final part is below the
/// minimum — which is most files. Riding inside the last part keeps that part last,
/// where any size is allowed.
pub fn append_descriptor(
    out: &mut Vec<u8>,
    enc: &dyn crate::core::encryption::ChunkEncryptor,
    part_number: u32,
    total_data_parts: u32,
    effective_part_size: u64,
    has_footer: bool,
) {
    if has_footer && part_number == total_data_parts {
        if let Some(footer) = enc.compute_footer(total_data_parts, effective_part_size) {
            out.extend_from_slice(&footer);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::encryption::{ChunkEncryptor, XChaCha20Poly1305Encryptor};

    fn encryptor() -> XChaCha20Poly1305Encryptor {
        XChaCha20Poly1305Encryptor::from_hex(&"ab".repeat(32), &"cd".repeat(24)).unwrap()
    }

    /// The layout that broke real uploads against S3: a 36.6 MB file at the server's
    /// 32 MiB part size used to emit data part 2 (3.08 MB) plus a 28-byte descriptor
    /// part, so part 2 stopped being the final part and S3 rejected the completion
    /// with EntityTooSmall (every part but the last must be >= 5 MiB).
    #[test]
    fn descriptor_rides_in_the_last_part_and_every_other_part_clears_5_mib() {
        const S3_MIN_PART: u64 = 5 * 1024 * 1024;
        let server_part_size = 32 * 1024 * 1024u64;
        let file_size = 36_631_089u64;
        let enc = encryptor();
        let (overhead, has_footer) = (enc.ciphertext_overhead() as u64, enc.has_footer());
        let footer_len = enc.footer_len() as u64;

        let effective =
            effective_part_size(server_part_size, overhead, has_footer, footer_len).unwrap();
        assert_eq!(effective, server_part_size - overhead - footer_len);
        let n = total_parts(file_size, effective);
        assert_eq!(n, 2);

        // Build the object exactly as the adapters do: encrypt each chunk, then let
        // append_descriptor add the descriptor to the final part.
        let mut uploaded: Vec<usize> = Vec::new();
        for part in 1..=n {
            let start = (part as u64 - 1) * effective;
            let len = (file_size - start).min(effective) as usize;
            let mut out = enc.encrypt_chunk(&vec![0u8; len], part);
            append_descriptor(&mut out, &enc, part, n, effective, has_footer);
            uploaded.push(out.len());
        }

        assert_eq!(uploaded.len() as u32, n, "no extra descriptor part");
        for (i, size) in uploaded.iter().enumerate() {
            let is_last = i + 1 == uploaded.len();
            assert!(*size as u64 <= server_part_size, "part {} overflows its slot", i + 1);
            if !is_last {
                assert!(
                    *size as u64 >= S3_MIN_PART,
                    "intermediate part {} is below S3's 5 MiB minimum",
                    i + 1
                );
            }
        }
        // The descriptor really is inside the final part.
        let last = *uploaded.last().unwrap();
        assert_eq!(last as u64, (file_size - effective) + overhead + footer_len);
    }

    /// A file that fits in one part stays one part, with the descriptor appended.
    #[test]
    fn single_part_file_gets_the_descriptor() {
        let enc = encryptor();
        let (overhead, has_footer) = (enc.ciphertext_overhead() as u64, enc.has_footer());
        let footer_len = enc.footer_len() as u64;
        let effective = effective_part_size(1024 * 1024, overhead, has_footer, footer_len).unwrap();
        let file_size = 5000u64;
        assert_eq!(total_parts(file_size, effective), 1);

        let mut out = enc.encrypt_chunk(&vec![7u8; file_size as usize], 1);
        append_descriptor(&mut out, &enc, 1, 1, effective, has_footer);
        assert_eq!(out.len() as u64, file_size + overhead + footer_len);
    }

    /// Oversized reservations are reported instead of silently producing empty parts.
    #[test]
    fn impossible_part_size_is_rejected() {
        // Tag and descriptor are both reserved when the scheme has a descriptor.
        assert_eq!(effective_part_size(1024, 16, true, 28), Some(980));
        assert_eq!(effective_part_size(1024, 16, false, 0), Some(1008));
        // A slot that cannot hold the reservation is rejected, not silently clamped.
        assert!(effective_part_size(40, 16, true, 28).is_none());
        assert!(effective_part_size(16, 16, false, 0).is_none());
    }
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
