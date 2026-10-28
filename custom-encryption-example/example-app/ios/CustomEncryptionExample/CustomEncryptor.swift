// CustomEncryptor.swift
// iOS integration for custom AES-256-GCM encryption in S3BgUploader.
//
// Registers the AES-256-GCM chunk encryptor factory (from libcustom_encryptor.a)
// with the uploader. Call `CustomEncryptor.register()` once at app startup, before
// the first upload — see AppDelegate.application(_:didFinishLaunchingWithOptions:).
// No key is passed here: the key/nonce of each upload come from
// `encryptAndUploadFile(filePath, transferId, keyHex, nonceHex)`.

import Foundation

// ---------------------------------------------------------------------------
// FFI: exported by libcustom_encryptor.a (custom-encryptor-rust/src/lib.rs)
// ---------------------------------------------------------------------------
//
// `custom_encryptor_register` is a plain C symbol and is not declared in a header
// this target imports, so it is bound by name. It builds an EncryptorVTable
// (declaring the 16-byte AES-GCM overhead and a 28-byte descriptor) and hands it to
// the uploader's `register_custom_encryptor`.
@_silgen_name("custom_encryptor_register")
func custom_encryptor_register()

// ---------------------------------------------------------------------------
// Swift API
// ---------------------------------------------------------------------------

/// Register the custom AES-256-GCM encryptor factory with S3BgUploader.
///
/// Call this once at app startup, before any uploads. The key and nonce are not
/// arguments: every `encryptAndUploadFile(filePath, transferId, keyHex, nonceHex)`
/// call hands its own key material to the factory, so different uploads may use
/// different keys (and a resumed upload reuses the stored key automatically).
@objc(CustomEncryptor)
public class CustomEncryptor: NSObject {

    @objc(register)
    public static func register() {
        custom_encryptor_register()
    }
}
