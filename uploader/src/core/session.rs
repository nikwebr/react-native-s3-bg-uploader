use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

// ---------------------------------------------------------------------------
// State enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UploadState {
    NotStarted,
    Running,
    Paused,
    Completed,
    Failed,
}

impl UploadState {
    pub fn as_str(&self) -> &'static str {
        match self {
            UploadState::NotStarted => "NOT_STARTED",
            UploadState::Running => "RUNNING",
            UploadState::Paused => "PAUSED",
            UploadState::Completed => "COMPLETED",
            UploadState::Failed => "FAILED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GlobalUploaderState {
    NotStarted,
    RunningInBg,
    Running,
    Paused,
    Completed,
    Failed,
}

impl GlobalUploaderState {
    pub fn as_str(&self) -> &'static str {
        match self {
            GlobalUploaderState::NotStarted => "NOT_STARTED",
            GlobalUploaderState::RunningInBg => "RUNNING_IN_BG",
            GlobalUploaderState::Running => "RUNNING",
            GlobalUploaderState::Paused => "PAUSED",
            GlobalUploaderState::Completed => "COMPLETED",
            GlobalUploaderState::Failed => "FAILED",
        }
    }
}

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// S3 key returned by startUploadApi — the public identifier for this file.
    pub file_key: String,
    /// SHA-256(transferId || fileContent) — internal only, used for dedup/resume detection.
    pub file_hash: String,
    pub transfer_id: String,
    /// Filesystem path (iOS) or empty string (Android uses fd at runtime).
    pub file_path: String,
    pub file_name: String,
    pub state: UploadState,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub completed_parts: u32,
    pub total_parts: u32,
    /// Multipart upload ID from startUploadApi; needed to resume.
    pub upload_id: Option<String>,
    pub part_size: Option<u64>,
    /// ETags for already-confirmed parts; used to skip on resume.
    pub completed_chunk_etags: Vec<(u32, String)>,
    #[serde(default)]
    pub run_version: u64,
    /// Extra params forwarded to startUploadApi.
    pub user_params: HashMap<String, String>,
}

impl FileEntry {
    pub fn new(
        file_key: String,
        file_hash: String,
        transfer_id: String,
        file_path: String,
        file_name: String,
        total_bytes: u64,
        user_params: HashMap<String, String>,
    ) -> Self {
        Self {
            file_key,
            file_hash,
            transfer_id,
            file_path,
            file_name,
            state: UploadState::NotStarted,
            total_bytes,
            uploaded_bytes: 0,
            completed_parts: 0,
            total_parts: 0,
            upload_id: None,
            part_size: None,
            completed_chunk_etags: Vec::new(),
            run_version: 0,
            user_params,
        }
    }

    pub fn percentage(&self) -> f64 {
        if self.state == UploadState::Completed {
            return 100.0;
        }
        if self.total_bytes == 0 {
            return 0.0;
        }
        ((self.uploaded_bytes as f64 / self.total_bytes as f64) * 100.0).min(100.0)
    }

    pub fn is_resumable(&self) -> bool {
        matches!(
            self.state,
            UploadState::NotStarted
                | UploadState::Failed
                | UploadState::Paused
                | UploadState::Running
        ) && self.upload_id.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEntry {
    pub transfer_id: String,
    /// File keys in FIFO order.
    pub file_keys: Vec<String>,
}

// ---------------------------------------------------------------------------
// Aggregate progress
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AggregateProgress {
    pub percentage: f64,
    pub total_size: u64,
    pub uploaded_size: u64,
    /// None when scoped to a single transfer.
    pub total_transfers: Option<u32>,
    pub completed_transfers: Option<u32>,
    pub total_files: u32,
    pub completed_files: u32,
    pub state: GlobalUploaderState,
}

impl AggregateProgress {
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "percentage": self.percentage,
            "totalSize": self.total_size,
            "uploadedSize": self.uploaded_size,
            "totalFiles": self.total_files,
            "completedFiles": self.completed_files,
            "state": self.state.as_str(),
        });
        if let Some(t) = self.total_transfers {
            v["totalTransfers"] = serde_json::Value::from(t);
        }
        if let Some(c) = self.completed_transfers {
            v["completedTransfers"] = serde_json::Value::from(c);
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Per-file progress snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FileProgress {
    pub file_key: String,
    pub transfer_id: String,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub completed_parts: u32,
    pub total_parts: u32,
    pub percentage: f64,
    pub state: UploadState,
}

impl FileProgress {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "fileKey": self.file_key,
            "transferId": self.transfer_id,
            "totalBytes": self.total_bytes,
            "uploadedBytes": self.uploaded_bytes,
            "completedParts": self.completed_parts,
            "totalParts": self.total_parts,
            "percentage": self.percentage,
            "state": self.state.as_str(),
        })
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    /// fileKey (S3 key) → FileEntry — the primary index.
    pub files: HashMap<String, FileEntry>,
    /// fileHash (SHA-256) → fileKey — used for dedup/resume lookup.
    pub hash_to_key: HashMap<String, String>,
    /// Insertion-ordered transfer registry.
    pub transfers: IndexMap<String, TransferEntry>,
    pub global_state: GlobalUploaderState,
    pub current_transfer_id: Option<String>,
    pub title_template: String,
    pub subtitle_template: String,
}

impl Session {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            hash_to_key: HashMap::new(),
            transfers: IndexMap::new(),
            global_state: GlobalUploaderState::NotStarted,
            current_transfer_id: None,
            title_template: String::new(),
            subtitle_template: String::new(),
        }
    }

    // ------------------------------------------------------------------
    // Dedup lookup
    // ------------------------------------------------------------------

    /// Returns the existing fileKey for a given SHA-256 hash, if already in session.
    pub fn find_by_hash(&self, file_hash: &str) -> Option<&FileEntry> {
        let key = self.hash_to_key.get(file_hash)?;
        self.files.get(key)
    }

    // ------------------------------------------------------------------
    // Enqueueing
    // ------------------------------------------------------------------

    /// Register a new file entry (after startUploadApi returned the fileKey).
    /// Call only when `find_by_hash` returned None (or returned Completed).
    pub fn register_file(
        &mut self,
        file_key: String,
        file_hash: String,
        transfer_id: String,
        file_path: String,
        file_name: String,
        total_bytes: u64,
        user_params: HashMap<String, String>,
    ) {
        let entry = FileEntry::new(
            file_key.clone(),
            file_hash.clone(),
            transfer_id.clone(),
            file_path,
            file_name,
            total_bytes,
            user_params,
        );
        self.hash_to_key.insert(file_hash, file_key.clone());
        self.files.insert(file_key.clone(), entry);

        let transfer = self
            .transfers
            .entry(transfer_id)
            .or_insert_with_key(|id| TransferEntry {
                transfer_id: id.clone(),
                file_keys: Vec::new(),
            });
        transfer.file_keys.push(file_key);
    }

    /// Update file_path for a FAILED/PAUSED entry when the user re-selects the file.
    pub fn update_file_path(&mut self, file_key: &str, file_path: String) {
        if let Some(entry) = self.files.get_mut(file_key) {
            entry.file_path = file_path;
        }
    }

    // ------------------------------------------------------------------
    // Queue management
    // ------------------------------------------------------------------

    /// Pick the next file that should be uploaded.
    /// Prioritises files in `current_transfer_id`, then advances FIFO.
    pub fn next_pending_file(&mut self) -> Option<String> {
        if let Some(ref tid) = self.current_transfer_id.clone() {
            if let Some(key) = self.next_pending_in_transfer(tid) {
                return Some(key);
            }
        }
        let transfer_ids: Vec<String> = self.transfers.keys().cloned().collect();
        for tid in &transfer_ids {
            if let Some(key) = self.next_pending_in_transfer(tid) {
                self.current_transfer_id = Some(tid.clone());
                return Some(key);
            }
        }
        None
    }

    fn next_pending_in_transfer(&self, transfer_id: &str) -> Option<String> {
        let transfer = self.transfers.get(transfer_id)?;
        for key in &transfer.file_keys {
            if let Some(entry) = self.files.get(key) {
                if matches!(
                    entry.state,
                    UploadState::NotStarted | UploadState::Failed | UploadState::Paused
                ) {
                    return Some(key.clone());
                }
            }
        }
        None
    }

    // ------------------------------------------------------------------
    // State mutations
    // ------------------------------------------------------------------

    pub fn mark_file_state(&mut self, file_key: &str, state: UploadState) {
        if let Some(entry) = self.files.get_mut(file_key) {
            entry.state = state;
        }
        self.recompute_global_state();
    }

    pub fn set_upload_info(
        &mut self,
        file_key: &str,
        upload_id: String,
        part_size: u64,
        total_parts: u32,
    ) {
        if let Some(entry) = self.files.get_mut(file_key) {
            entry.upload_id = Some(upload_id);
            entry.part_size = Some(part_size);
            entry.total_parts = total_parts;
        }
    }

    pub fn run_version(&self, file_key: &str) -> Option<u64> {
        self.files.get(file_key).map(|entry| entry.run_version)
    }

    pub fn bump_run_version(&mut self, file_key: &str) -> Option<u64> {
        let entry = self.files.get_mut(file_key)?;
        entry.run_version = entry.run_version.saturating_add(1);
        Some(entry.run_version)
    }

    pub fn set_file_uploaded_bytes(&mut self, file_key: &str, uploaded_bytes: u64) {
        if let Some(entry) = self.files.get_mut(file_key) {
            entry.uploaded_bytes = uploaded_bytes.min(entry.total_bytes);
        }
    }

    pub fn complete_chunk(&mut self, file_key: &str, part_number: u32, etag: String, size: u64) {
        if let Some(entry) = self.files.get_mut(file_key) {
            if !entry
                .completed_chunk_etags
                .iter()
                .any(|(p, _)| *p == part_number)
            {
                entry.completed_chunk_etags.push((part_number, etag));
                entry.completed_parts += 1;
                entry.uploaded_bytes = (entry.uploaded_bytes + size).min(entry.total_bytes);
            }
        }
    }

    pub fn recompute_global_state(&mut self) {
        if self.files.is_empty() {
            self.global_state = GlobalUploaderState::NotStarted;
            return;
        }
        let states: Vec<_> = self.files.values().map(|f| f.state.clone()).collect();
        if states.iter().all(|s| *s == UploadState::Completed) {
            self.global_state = GlobalUploaderState::Completed;
        } else if states.iter().any(|s| *s == UploadState::Running) {
            self.global_state = GlobalUploaderState::Running;
        } else if states.iter().any(|s| *s == UploadState::Failed) {
            self.global_state = GlobalUploaderState::Failed;
        } else if states.iter().any(|s| *s == UploadState::Paused)
            && states
                .iter()
                .all(|s| matches!(s, UploadState::Paused | UploadState::Completed | UploadState::NotStarted))
        {
            self.global_state = GlobalUploaderState::Paused;
        } else {
            self.global_state = GlobalUploaderState::NotStarted;
        }
    }

    // ------------------------------------------------------------------
    // Cancel
    // ------------------------------------------------------------------

    pub fn cancel_file(&mut self, file_key: &str) {
        if let Some(entry) = self.files.remove(file_key) {
            self.hash_to_key.remove(&entry.file_hash);
        }
        for transfer in self.transfers.values_mut() {
            transfer.file_keys.retain(|k| k != file_key);
        }
        self.recompute_global_state();
    }

    pub fn cancel_transfer(&mut self, transfer_id: &str) {
        if let Some(transfer) = self.transfers.get(transfer_id) {
            let keys: Vec<String> = transfer.file_keys.clone();
            for key in keys {
                if let Some(entry) = self.files.remove(&key) {
                    self.hash_to_key.remove(&entry.file_hash);
                }
            }
        }
        self.transfers.shift_remove(transfer_id);
        if self.current_transfer_id.as_deref() == Some(transfer_id) {
            self.current_transfer_id = None;
        }
        self.recompute_global_state();
    }

    pub fn cancel_all(&mut self) {
        self.files.clear();
        self.hash_to_key.clear();
        self.transfers.clear();
        self.current_transfer_id = None;
        self.global_state = GlobalUploaderState::NotStarted;
    }

    pub fn pause_all(&mut self) {
        for entry in self.files.values_mut() {
            if entry.state == UploadState::Running {
                entry.state = UploadState::Paused;
            }
        }
        self.recompute_global_state();
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub fn get_progress(
        &self,
        transfer_id: Option<&str>,
        file_key: Option<&str>,
    ) -> Vec<FileProgress> {
        self.files
            .values()
            .filter(|e| {
                transfer_id.map_or(true, |t| e.transfer_id == t)
                    && file_key.map_or(true, |k| e.file_key == k)
            })
            .map(|e| FileProgress {
                file_key: e.file_key.clone(),
                transfer_id: e.transfer_id.clone(),
                total_bytes: e.total_bytes,
                uploaded_bytes: e.uploaded_bytes,
                completed_parts: e.completed_parts,
                total_parts: e.total_parts,
                percentage: e.percentage(),
                state: e.state.clone(),
            })
            .collect()
    }

    pub fn get_aggregate_progress(&self, transfer_id: Option<&str>) -> AggregateProgress {
        let entries: Vec<&FileEntry> = self
            .files
            .values()
            .filter(|e| transfer_id.map_or(true, |t| e.transfer_id == t))
            .collect();

        let total_size: u64 = entries.iter().map(|e| e.total_bytes).sum();
        let raw_uploaded_size: u64 = entries.iter().map(|e| e.uploaded_bytes).sum();
        let total_files = entries.len() as u32;
        let completed_files = entries
            .iter()
            .filter(|e| e.state == UploadState::Completed)
            .count() as u32;

        let (total_transfers, completed_transfers) = if transfer_id.is_none() {
            let total = self.transfers.len() as u32;
            let completed = self
                .transfers
                .values()
                .filter(|t| {
                    t.file_keys.iter().all(|k| {
                        self.files
                            .get(k)
                            .map_or(false, |e| e.state == UploadState::Completed)
                    })
                })
                .count() as u32;
            (Some(total), Some(completed))
        } else {
            (None, None)
        };

        let state = if entries.is_empty() {
            GlobalUploaderState::NotStarted
        } else if entries.iter().all(|e| e.state == UploadState::Completed) {
            GlobalUploaderState::Completed
        } else if entries.iter().any(|e| e.state == UploadState::Running) {
            GlobalUploaderState::Running
        } else if entries.iter().any(|e| e.state == UploadState::Failed) {
            GlobalUploaderState::Failed
        } else if entries
            .iter()
            .all(|e| matches!(e.state, UploadState::Paused | UploadState::Completed))
        {
            GlobalUploaderState::Paused
        } else {
            self.global_state.clone()
        };

        // When all files are done, ensure percentage and uploaded_size reflect 100%.
        let (percentage, uploaded_size) = if state == GlobalUploaderState::Completed {
            (100.0, total_size)
        } else {
            let pct = if total_size == 0 {
                0.0
            } else {
                ((raw_uploaded_size as f64 / total_size as f64) * 100.0).min(100.0)
            };
            (pct, raw_uploaded_size)
        };

        AggregateProgress {
            percentage,
            total_size,
            uploaded_size,
            total_transfers,
            completed_transfers,
            total_files,
            completed_files,
            state,
        }
    }

    // ------------------------------------------------------------------
    // Title / subtitle formatting
    // ------------------------------------------------------------------

    pub fn format_title(&self) -> String {
        if self.title_template.is_empty() {
            return String::new();
        }
        format_template(&self.title_template, &self.get_aggregate_progress(None))
    }

    pub fn format_subtitle(&self) -> String {
        if self.subtitle_template.is_empty() {
            return String::new();
        }
        format_template(&self.subtitle_template, &self.get_aggregate_progress(None))
    }

    // ------------------------------------------------------------------
    // Persistence helpers (used by redb layer below)
    // ------------------------------------------------------------------

    pub fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }

    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

// ---------------------------------------------------------------------------
// Template formatting
// ---------------------------------------------------------------------------

pub fn format_template(template: &str, agg: &AggregateProgress) -> String {
    let mut s = template.to_string();
    s = s.replace("{percentage}", &format!("{:.0}%", agg.percentage));
    s = s.replace("{totalSize}", &human_bytes(agg.total_size));
    s = s.replace("{uploadedSize}", &human_bytes(agg.uploaded_size));
    if let Some(t) = agg.total_transfers {
        s = s.replace("{totalTransfers}", &t.to_string());
    }
    if let Some(c) = agg.completed_transfers {
        s = s.replace("{completedTransfers}", &c.to_string());
    }
    s = s.replace("{totalFiles}", &agg.total_files.to_string());
    s = s.replace("{completedFiles}", &agg.completed_files.to_string());
    s
}

pub fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ---------------------------------------------------------------------------
// Global session singleton
// ---------------------------------------------------------------------------

pub trait SessionStore: Send + Sync {
    fn load(&self) -> Option<Session>;
    fn save(&self, json: &str);
    fn clear(&self);
    fn set_storage_path(&self, _path: &str) {}
}

static STORE: OnceLock<Box<dyn SessionStore>> = OnceLock::new();
static SESSION: OnceLock<Mutex<Session>> = OnceLock::new();

pub fn register_store(store: impl SessionStore + 'static) {
    let _ = STORE.set(Box::new(store));
}

pub fn session() -> MutexGuard<'static, Session> {
    SESSION
        .get_or_init(|| {
            let initial = STORE.get()
                .and_then(|s| s.load())
                .unwrap_or_else(Session::new);
            Mutex::new(initial)
        })
        .lock()
        .unwrap()
}

pub fn persist_session() {
    if let Some(store) = STORE.get() {
        if let Some(json) = session().to_json() {
            store.save(&json);
        }
    }
}

pub fn cancel_file(key: &str) {
    session().cancel_file(key);
    persist_session();
}

pub fn cancel_transfer(tid: &str) {
    session().cancel_transfer(tid);
    persist_session();
}

pub fn pause_all() {
    session().pause_all();
    persist_session();
}

pub fn resume_all() {
    let file_keys: Vec<String> = session()
        .files
        .values()
        .filter(|e| e.state == UploadState::Paused || e.state == UploadState::Failed)
        .map(|e| e.file_key.clone())
        .collect();
    for key in &file_keys {
        session().bump_run_version(key);
        session().mark_file_state(key, UploadState::NotStarted);
    }
    persist_session();
}

pub fn file_run_version(file_key: &str) -> Option<u64> {
    session().run_version(file_key)
}

pub fn is_current_run(file_key: &str, run_version: u64) -> bool {
    file_run_version(file_key) == Some(run_version)
}

pub fn clear_session() {
    session().cancel_all();
    if let Some(store) = STORE.get() {
        store.clear();
    }
}

pub fn set_storage_path(path: &str) {
    if let Some(store) = STORE.get() {
        store.set_storage_path(path);
    }
}
