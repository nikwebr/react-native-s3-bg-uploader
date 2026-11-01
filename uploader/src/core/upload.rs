use std::collections::{HashMap, HashSet};

use crate::core::api::{ApiClient, EncryptionInfo, StartUploadResponse};
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
    /// Bytes already in S3 for completed parts — ciphertext bytes (each part's
    /// plaintext length plus the encryptor's overhead and, for the final part,
    /// the descriptor). This is what progress callbacks report.
    pub committed_bytes: u64,
    /// Exact size of the uploaded object in bytes (ciphertext size for encrypted
    /// uploads). Progress percentages are relative to this value.
    pub total_ciphertext_bytes: u64,
}

/// Exact wire layout of one object, derived from the part size the backend chose.
#[derive(Debug, Clone, Copy)]
pub struct CiphertextLayout {
    /// Plaintext bytes the engine reads per part.
    pub part_size: u64,
    /// Data parts; a descriptor, when present, rides inside the last one.
    pub data_parts: u32,
    /// Exact size of the uploaded object.
    pub total_bytes: u64,
    pub overhead: u64,
    pub has_footer: bool,
    pub footer_len: u64,
}

impl CiphertextLayout {
    /// Ciphertext bytes a single part occupies on the wire.
    pub fn part_len(&self, part_number: u32, plaintext_bytes: u64) -> u64 {
        let plain = part_size_for(part_number, self.part_size, plaintext_bytes);
        let footer = if self.has_footer && part_number == self.data_parts {
            self.footer_len
        } else {
            0
        };
        plain + self.overhead + footer
    }

    /// Ciphertext bytes of all already-completed parts.
    pub fn committed_bytes(&self, done_parts: &HashSet<u32>, plaintext_bytes: u64) -> u64 {
        done_parts
            .iter()
            .filter(|&&p| p >= 1 && p <= self.data_parts)
            .map(|&p| self.part_len(p, plaintext_bytes))
            .sum()
    }
}

/// Format numbers of the encryptor that will actually encrypt a file's chunks.
///
/// These are needed before the backend has chosen a part size (to predict the
/// callback size) and when the file is registered. Keeping them explicit lets the
/// web build pass the numbers of a registered JavaScript encryptor instead of the
/// built-in cipher's.
#[derive(Debug, Clone, Copy)]
pub struct CipherParams {
    pub overhead: u64,
    pub has_footer: bool,
    pub footer_len: u64,
}

impl CipherParams {
    pub fn of(enc: &dyn crate::core::encryption::ChunkEncryptor) -> Self {
        Self {
            overhead: enc.ciphertext_overhead() as u64,
            has_footer: enc.has_footer(),
            footer_len: enc.footer_len() as u64,
        }
    }

    pub fn of_config(config: Option<&ArcEncryptor>) -> Option<Self> {
        config.map(|c| Self::of(c.as_ref()))
    }

    pub fn layout(self, plaintext_bytes: u64, server_part_size: u64) -> Option<CiphertextLayout> {
        ciphertext_layout(
            plaintext_bytes,
            server_part_size,
            self.overhead,
            self.has_footer,
            self.footer_len,
        )
    }
}

/// The wire description sent to `startUpload` for an encrypted upload.
impl From<CipherParams> for EncryptionInfo {
    fn from(params: CipherParams) -> Self {
        Self {
            overhead: params.overhead,
            has_footer: params.has_footer,
            footer_len: params.footer_len,
        }
    }
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
    cipher_params: Option<CipherParams>,
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
            let params =
                cipher_params.or_else(|| CipherParams::of_config(encryption_config.as_ref()));
            // `fileSize` is always the plaintext size of the local file; the
            // encryption details let the backend derive the ciphertext size it will
            // actually store, using the part size it returns for this upload.
            let start_resp = api
                .start_upload(
                    &file_name,
                    &file_hash,
                    file_size,
                    params.map(EncryptionInfo::from),
                    &user_params,
                )
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
                params,
                start_resp,
            );
            session::persist_session();
            Ok(StartResult::Started(file_key))
        }
    }
}

/// S3's minimum size for every part except the last.
pub const S3_MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// Predicted ciphertext size for the smallest legal plaintext part size.
///
/// Used only for local progress before the backend has returned its part size
/// (pending files). The `startUpload` body sends the plaintext size plus the
/// encryption details instead, so the backend computes the stored size itself.
///
/// An intermediate ciphertext part is `part_size + overhead` bytes and S3 requires
/// at least [`S3_MIN_PART_SIZE`], so the smallest legal plaintext part size is
/// `S3_MIN_PART_SIZE - overhead`.
pub fn predicted_ciphertext_bytes(
    plaintext_bytes: u64,
    overhead: u64,
    has_footer: bool,
    footer_len: u64,
) -> u64 {
    let min_part_size = S3_MIN_PART_SIZE.saturating_sub(overhead).max(1);
    let mut data_parts = total_parts(plaintext_bytes, min_part_size);
    if has_footer && data_parts == 0 {
        data_parts = 1;
    }
    plaintext_bytes
        + overhead.saturating_mul(data_parts as u64)
        + if has_footer { footer_len } else { 0 }
}

/// Exact wire layout for the part size the backend returned.
///
/// `None` when that part size cannot hold the encryptor's per-part reservation.
pub fn ciphertext_layout(
    plaintext_bytes: u64,
    server_part_size: u64,
    overhead: u64,
    has_footer: bool,
    footer_len: u64,
) -> Option<CiphertextLayout> {
    let part_size = effective_part_size(server_part_size, overhead, has_footer, footer_len)?;
    let mut data_parts = total_parts(plaintext_bytes, part_size);
    // An empty file still needs one part to carry the descriptor.
    if has_footer && data_parts == 0 {
        data_parts = 1;
    }
    let total_bytes = plaintext_bytes
        + overhead.saturating_mul(data_parts as u64)
        + if has_footer { footer_len } else { 0 };
    Some(CiphertextLayout {
        part_size,
        data_parts,
        total_bytes,
        overhead,
        has_footer,
        footer_len,
    })
}

pub fn register_started_upload(
    file_hash: String,
    transfer_id: &str,
    file_path: String,
    file_name: String,
    file_size: u64,
    user_params: HashMap<String, String>,
    encryption_config: Option<ArcEncryptor>,
    cipher_params: Option<CipherParams>,
    start_resp: StartUploadResponse,
) -> String {
    let file_key = start_resp.key.clone();
    let params = cipher_params.or_else(|| CipherParams::of_config(encryption_config.as_ref()));
    let layout = params.and_then(|p| p.layout(file_size, start_resp.part_size));
    let data_parts = layout
        .map(|l| l.data_parts)
        .unwrap_or_else(|| total_parts(file_size, start_resp.part_size));
    // Progress and the persisted session track the size of the object in S3, so an
    // encrypted file reports its ciphertext size while `plaintext_bytes` keeps the
    // size of the local file (used to read chunks).
    let wire_size = layout.map(|l| l.total_bytes).unwrap_or(file_size);

    // Publish the entry, upload metadata and encryption state under one lock.
    let mut sess = session::session();
    sess.register_file(
        file_key.clone(),
        file_hash,
        transfer_id.to_string(),
        file_path,
        file_name,
        wire_size,
        user_params,
    );
    sess.set_file_sizes(&file_key, wire_size, file_size);
    sess.set_upload_info(
        &file_key,
        start_resp.upload_id,
        start_resp.part_size,
        data_parts,
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
    let layout = ciphertext_layout(
        total_bytes,
        server_part_size,
        ciphertext_overhead,
        has_footer,
        footer_len,
    )
    .ok_or_else(|| {
        format!(
            "part_size {} is too small for the active encryptor (overhead={}, \
             has_footer={}, footer_len={} -> reserved={} bytes)",
            server_part_size, ciphertext_overhead, has_footer, footer_len, reserved
        )
    })?;
    let total_data_parts = layout.data_parts;

    let completed_etags = entry.completed_chunk_etags;
    let done_parts: HashSet<u32> = completed_etags.iter().map(|(p, _)| *p).collect();

    // Every uploaded part is a data part — the descriptor is appended to the last
    // one by `append_descriptor`, so no extra S3 part exists (see its docs).
    let remaining_parts = (1..=total_data_parts)
        .filter(|p| !done_parts.contains(p))
        .collect();

    // Committed bytes are ciphertext bytes (data part plaintext plus the tag and,
    // for the final part, the descriptor) so progress matches what S3 received.
    let committed_bytes = layout.committed_bytes(&done_parts, total_bytes);

    Ok(PreparedUpload {
        transfer_id: entry.transfer_id,
        upload_id,
        part_size: layout.part_size,
        total_parts: total_data_parts,
        completed_etags,
        done_parts,
        remaining_parts,
        committed_bytes,
        total_ciphertext_bytes: layout.total_bytes,
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
                None,
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

            let expected_part_size = if has_footer { 996 } else { 1016 };
            assert_eq!(prepared.part_size, expected_part_size);
            assert_eq!(prepared.total_parts, 3);
            assert_eq!(prepared.remaining_parts, vec![2, 3]);
            assert_eq!(prepared.completed_etags, vec![(1, "etag".into())]);
            // Part 1 is a data part (not the last), so it carries the tag but no footer.
            assert_eq!(prepared.committed_bytes, expected_part_size + 8);
            assert_eq!(
                prepared.total_ciphertext_bytes,
                2500 + 8 * 3 + if has_footer { 20 } else { 0 }
            );
            assert_eq!(
                session::session().files[&key].total_bytes,
                prepared.total_ciphertext_bytes
            );
            assert_eq!(session::session().files[&key].plaintext_bytes, 2500);
            session::session().cancel_file(&key);

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
                None,
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
            let committed = if encrypted {
                prepared.part_size + 16 // ciphertext of part 1 (tag, no footer)
            } else {
                prepared.part_size
            };
            assert_eq!(prepared.committed_bytes, committed);
            assert_eq!(
                prepared.total_ciphertext_bytes,
                if encrypted { 1500 + 32 + 28 } else { 1500 }
            );
            session::session().files.remove(&key);
        }
    }

    #[test]
    fn start_upload_body_lets_the_backend_compute_the_stored_size() {
        let config: ArcEncryptor = std::sync::Arc::new(encryptor());
        let params = CipherParams::of_config(Some(&config)).unwrap();

        for plaintext in [0u64, 31 * 1024 * 1024 + 123] {
            let body = crate::core::api::start_upload_body(
                "f.bin",
                "hash",
                plaintext,
                false,
                Some(params.into()),
                &HashMap::new(),
            )
            .unwrap();
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();

            // `fileSize` is the unencrypted size, always.
            assert_eq!(json["fileSize"].as_u64().unwrap(), plaintext);
            let overhead = json["encryption"]["overhead"].as_u64().unwrap();
            let has_footer = json["encryption"]["hasFooter"].as_bool().unwrap();
            let footer_len = json["encryption"]["footerLen"].as_u64().unwrap();

            // The backend knows the part size it will return, so these numbers are
            // enough for it to derive the exact stored (ciphertext) size.
            for server_part_size in [5 * 1024 * 1024 + 28, 32 * 1024 * 1024, 1024 * 1024] {
                let reserved = overhead + if has_footer { footer_len } else { 0 };
                let effective = server_part_size - reserved;
                let mut parts = total_parts(plaintext, effective);
                if has_footer && parts == 0 {
                    parts = 1;
                }
                let backend_total =
                    plaintext + overhead * parts as u64 + if has_footer { footer_len } else { 0 };
                let layout = ciphertext_layout(
                    plaintext,
                    server_part_size,
                    overhead,
                    has_footer,
                    footer_len,
                )
                .unwrap();
                assert_eq!(
                    backend_total, layout.total_bytes,
                    "backend derivation diverges for part size {server_part_size}"
                );
            }
        }
    }

    #[test]
    fn exact_layout_matches_every_uploaded_part() {
        let enc = encryptor();
        let config: ArcEncryptor = std::sync::Arc::new(enc);
        let plaintext = 12_345_678u64;
        let server_part_size = 4 * 1024 * 1024;
        let layout = ciphertext_layout(
            plaintext,
            server_part_size,
            config.ciphertext_overhead() as u64,
            config.has_footer(),
            config.footer_len() as u64,
        )
        .unwrap();

        let mut uploaded = 0u64;
        for part in 1..=layout.data_parts {
            let plain_len = part_size_for(part, layout.part_size, plaintext) as usize;
            uploaded += config.encrypt_chunk(&vec![0; plain_len], part).len() as u64;
            uploaded += if layout.has_footer && part == layout.data_parts {
                config
                    .compute_footer(layout.data_parts, layout.part_size)
                    .unwrap()
                    .len() as u64
            } else {
                0
            };
        }
        assert_eq!(uploaded, layout.total_bytes);

        let done: HashSet<u32> = (1..=layout.data_parts).collect();
        assert_eq!(layout.committed_bytes(&done, plaintext), layout.total_bytes);
    }

    #[test]
    fn empty_encrypted_file_has_a_descriptor_only_part() {
        let enc = encryptor();
        let layout = ciphertext_layout(0, 1024, 16, true, 28).unwrap();
        assert_eq!(layout.data_parts, 1);
        assert_eq!(layout.total_bytes, 16 + 28);
        assert_eq!(layout.part_len(1, 0), 16 + 28);
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
