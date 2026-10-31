use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::core::chunk::ChunkInfo;
use crate::core::upload;
use crate::core::{ChunkUploadResult, MAX_CONCURRENT_UPLOADS};

#[derive(Clone, Copy)]
pub enum BlockingPrefetch {
    AllUpfront,
    Rolling,
}

#[derive(Clone, Copy)]
pub struct BlockingEngineConfig {
    pub prefetch: BlockingPrefetch,
    pub fail_fast: bool,
}

pub trait BlockingWorker: Send {
    fn read_chunk(&self, chunk: &ChunkInfo) -> Result<Vec<u8>, String>;
    fn upload_chunk(
        &self,
        url: &str,
        data: &[u8],
        part_number: u32,
        file_key: &str,
    ) -> Result<String, String>;
}

pub trait BlockingPlatformAdapter: Send + Sync + 'static {
    fn is_paused(&self) -> bool {
        false
    }

    fn is_current_run(&self) -> bool {
        true
    }

    fn fetch_urls(&self, parts: &[u32]) -> Result<HashMap<u32, String>, String>;

    fn make_worker(&self) -> Result<Box<dyn BlockingWorker>, String>;

    fn on_missing_url(&self, _part_number: u32) {}

    fn on_read_error(&self, _part_number: u32, _error: &str) {}

    fn on_upload_error(&self, _part_number: u32, _error: &str) {}
}

pub fn run_blocking_upload(
    adapter: Arc<dyn BlockingPlatformAdapter>,
    config: BlockingEngineConfig,
    file_key: &str,
    part_size: u64,
    file_size: u64,
    parts_to_upload: Vec<u32>,
) -> Result<Vec<ChunkUploadResult>, String> {
    let completed_parts = Arc::new(Mutex::new(Vec::<ChunkUploadResult>::new()));
    let url_pool: Arc<Mutex<HashMap<u32, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let parts_arc = Arc::new(parts_to_upload);
    let parts_len = parts_arc.len();

    match config.prefetch {
        BlockingPrefetch::AllUpfront => {
            url_pool
                .lock()
                .unwrap()
                .extend(adapter.fetch_urls(&parts_arc[..])?);
        }
        BlockingPrefetch::Rolling => {
            let initial = &parts_arc[..MAX_CONCURRENT_UPLOADS.min(parts_len)];
            if !initial.is_empty() {
                url_pool
                    .lock()
                    .unwrap()
                    .extend(adapter.fetch_urls(initial)?);
            }
        }
    }

    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(MAX_CONCURRENT_UPLOADS);
    let rx = Arc::new(Mutex::new(rx));
    let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error_abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let file_key_owned = file_key.to_string();

    let mut handles = vec![];
    for _ in 0..MAX_CONCURRENT_UPLOADS {
        let rx = rx.clone();
        let parts = parts_arc.clone();
        let url_pool = url_pool.clone();
        let completed = completed_parts.clone();
        let abort = abort.clone();
        let error_abort = error_abort.clone();
        let adapter = adapter.clone();
        let file_key_str = file_key_owned.clone();

        let handle = thread::spawn(move || {
            let worker = match adapter.make_worker() {
                Ok(worker) => worker,
                Err(error) => {
                    error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    adapter.on_upload_error(0, &error);
                    return;
                }
            };

            loop {
                let part_idx = match rx.lock().unwrap().recv() {
                    Ok(i) => i,
                    Err(_) => break,
                };

                if adapter.is_paused() || !adapter.is_current_run() {
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }

                let part_number = parts[part_idx];
                let url = match url_pool.lock().unwrap().get(&part_number).cloned() {
                    Some(url) => url,
                    None => {
                        adapter.on_missing_url(part_number);
                        if config.fail_fast {
                            abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        continue;
                    }
                };

                let chunk_info = ChunkInfo {
                    part_number,
                    start_pos: (part_number as u64 - 1) * part_size,
                    chunk_size: upload::part_size_for(part_number, part_size, file_size),
                    url: url.clone(),
                };

                let chunk = match worker.read_chunk(&chunk_info) {
                    Ok(data) => data,
                    Err(error) => {
                        adapter.on_read_error(part_number, &error);
                        if config.fail_fast {
                            abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            if !adapter.is_paused()
                                && !error.contains("paused")
                                && !error.contains("Interrupted")
                                && !error.contains("stale run")
                            {
                                error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            break;
                        }
                        continue;
                    }
                };

                let etag = match worker.upload_chunk(&url, &chunk, part_number, &file_key_str) {
                    Ok(etag) => etag,
                    Err(error) => {
                        adapter.on_upload_error(part_number, &error);
                        if config.fail_fast {
                            abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            if !adapter.is_paused()
                                && !error.contains("paused")
                                && !error.contains("Interrupted")
                                && !error.contains("stale run")
                            {
                                error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            break;
                        }
                        continue;
                    }
                };

                completed
                    .lock()
                    .unwrap()
                    .push(ChunkUploadResult { part_number, etag });
            }
        });
        handles.push(handle);
    }

    // Drop the main rx reference so that when all worker threads exit (after aborting),
    // the Receiver is disconnected and a blocked tx.send() returns Err rather than
    // hanging forever. Workers hold the remaining Arc clones and are unaffected.
    drop(rx);

    for (idx, _) in parts_arc.iter().enumerate() {
        if let BlockingPrefetch::Rolling = config.prefetch {
            if idx > 0 && idx % MAX_CONCURRENT_UPLOADS == 0 && idx < parts_len {
                let batch_end = (idx + MAX_CONCURRENT_UPLOADS).min(parts_len);
                let next: Vec<u32> = parts_arc[idx..batch_end].to_vec();
                if let Ok(new_urls) = adapter.fetch_urls(&next) {
                    url_pool.lock().unwrap().extend(new_urls);
                }
            }
        }

        if adapter.is_paused()
            || !adapter.is_current_run()
            || abort.load(std::sync::atomic::Ordering::Relaxed)
        {
            break;
        }
        tx.send(idx).ok();
    }
    drop(tx);

    // For pause/resume races we intentionally do not wait for old worker threads to finish.
    // On iOS, in-flight NSURLSession requests can take a while to terminate even after pause.
    // Waiting here would block the resumed run from starting and eventually calling complete.
    let should_return_early_for_pause = adapter.is_paused()
        || !adapter.is_current_run()
        || (abort.load(std::sync::atomic::Ordering::Relaxed)
            && !error_abort.load(std::sync::atomic::Ordering::Relaxed));
    if should_return_early_for_pause {
        return Err("Upload paused".to_string());
    }

    // A pause can arrive after the check above, while a network worker is stuck.
    // Do not let joining that worker block the queue from starting the resumed run.
    while handles.iter().any(|handle| !handle.is_finished()) {
        if adapter.is_paused()
            || !adapter.is_current_run()
            || (abort.load(std::sync::atomic::Ordering::Relaxed)
                && !error_abort.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err("Upload paused".to_string());
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }

    for handle in handles {
        handle.join().ok();
    }

    if adapter.is_paused()
        || !adapter.is_current_run()
        || (abort.load(std::sync::atomic::Ordering::Relaxed)
            && !error_abort.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err("Upload paused".to_string());
    }

    let results = completed_parts.lock().unwrap().clone();
    if results.len() != parts_arc.len() {
        return Err(format!(
            "Only {}/{} parts uploaded successfully",
            results.len(),
            parts_arc.len()
        ));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Condvar;
    use std::time::Duration;

    struct WorkerGate {
        released: Mutex<bool>,
        signal: Condvar,
        entered: Option<mpsc::Sender<()>>,
    }

    impl WorkerGate {
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.signal.notify_all();
        }
    }

    struct TestAdapter {
        gate: Arc<WorkerGate>,
        paused: AtomicBool,
        current: AtomicBool,
        engine_thread: Mutex<Option<thread::ThreadId>>,
        engine_checks: AtomicUsize,
        before_join: Option<mpsc::Sender<()>>,
    }

    impl BlockingPlatformAdapter for TestAdapter {
        fn is_paused(&self) -> bool {
            self.paused.load(Ordering::SeqCst)
        }

        fn is_current_run(&self) -> bool {
            let current = self.current.load(Ordering::SeqCst);
            if *self.engine_thread.lock().unwrap() == Some(thread::current().id())
                && self.engine_checks.fetch_add(1, Ordering::SeqCst) == 1
            {
                // The producer checked once; this is the final check before joining.
                self.before_join.as_ref().unwrap().send(()).unwrap();
            }
            current
        }

        fn fetch_urls(&self, parts: &[u32]) -> Result<HashMap<u32, String>, String> {
            Ok(parts
                .iter()
                .map(|part| (*part, "test://part".into()))
                .collect())
        }

        fn make_worker(&self) -> Result<Box<dyn BlockingWorker>, String> {
            Ok(Box::new(TestWorker(self.gate.clone())))
        }
    }

    struct TestWorker(Arc<WorkerGate>);

    impl BlockingWorker for TestWorker {
        fn read_chunk(&self, _: &ChunkInfo) -> Result<Vec<u8>, String> {
            Ok(vec![1])
        }

        fn upload_chunk(&self, _: &str, _: &[u8], _: u32, _: &str) -> Result<String, String> {
            if let Some(entered) = &self.0.entered {
                entered.send(()).unwrap();
            }
            let mut released = self.0.released.lock().unwrap();
            while !*released {
                released = self.0.signal.wait(released).unwrap();
            }
            Ok("etag".into())
        }
    }

    #[test]
    fn pause_or_replaced_run_during_join_does_not_block_resume() {
        for pause in [true, false] {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (before_join_tx, before_join_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let gate = Arc::new(WorkerGate {
                released: Mutex::new(false),
                signal: Condvar::new(),
                entered: Some(entered_tx),
            });
            let adapter = Arc::new(TestAdapter {
                gate: gate.clone(),
                paused: AtomicBool::new(false),
                current: AtomicBool::new(true),
                engine_thread: Mutex::new(None),
                engine_checks: AtomicUsize::new(0),
                before_join: Some(before_join_tx),
            });
            let config = BlockingEngineConfig {
                prefetch: BlockingPrefetch::Rolling,
                fail_fast: true,
            };
            let engine_adapter = adapter.clone();
            let engine = thread::spawn(move || {
                *engine_adapter.engine_thread.lock().unwrap() = Some(thread::current().id());
                result_tx
                    .send(run_blocking_upload(
                        engine_adapter,
                        config,
                        "file",
                        1,
                        1,
                        vec![1],
                    ))
                    .unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            before_join_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if pause {
                adapter.paused.store(true, Ordering::SeqCst);
            } else {
                adapter.current.store(false, Ordering::SeqCst);
            }
            let paused_result = result_rx.recv_timeout(Duration::from_secs(2));
            if paused_result.is_err() {
                gate.release();
                engine.join().unwrap();
                panic!("The paused upload remained blocked on its old network worker");
            }
            assert_eq!(paused_result.unwrap().unwrap_err(), "Upload paused");
            engine.join().unwrap();

            // A resumed run must finish while the old network request is still blocked.
            let resumed = Arc::new(TestAdapter {
                gate: Arc::new(WorkerGate {
                    released: Mutex::new(true),
                    signal: Condvar::new(),
                    entered: None,
                }),
                paused: AtomicBool::new(false),
                current: AtomicBool::new(true),
                engine_thread: Mutex::new(None),
                engine_checks: AtomicUsize::new(0),
                before_join: None,
            });
            let result = run_blocking_upload(resumed, config, "file", 1, 1, vec![1]);
            gate.release();
            assert_eq!(result.unwrap().len(), 1);
        }
    }
}
