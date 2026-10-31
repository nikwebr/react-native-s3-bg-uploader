//! Web-only custom encryptor: a customer mechanism implemented in JavaScript.
//!
//! Native platforms register their encryptor across a C ABI vtable (function
//! pointers inside the host binary). On web there is no second binary — the WASM
//! module and the customer's code share one worker — so the mechanism is
//! registered as a JavaScript *factory* instead.
//!
//! The factory mirrors the native vtable's `create` callback: the uploader calls
//! it once per upload with that upload's key and nonce and keeps the returned
//! instance for that file. Two uploads may therefore use different keys, exactly
//! as on iOS and Android.
//!
//! The engine's `ChunkEncryptor` trait is synchronous while browser crypto
//! (`crypto.subtle`) is Promise-based, so this encryptor is called from the async
//! part-read path (`AsyncPlatformAdapter::read_chunk_async`). A callback may
//! return a `Uint8Array`/`ArrayBuffer` either directly or as a Promise.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Promise, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// A customer encryptor instance, bound to one upload's key material.
pub struct JsEncryptor {
    /// Bytes the scheme adds to every part (tag/IV/...).
    pub overhead: u32,
    /// Whether the format ends with a descriptor appended to the last data part.
    pub has_footer: bool,
    /// Bytes that descriptor occupies inside the last data part.
    pub footer_len: u32,
    encrypt_chunk: Function,
    compute_footer: Option<Function>,
}

/// The registered factory plus the format numbers its instances use.
struct JsFactory {
    overhead: u32,
    has_footer: bool,
    footer_len: u32,
    create: Function,
}

thread_local! {
    /// The worker's registered factory. WASM is single-threaded, so a
    /// thread-local is the right home for a JS callback handle.
    static FACTORY: RefCell<Option<Rc<JsFactory>>> = RefCell::new(None);

    /// One instance per upload, keyed by the upload's file key (its content
    /// hash). Keeping it here is what lets a resumed upload keep encrypting with
    /// the key it started with, without the host persisting anything.
    static INSTANCES: RefCell<HashMap<String, Rc<JsEncryptor>>> = RefCell::new(HashMap::new());
}

/// Register the factory. Validation mirrors the native ABI: a declared
/// descriptor must be both callable and sized, and `create` must be callable.
pub fn register(
    overhead: u32,
    has_footer: bool,
    footer_len: u32,
    create: Function,
) -> Result<(), String> {
    if has_footer && footer_len == 0 {
        return Err("encryptor declares a descriptor but footer_len is 0".to_string());
    }
    FACTORY.with(|c| {
        *c.borrow_mut() = Some(Rc::new(JsFactory {
            overhead,
            has_footer,
            footer_len,
            create,
        }))
    });
    Ok(())
}

/// Whether a JS factory is registered.
pub fn factory_registered() -> bool {
    FACTORY.with(|c| c.borrow().is_some())
}

/// The instance for one upload, if the factory built one.
pub fn instance(file_key: &str) -> Option<Rc<JsEncryptor>> {
    INSTANCES.with(|m| m.borrow().get(file_key).cloned())
}

/// Drop the instance for one upload (finished, cancelled or removed).
pub fn forget_instance(file_key: &str) {
    INSTANCES.with(|m| {
        m.borrow_mut().remove(file_key);
    });
}

pub fn forget_all_instances() {
    INSTANCES.with(|m| m.borrow_mut().clear());
}

/// Ask the factory for the instance of one upload.
///
/// `Ok(true)` = a JS instance now handles this file; `Ok(false)` = the factory
/// returned `null`/`undefined`, so the caller should use the built-in cipher.
pub fn create_instance(file_key: &str, key: &[u8], nonce: &[u8]) -> Result<bool, String> {
    let factory = match FACTORY.with(|c| c.borrow().clone()) {
        Some(f) => f,
        None => {
            forget_instance(file_key);
            return Ok(false);
        }
    };

    let args = Array::new();
    args.push(&Uint8Array::from(key).into());
    args.push(&Uint8Array::from(nonce).into());
    let value = factory
        .create
        .apply(&JsValue::NULL, &args)
        .map_err(|e| format!("encryptor factory threw: {}", describe(&e)))?;

    if value.is_null() || value.is_undefined() {
        forget_instance(file_key);
        return Ok(false);
    }
    let obj: Object = value
        .dyn_into()
        .map_err(|_| "encryptor factory must return an object or null".to_string())?;

    let encrypt_chunk = get_fn(&obj, "encryptChunk")?
        .ok_or_else(|| "encryptor factory returned an object without encryptChunk".to_string())?;
    let compute_footer = get_fn(&obj, "computeFooter")?;
    if factory.has_footer && compute_footer.is_none() {
        return Err(
            "encryptor declares a descriptor but the instance has no computeFooter".to_string(),
        );
    }

    INSTANCES.with(|m| {
        m.borrow_mut().insert(
            file_key.to_string(),
            Rc::new(JsEncryptor {
                overhead: factory.overhead,
                has_footer: factory.has_footer,
                footer_len: factory.footer_len,
                encrypt_chunk,
                compute_footer,
            }),
        );
    });
    Ok(true)
}

fn get_fn(obj: &Object, name: &str) -> Result<Option<Function>, String> {
    match js_sys::Reflect::get(obj, &JsValue::from_str(name)) {
        Ok(v) if v.is_function() => Ok(Some(v.unchecked_into())),
        Ok(v) if v.is_null() || v.is_undefined() => Ok(None),
        Ok(_) => Err(format!("encryptor.{name} must be a function")),
        Err(e) => Err(format!("encryptor.{name} is unreadable: {}", describe(&e))),
    }
}

/// Describe a JS exception for a Rust `Err`, preferring the message a developer
/// would actually see in the console.
fn describe(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        if let Some(m) = err.message().as_string() {
            return m;
        }
    }
    format!("{:?}", e)
}

fn bytes_arg(data: &[u8]) -> JsValue {
    Uint8Array::from(data).into()
}

/// Call a JS callback and read back its bytes, awaiting a Promise if it returns one.
async fn call_bytes(f: &Function, args: &[JsValue]) -> Result<Vec<u8>, String> {
    let list = Array::new();
    for arg in args {
        list.push(arg);
    }
    let value = f
        .apply(&JsValue::NULL, &list)
        .map_err(|e| format!("encryptor callback threw: {}", describe(&e)))?;
    let value = match value.dyn_ref::<Promise>() {
        Some(promise) => JsFuture::from(promise.clone())
            .await
            .map_err(|e| format!("encryptor callback rejected: {}", describe(&e)))?,
        None => value,
    };
    if let Some(arr) = value.dyn_ref::<Uint8Array>() {
        return Ok(arr.to_vec());
    }
    if let Some(buf) = value.dyn_ref::<js_sys::ArrayBuffer>() {
        return Ok(Uint8Array::new(buf).to_vec());
    }
    Err(
        "encryptor callback must return a Uint8Array or ArrayBuffer (or a Promise of one)"
            .to_string(),
    )
}

impl JsEncryptor {
    /// Ciphertext of one part; `part_number` is 1-based and stable across resumes.
    pub async fn encrypt_chunk(&self, data: &[u8], part_number: u32) -> Result<Vec<u8>, String> {
        call_bytes(
            &self.encrypt_chunk,
            &[bytes_arg(data), JsValue::from(part_number)],
        )
        .await
    }

    /// The descriptor that rides inside the final data part.
    pub async fn compute_footer(
        &self,
        n_data_parts: u32,
        effective_part_size: u64,
    ) -> Result<Vec<u8>, String> {
        let f = self
            .compute_footer
            .as_ref()
            .ok_or_else(|| "no descriptor callback registered".to_string())?;
        call_bytes(
            f,
            &[
                JsValue::from(n_data_parts),
                JsValue::from(effective_part_size as f64),
            ],
        )
        .await
    }
}
