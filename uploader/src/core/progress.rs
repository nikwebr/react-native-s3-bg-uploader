use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct UploadProgress {
    pub total_bytes: u64,
    pub total_parts: u32,
    pub completed_bytes: u64, // only considers completed chunks
    pub in_flight_progress: HashMap<u32, u64>, // part_number -> uploaded bytes
    pub completed_parts: u32
}

impl UploadProgress {
    fn new(total_bytes: u64, total_parts: u32) -> Self {
        Self {
            total_bytes,
            completed_bytes: 0,
            in_flight_progress: HashMap::new(),
            completed_parts: 0,
            total_parts,
        }
    }

    // considers completed & in-flight chunks
    pub fn uploaded_bytes(&self) -> u64 {
        let in_flight_sum: u64 = self.in_flight_progress.values().sum();
        self.completed_bytes + in_flight_sum
    }

    // considers completed & in-flight chunks
    pub fn percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.uploaded_bytes() as f64 / self.total_bytes as f64) * 100.0
    }

    fn update_in_flight(&mut self, part_number: u32, bytes_uploaded: u64) {
        self.in_flight_progress.insert(part_number, bytes_uploaded);
    }

    fn complete_chunk(&mut self, part_number: u32, chunk_size: u64) {
        // Entferne aus in-flight und addiere zu completed_bytes
        self.in_flight_progress.remove(&part_number);
        self.completed_bytes += chunk_size;
        self.completed_parts += 1;
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "totalBytes": self.total_bytes,
            "uploadedBytes": self.uploaded_bytes(),
            "completedParts": self.completed_parts,
            "totalParts": self.total_parts,
            "percentage": self.percentage()
        })
    }
}

/// Trait für Progress-Benachrichtigungen
/// Ermöglicht plattformspezifische Implementierungen
pub trait ProgressNotifier: Send + Sync {
    fn notify(&self, progress: &UploadProgress);
}

/// Dummy-Notifier für Tests oder wenn kein Callback gesetzt ist
pub struct NoOpNotifier;

impl ProgressNotifier for NoOpNotifier {
    fn notify(&self, _progress: &UploadProgress) {}
}

pub struct ProgressManager<N: ProgressNotifier> {
    notifier: N,
    progress: Mutex<Option<UploadProgress>>,
}

impl<N: ProgressNotifier> ProgressManager<N> {
    pub fn new(notifier: N) -> Self {
        Self {
            notifier,
            progress: Mutex::new(None),
        }
    }

    pub fn init(&self, total_bytes: u64, total_parts: u32) {
        let mut progress = self.progress.lock().unwrap();
        *progress = Some(UploadProgress::new(total_bytes, total_parts));
        if let Some(ref p) = *progress {
            let snapshot = p.clone();
            drop(progress);
            self.notifier.notify(&snapshot);
        }
    }

    pub fn update_in_flight(&self, part_number: u32, bytes_uploaded: u64) {
        let mut progress = self.progress.lock().unwrap();
        if let Some(ref mut p) = *progress {
            p.update_in_flight(part_number, bytes_uploaded);
            let snapshot = p.clone();
            drop(progress);
            self.notifier.notify(&snapshot);
        }
    }

    pub fn complete_chunk(&self, part_number: u32, chunk_size: u64) {
        let mut progress = self.progress.lock().unwrap();
        if let Some(ref mut p) = *progress {
            p.complete_chunk(part_number, chunk_size);
            let snapshot = p.clone();
            drop(progress);
            self.notifier.notify(&snapshot);
        }
    }

    pub fn percentage(&self) -> f64 {
        let progress = self.progress.lock().unwrap();
        progress.as_ref().map(|p| p.percentage()).unwrap_or(0.0)
    }

    pub fn to_json(&self) -> Option<serde_json::Value> {
        let progress = self.progress.lock().unwrap();
        progress.as_ref().map(|p| p.to_json())
    }

    pub fn snapshot(&self) -> Option<UploadProgress> {
        let progress = self.progress.lock().unwrap();
        progress.clone()
    }
}