use crate::core::session::{Session, SessionStore};

const IDB_NAME: &str = "s3_bg_uploader";
const IDB_STORE: &str = "session";

pub struct WasmSessionStore;

impl SessionStore for WasmSessionStore {
    fn load(&self) -> Option<Session> {
        None
    }

    fn save(&self, json: &str) {
        let json = json.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = idb_save(&json).await;
        });
    }

    fn clear(&self) {
        wasm_bindgen_futures::spawn_local(async {
            let _ = idb_clear().await;
        });
    }
}

pub async fn load() -> Option<Session> {
    let db = open_idb().await.ok()?;
    let tx = db
        .transaction(&[IDB_STORE], rexie::TransactionMode::ReadOnly)
        .ok()?;
    let store = tx.store(IDB_STORE).ok()?;
    let val = store
        .get(wasm_bindgen::JsValue::from_str("data"))
        .await
        .ok()??;
    Session::from_json(&val.as_string()?)
}

async fn open_idb() -> rexie::Result<rexie::Rexie> {
    rexie::Rexie::builder(IDB_NAME)
        .version(1)
        .add_object_store(rexie::ObjectStore::new(IDB_STORE).auto_increment(false))
        .build()
        .await
}

async fn idb_save(json: &str) -> rexie::Result<()> {
    let db = open_idb().await?;
    let tx = db.transaction(&[IDB_STORE], rexie::TransactionMode::ReadWrite)?;
    let store = tx.store(IDB_STORE)?;
    store
        .put(
            &wasm_bindgen::JsValue::from_str(json),
            Some(&wasm_bindgen::JsValue::from_str("data")),
        )
        .await?;
    tx.done().await?;
    Ok(())
}

async fn idb_clear() -> rexie::Result<()> {
    let db = open_idb().await?;
    let tx = db.transaction(&[IDB_STORE], rexie::TransactionMode::ReadWrite)?;
    let store = tx.store(IDB_STORE)?;
    store.clear().await?;
    tx.done().await?;
    Ok(())
}
