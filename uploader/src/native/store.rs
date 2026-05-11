use std::sync::{Mutex, OnceLock};

use crate::core::session::{Session, SessionStore};

static DB: OnceLock<Mutex<redb::Database>> = OnceLock::new();
static STORE_PATH: Mutex<String> = Mutex::new(String::new());
const SESSION_TABLE: redb::TableDefinition<&str, &str> = redb::TableDefinition::new("session");

fn db() -> std::sync::MutexGuard<'static, redb::Database> {
    DB.get_or_init(|| {
        let base = STORE_PATH.lock().unwrap().clone();
        let path = if base.is_empty() {
            "s3_uploader.redb".to_string()
        } else {
            format!("{}/s3_uploader.redb", base)
        };
        Mutex::new(redb::Database::create(&path).expect("redb open failed"))
    })
    .lock()
    .unwrap()
}

pub struct NativeSessionStore;

impl SessionStore for NativeSessionStore {
    fn load(&self) -> Option<Session> {
        let txn = db().begin_read().ok()?;
        let table = txn.open_table(SESSION_TABLE).ok()?;
        let guard = table.get("data").ok()??;
        Session::from_json(guard.value())
    }

    fn save(&self, json: &str) {
        if let Ok(txn) = db().begin_write() {
            if let Ok(mut table) = txn.open_table(SESSION_TABLE) {
                let _ = table.insert("data", json);
            }
            let _ = txn.commit();
        }
    }

    fn clear(&self) {
        if let Ok(txn) = db().begin_write() {
            if let Ok(mut table) = txn.open_table(SESSION_TABLE) {
                let _ = table.remove("data");
            }
            let _ = txn.commit();
        }
    }

    fn set_storage_path(&self, path: &str) {
        *STORE_PATH.lock().unwrap() = path.to_string();
    }
}
