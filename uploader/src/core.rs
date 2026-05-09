pub mod api;
pub mod chunk;
pub mod config;
pub mod hash;
pub mod progress;
pub mod retry;
pub mod runtime;
pub mod session;
pub mod upload;
pub mod upload_orchestrator;

pub use chunk::{clean_etag, ChunkUploadResult};

pub const MAX_CONCURRENT_UPLOADS: usize = 4;
pub const MAX_RETRIES: usize = 5;