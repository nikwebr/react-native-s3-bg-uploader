use std::collections::{HashMap, HashSet};

use crate::core::api::{ApiClient, StartUploadResponse};
use crate::core::encryption::ArcEncryptor;
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
    encryption_config: Option<ArcEncryptor>,
    api: &A,
) -> Result<StartResult, String> {
    match start_decision(&file_hash) {
        StartDecision::Completed { file_key } => Ok(StartResult::AlreadyCompleted(file_key)),
        StartDecision::Resume { file_key } => {
            if let Some(config) = encryption_config {
                session::session().update_encryption_config(&file_key, config);
            }
            Ok(StartResult::Resumed(file_key))
        }
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
                encryption_config,
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
    encryption_config: Option<ArcEncryptor>,
    start_resp: StartUploadResponse,
) -> String {
    let file_key = start_resp.key.clone();
    let total_parts = total_parts(file_size, start_resp.part_size);

    // Publish the entry, upload metadata and encryption state under one lock.
    let mut sess = session::session();
    sess.register_file(
        file_key.clone(),
        file_hash,
        transfer_id.to_string(),
        file_path,
        file_name,
        file_size,
        user_params,
    );
    sess.set_upload_info(
        &file_key,
        start_resp.upload_id,
        start_resp.part_size,
        total_parts,
    );
    if let Some(config) = encryption_config {
        sess.update_encryption_config(&file_key, config);
    }

    file_key
}

pub fn prepare_upload(
    file_key: &str,
    total_bytes: u64,
    ciphertext_overhead: u64,
    has_footer: bool,
    footer_len: u64,
    encryption_config: Option<&ArcEncryptor>,
) -> Result<PreparedUpload, String> {
    let entry = {
        let sess = session::session();
        sess.files
            .get(file_key)
            .cloned()
            .ok_or_else(|| format!("File entry not found for {}", file_key))?
    };

    entry.require_encryption_config(encryption_config)?;

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

    #[derive(Debug)]
    struct CustomLayout {
        has_footer: bool,
    }

    impl ChunkEncryptor for CustomLayout {
        fn encrypt_chunk(&self, data: &[u8], _: u32) -> Vec<u8> {
            let mut out = data.to_vec();
            out.resize(data.len() + self.ciphertext_overhead(), 0);
            out
        }

        fn ciphertext_overhead(&self) -> usize {
            8
        }

        fn has_footer(&self) -> bool {
            self.has_footer
        }

        fn footer_len(&self) -> usize {
            if self.has_footer {
                20
            } else {
                0
            }
        }

        fn compute_footer(&self, _: u32, _: u64) -> Option<Vec<u8>> {
            self.has_footer.then(|| vec![0; self.footer_len()])
        }
    }

    #[test]
    fn prepare_upload_uses_custom_overhead_and_optional_footer() {
        for has_footer in [false, true] {
            let key = format!("custom-layout-{has_footer}");
            let config: ArcEncryptor = std::sync::Arc::new(CustomLayout { has_footer });
            register_started_upload(
                key.clone(),
                &key,
                "file".into(),
                "file".into(),
                2500,
                HashMap::new(),
                Some(config.clone()),
                StartUploadResponse {
                    key: key.clone(),
                    upload_id: "upload-id".into(),
                    part_size: 1024,
                },
            );
            session::session()
                .files.get_mut(&key).unwrap()
                .completed_chunk_etags = vec![(1, "etag".into())];
            let prepared = prepare_upload(
                &key,
                2500,
                config.ciphertext_overhead() as u64,
                config.has_footer(),
                config.footer_len() as u64,
                Some(&config),
            )
            .unwrap();
            session::session().cancel_file(&key);

            let expected_part_size = if has_footer { 996 } else { 1016 };
            assert_eq!(prepared.part_size, expected_part_size);
            assert_eq!(prepared.total_parts, 3);
            assert_eq!(prepared.remaining_parts, vec![2, 3]);
            assert_eq!(prepared.completed_etags, vec![(1, "etag".into())]);
            assert_eq!(prepared.committed_bytes, expected_part_size);

            for part in 1..=prepared.total_parts {
                let plain_len = part_size_for(part, prepared.part_size, 2500) as usize;
                let mut out = config.encrypt_chunk(&vec![0; plain_len], part);
                append_descriptor(
                    &mut out,
                    config.as_ref(),
                    part,
                    prepared.total_parts,
                    prepared.part_size,
                    has_footer,
                );
                let footer_len = if has_footer && part == prepared.total_parts {
                    20
                } else {
                    0
                };
                assert_eq!(out.len(), plain_len + 8 + footer_len);
                assert!(out.len() <= 1024);
            }
        }
    }

    #[test]
    fn registration_publishes_encryption_before_the_first_session_snapshot() {
        for encrypted in [false, true] {
            let key = format!("atomic-encryption-registration-{encrypted}");
            let config = encrypted.then(|| -> ArcEncryptor { std::sync::Arc::new(encryptor()) });
            register_started_upload(
                key.clone(),
                &key,
                "file".into(),
                "file".into(),
                2500,
                HashMap::new(),
                config.clone(),
                StartUploadResponse {
                    key: key.clone(),
                    upload_id: "upload-id".into(),
                    part_size: 1024,
                },
            );
            let (entry, snapshot) = {
                let sess = session::session();
                (sess.files[&key].clone(), sess.to_json().unwrap())
            };
            session::session().cancel_file(&key);

            assert_eq!(entry.encrypted, encrypted);
            assert_eq!(entry.upload_id.as_deref(), Some("upload-id"));
            assert_eq!(entry.part_size, Some(1024));
            match (entry.encryption_config.as_ref(), config.as_ref()) {
                (Some(actual), Some(expected)) => assert!(std::sync::Arc::ptr_eq(actual, expected)),
                (None, None) => {}
                _ => panic!("registration lost the upload's encryption configuration"),
            }

            let restored = session::Session::from_json(&snapshot).unwrap();
            let entry = &restored.files[&key];
            assert_eq!(entry.encrypted, encrypted);
            assert!(entry.encryption_config.is_none());
            if encrypted {
                assert!(entry
                    .require_encryption_config(None)
                    .unwrap_err()
                    .contains("encryptAndUploadFile"));
            } else {
                assert!(entry.require_encryption_config(None).is_ok());
            }
        }
    }

    #[test]
    fn provisioned_encrypted_and_plain_uploads_keep_their_resume_layout() {
        for encrypted in [false, true] {
            let key = format!("resume-layout-{encrypted}");
            let config = encrypted.then(|| -> ArcEncryptor { std::sync::Arc::new(encryptor()) });
            let mut entry = session::FileEntry::new(
                key.clone(),
                key.clone(),
                key.clone(),
                "file".into(),
                "file".into(),
                1500,
                HashMap::new(),
            );
            entry.encrypted = encrypted;
            entry.encryption_config = config.clone();
            entry.upload_id = Some("upload-id".into());
            entry.part_size = Some(1024);
            entry.completed_chunk_etags = vec![(1, "etag".into())];
            session::session().files.insert(key.clone(), entry);

            let prepared = prepare_upload(
                &key,
                1500,
                if encrypted { 16 } else { 0 },
                encrypted,
                if encrypted { 28 } else { 0 },
                config.as_ref(),
            )
            .unwrap();
            assert_eq!(prepared.upload_id, "upload-id");
            assert_eq!(prepared.part_size, if encrypted { 980 } else { 1024 });
            assert_eq!(prepared.total_parts, 2);
            assert_eq!(prepared.remaining_parts, vec![2]);
            assert_eq!(prepared.completed_etags, vec![(1, "etag".into())]);
            assert_eq!(prepared.committed_bytes, prepared.part_size);
            session::session().files.remove(&key);
        }
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
