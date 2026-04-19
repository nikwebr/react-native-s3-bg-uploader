pub mod api;
pub mod config;
pub mod chunk;
pub mod hash;
pub mod progress;
pub mod retry;
pub mod session;

pub const MAX_CONCURRENT_UPLOADS: usize = 4;
pub const MAX_RETRIES: usize = 5;

/// Result of a single multipart chunk upload (ETag + part number).
#[derive(Debug, Clone)]
pub struct ChunkUploadResult {
    pub part_number: u32,
    pub etag: String,
}

/// Cleans ETag values (removes surrounding quotes).
pub fn clean_etag(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}
