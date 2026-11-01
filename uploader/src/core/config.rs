use std::sync::{Mutex, MutexGuard, OnceLock};

pub struct UploaderConfig {
    /// Called once per file at upload start; returns S3 key, uploadId, partSize.
    pub start_upload_api: String,
    /// Called in batches (up to MAX_CONCURRENT_UPLOADS parts) to get presigned PUT URLs.
    pub get_upload_urls_api: String,
    /// Called once per file when all parts are done.
    pub complete_api: String,
    /// Whether `fileHash` is included in the startUpload request body.
    /// Some backends do not accept (or do not want) the content hash; the hash is
    /// still computed locally for dedup/resume either way.
    pub send_file_hash: bool,
}

static CONFIG: OnceLock<Mutex<UploaderConfig>> = OnceLock::new();

fn config_lock() -> &'static Mutex<UploaderConfig> {
    CONFIG.get_or_init(|| {
        Mutex::new(UploaderConfig {
            start_upload_api: String::new(),
            get_upload_urls_api: String::new(),
            complete_api: String::new(),
            send_file_hash: true,
        })
    })
}

pub fn set_config(
    start_upload_api: &str,
    get_upload_urls_api: &str,
    complete_api: &str,
    send_file_hash: bool,
) {
    let mut cfg = config_lock().lock().unwrap();
    cfg.start_upload_api = start_upload_api.to_string();
    cfg.get_upload_urls_api = get_upload_urls_api.to_string();
    cfg.complete_api = complete_api.to_string();
    cfg.send_file_hash = send_file_hash;
}

pub fn get_config() -> MutexGuard<'static, UploaderConfig> {
    config_lock().lock().unwrap()
}
